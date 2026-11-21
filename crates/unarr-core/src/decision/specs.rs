//! The specifications of `NzbDrone.Core.DecisionEngine.Specifications`, each
//! answering `None` (accepted) or the rejection the original raises, with
//! its message. They are grouped by the originals' priority: the disk ones
//! only run when nothing else rejected.

use chrono::{DateTime, Duration, Utc};

use crate::decision::formats;
use crate::decision::lookup::{Catalog, Lookup, ProperDownloadTypes};
use crate::decision::upgradable::{self, UpgradeRejectReason};
use crate::decision::{
    megabytes, quality_model_string, revision_string, size_suffix, Rejection, RejectionReason as R,
    RemoteEpisode, SearchCriteria, SearchKind,
};
use crate::model::{Episode, EpisodeFile, HistoryEvent, Protocol, QualityProfile, Series, SeriesType};
use crate::parser::netregex::{Lazy, NetRegex};

/// `SpecificationPriority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Default = 0,
    Disk = 1,
}

pub struct Context<'a> {
    pub catalog: &'a Catalog,
    pub lookup: &'a dyn Lookup,
    pub now: DateTime<Utc>,
}

/// What every specification receives: a mapped release with a series.
pub struct Subject<'a> {
    pub remote: &'a RemoteEpisode,
    pub series: &'a Series,
    pub profile: &'a QualityProfile,
    pub search: Option<&'a SearchCriteria>,
}

impl Subject<'_> {
    fn parsed(&self) -> &crate::parser::title::ParsedEpisodeInfo {
        self.remote.parsed.as_ref().expect("a mapped release was parsed")
    }

    fn release(&self) -> &crate::indexers::rss::ReleaseInfo {
        &self.remote.release
    }

    /// The files of the release's episodes (`EpisodeFile.Value`).
    fn files(&self, ctx: &Context<'_>) -> Vec<EpisodeFile> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for e in &self.remote.episodes {
            if e.episode_file_id != 0 && !seen.contains(&e.episode_file_id) {
                seen.push(e.episode_file_id);
                match ctx.lookup.episode_file(e.episode_file_id) {
                    Some(f) => out.push(f),
                    None => tracing::debug!("file is no longer available, skipping this file"),
                }
            }
        }
        out
    }
}

pub type Spec = fn(&Context<'_>, &Subject<'_>) -> Option<Rejection>;

/// Every specification with its priority, in the order the originals
/// evaluate them.
pub const SPECIFICATIONS: &[(&str, Priority, Spec)] = &[
    ("AcceptableSize", Priority::Default, acceptable_size),
    ("AlreadyImported", Priority::Default, already_imported),
    ("AnimeVersionUpgrade", Priority::Default, anime_version_upgrade),
    ("BlockedIndexer", Priority::Default, blocked_indexer),
    ("Blocklist", Priority::Default, blocklist),
    (
        "CustomFormatAllowedByProfile",
        Priority::Default,
        custom_format_allowed_by_profile,
    ),
    ("FreeSpace", Priority::Disk, free_space),
    ("FullSeason", Priority::Default, full_season),
    ("MaximumSize", Priority::Default, maximum_size),
    ("MinimumAge", Priority::Default, minimum_age),
    ("MultiSeason", Priority::Default, multi_season),
    ("NotSample", Priority::Default, not_sample),
    ("Protocol", Priority::Default, protocol),
    (
        "QualityAllowedByProfile",
        Priority::Default,
        quality_allowed_by_profile,
    ),
    ("Queue", Priority::Default, queue),
    ("RawDisk", Priority::Default, raw_disk),
    ("ReleaseRestrictions", Priority::Default, release_restrictions),
    ("Repack", Priority::Default, repack),
    ("Retention", Priority::Default, retention),
    ("Delay", Priority::Default, delay),
    ("DeletedEpisodeFile", Priority::Disk, deleted_episode_file),
    ("History", Priority::Default, history),
    ("IndexerTag", Priority::Default, indexer_tag),
    ("MonitoredEpisode", Priority::Default, monitored_episode),
    ("Proper", Priority::Default, proper),
    ("SameEpisodesGrab", Priority::Default, same_episodes_grab),
    ("SceneMapping", Priority::Default, scene_mapping),
    ("EpisodeRequested", Priority::Default, episode_requested),
    ("SeasonMatch", Priority::Default, season_match),
    ("SeriesSpecification", Priority::Default, series_matches_search),
    (
        "SingleEpisodeSearchMatch",
        Priority::Default,
        single_episode_search_match,
    ),
    ("SeasonPackOnly", Priority::Default, season_pack_only),
    ("SplitEpisode", Priority::Default, split_episode),
    ("TorrentSeeding", Priority::Default, torrent_seeding),
    ("UpgradeAllowed", Priority::Default, upgrade_allowed),
    ("UpgradeDisk", Priority::Default, upgrade_disk),
];

fn permanent(reason: R, message: impl Into<String>) -> Option<Rejection> {
    Some(Rejection::permanent(reason, message))
}

fn temporary(reason: R, message: impl Into<String>) -> Option<Rejection> {
    Some(Rejection::temporary(reason, message))
}

fn age_minutes(ctx: &Context<'_>, s: &Subject<'_>) -> f64 {
    (ctx.now - s.release().publish_date).num_milliseconds() as f64 / 60_000.0
}

fn age_days(ctx: &Context<'_>, s: &Subject<'_>) -> i64 {
    (ctx.now - s.release().publish_date).num_days()
}

// --- general

pub fn acceptable_size(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let parsed = s.parsed();
    if parsed.special {
        return None;
    }
    let size = s.release().size;
    if size == 0 {
        return None;
    }
    let mut series_runtime = s.series.runtime;
    if series_runtime == 0 {
        let first_season = s
            .series
            .seasons
            .iter()
            .map(|x| x.season_number)
            .filter(|n| *n > 0)
            .min();
        let pilot =
            first_season.and_then(|n| ctx.lookup.episodes_by_season(s.series.id, n).into_iter().next());
        if let (Some(pilot), Some(first)) = (pilot, s.remote.episodes.first()) {
            if first.season_number == pilot.season_number {
                let grace_end = pilot.air_date_utc.unwrap_or(s.release().publish_date) + Duration::hours(24);
                if s.remote
                    .episodes
                    .iter()
                    .all(|e| e.air_date_utc.is_none_or(|d| d < grace_end))
                {
                    series_runtime = 45;
                }
            }
        }
    }
    let runtime: i64 = s
        .remote
        .episodes
        .iter()
        .map(|e| if e.runtime > 0 { e.runtime } else { series_runtime })
        .sum();
    if runtime == 0 {
        return permanent(
            R::UnknownRuntime,
            "Runtime of all episodes is 0, unable to validate size until it is available",
        );
    }
    let definition = ctx.catalog.definition(parsed.quality.quality.quality_id);
    let count = s.remote.episodes.len();
    let runtime_message = if count == 1 {
        format!("{runtime}min")
    } else {
        format!("{count}x {runtime}min")
    };
    if let Some(min) = definition.min_size {
        let min_size = (min * 1024.0 * 1024.0) as i64 * runtime;
        if size < min_size {
            return permanent(
                R::BelowMinimumSize,
                format!(
                    "{} is smaller than minimum allowed {} (for {runtime_message})",
                    size_suffix(size),
                    size_suffix(min_size)
                ),
            );
        }
    }
    if let Some(max) = definition.max_size.filter(|m| *m != 0.0) {
        let max_size = (max * 1024.0 * 1024.0) as i64 * runtime;
        if size > max_size {
            return permanent(
                R::AboveMaximumSize,
                format!(
                    "{} is larger than maximum allowed {} (for {runtime_message})",
                    size_suffix(size),
                    size_suffix(max_size)
                ),
            );
        }
    }
    None
}

pub fn already_imported(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if !ctx.catalog.config.enable_completed_download_handling {
        return None;
    }
    for episode in &s.remote.episodes {
        if episode.episode_file_id == 0 {
            continue;
        }
        let history = ctx.lookup.history_for_episode(episode.id);
        let Some(last_grabbed) = history.iter().find(|h| h.event_type == HistoryEvent::Grabbed) else {
            continue;
        };
        let Some(imported) = history.iter().find(|h| {
            h.event_type == HistoryEvent::DownloadFolderImported && h.download_id == last_grabbed.download_id
        }) else {
            continue;
        };
        if last_grabbed.quality.quality_id == imported.quality.quality_id
            && upgradable::compare_revision(&last_grabbed.quality, &imported.quality).is_eq()
        {
            continue;
        }
        let release = s.release();
        if release.protocol == Protocol::Torrent {
            if let Some(hash) = &release.info_hash {
                if Some(hash.to_uppercase()) == last_grabbed.download_id.as_deref().map(str::to_string) {
                    return permanent(
                        R::AlreadyImportedSameHash,
                        "Has same torrent hash as a grabbed and imported release",
                    );
                }
            }
        }
        if last_grabbed
            .source_title
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case(&release.title))
        {
            return permanent(
                R::AlreadyImportedSameName,
                "Has same release name as a grabbed and imported release",
            );
        }
    }
    None
}

pub fn anime_version_upgrade(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.series.series_type != SeriesType::Anime {
        return None;
    }
    if ctx.catalog.config.download_propers_and_repacks == ProperDownloadTypes::DoNotPrefer {
        return None;
    }
    let release_group = s.parsed().release_group.clone().unwrap_or_default();
    for file in s.files(ctx) {
        if upgradable::is_revision_upgrade(&file.quality, &s.parsed().quality.quality) {
            let file_group = file.release_group.clone().unwrap_or_default();
            if file_group.trim().is_empty() {
                return permanent(R::UnknownReleaseGroup, "Existing release group is unknown");
            }
            if release_group.trim().is_empty() {
                return permanent(R::UnknownReleaseGroup, "Release group is unknown");
            }
            if file_group != release_group {
                return permanent(
                    R::ReleaseGroupDoesNotMatch,
                    format!("{release_group} does not match existing release group {file_group}"),
                );
            }
        }
    }
    None
}

pub fn blocked_indexer(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let status = ctx.catalog.blocked_indexers.get(&s.release().indexer_id)?;
    let till = status
        .disabled_till
        .map(|t| t.format("%m/%d/%Y %H:%M:%S").to_string())
        .unwrap_or_default();
    temporary(
        R::IndexerDisabled,
        format!(
            "Indexer {} is blocked till {till} due to failures, cannot grab release.",
            s.release().indexer
        ),
    )
}

/// `BlocklistRepository.BlocklistedByTitle`: `SourceTitle.Contains(title)`,
/// a `LIKE` that ignores ASCII case and matches inside the text.
fn blocklisted_title(entry: &crate::model::BlocklistEntry, title: &str) -> bool {
    blocklisted_title_text(entry.source_title.as_deref(), title)
}

fn blocklisted_title_text(source_title: Option<&str>, title: &str) -> bool {
    source_title.is_some_and(|t| t.to_lowercase().contains(&title.to_lowercase()))
}

pub fn blocklist(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let release = s.release();
    let entries = ctx.lookup.blocklist_for_series(s.series.id);
    let same_indexer = |item: &crate::model::BlocklistEntry| {
        item.indexer
            .as_deref()
            .is_none_or(|i| i.trim().is_empty() || i.eq_ignore_ascii_case(&release.indexer))
    };
    let blocked = if release.protocol == Protocol::Torrent {
        let same_torrent = |item: &crate::model::BlocklistEntry| match release.info_hash.as_deref() {
            Some(hash) if !hash.trim().is_empty() => item
                .torrent_info_hash
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(hash)),
            _ => same_indexer(item),
        };
        match release.info_hash.as_deref().filter(|h| !h.trim().is_empty()) {
            Some(hash) => entries
                .iter()
                .filter(|b| {
                    b.torrent_info_hash
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(hash))
                })
                .any(same_torrent),
            None => entries
                .iter()
                .filter(|b| blocklisted_title(b, &release.title))
                .filter(|b| b.protocol == Protocol::Torrent)
                .any(same_torrent),
        }
    } else {
        let same_nzb = |item: &crate::model::BlocklistEntry| {
            if item.published_date == Some(release.publish_date) {
                return true;
            }
            let same_date = item.published_date.is_none_or(|d| {
                d - Duration::minutes(2) <= release.publish_date
                    && d + Duration::minutes(2) >= release.publish_date
            });
            let same_size = item
                .size
                .is_none_or(|sz| (sz - release.size).abs() <= megabytes(2));
            !same_indexer(item) && same_date && same_size
        };
        entries
            .iter()
            .filter(|b| blocklisted_title(b, &release.title))
            .filter(|b| b.protocol == Protocol::Usenet)
            .any(same_nzb)
    };
    if blocked {
        return permanent(R::Blocklisted, "Release is blocklisted");
    }
    None
}

pub fn custom_format_allowed_by_profile(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let min = s.profile.min_format_score;
    let score = s.remote.custom_format_score;
    if score < min {
        return permanent(
            R::CustomFormatMinimumScore,
            format!(
                "Custom Formats {} have score {score} below Series profile minimum {min}",
                formats::names(&s.remote.custom_formats)
            ),
        );
    }
    None
}

pub fn free_space(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if ctx.catalog.config.skip_free_space_check_when_importing {
        return None;
    }
    let free = ctx.lookup.free_space(&s.series.path)?;
    let minimum = megabytes(ctx.catalog.config.minimum_free_space_when_importing_mb);
    let remaining = free - s.release().size;
    if remaining <= 0 {
        return permanent(
            R::MinimumFreeSpace,
            "Importing after download will exceed available disk space",
        );
    }
    if remaining < minimum {
        return permanent(
            R::MinimumFreeSpace,
            format!(
                "Not enough free space ({}) to import after download: {}. (Settings: Media Management: Minimum Free Space)",
                size_suffix(minimum),
                size_suffix(remaining)
            ),
        );
    }
    None
}

pub fn full_season(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.parsed().full_season
        && s.remote
            .episodes
            .iter()
            .any(|e| e.air_date_utc.is_none_or(|d| d > ctx.now + Duration::hours(24)))
    {
        return permanent(
            R::FullSeasonNotAired,
            "Full season release rejected. All episodes haven't aired yet.",
        );
    }
    None
}

pub fn maximum_size(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let size = s.release().size;
    let maximum = megabytes(ctx.catalog.config.indexer.maximum_size);
    if maximum == 0 || size == 0 {
        return None;
    }
    if size > maximum {
        return permanent(
            R::MaximumSizeExceeded,
            format!(
                "{} is too big, maximum size is {} (Settings->Indexers->Maximum Size)",
                size_suffix(size),
                size_suffix(maximum)
            ),
        );
    }
    None
}

pub fn minimum_age(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.release().protocol != Protocol::Usenet {
        return None;
    }
    let minimum = ctx.catalog.config.indexer.minimum_age;
    if minimum == 0 {
        return None;
    }
    let age = age_minutes(ctx, s);
    if age < minimum as f64 {
        let rounded = (age * 10.0).round() / 10.0;
        return temporary(
            R::MinimumAge,
            format!(
                "Only {} minutes old, minimum age is {minimum} minutes",
                dotnet_double(rounded)
            ),
        );
    }
    None
}

/// A double as .NET prints it: no trailing `.0`.
fn dotnet_double(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

pub fn multi_season(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.parsed().is_multi_season {
        return permanent(R::MultiSeason, "Multi-season releases are not supported");
    }
    None
}

pub fn not_sample(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let release = s.release();
    if release.title.to_lowercase().contains("sample") && release.size < megabytes(70) {
        return permanent(R::Sample, "Sample");
    }
    None
}

pub fn protocol(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let delay = ctx.catalog.delay_profile_for(&s.series.tags)?;
    let protocol = s.release().protocol;
    if protocol == Protocol::Usenet && !delay.enable_usenet {
        return permanent(R::ProtocolDisabled, "Usenet is not enabled for this series");
    }
    if protocol == Protocol::Torrent && !delay.enable_torrent {
        return permanent(R::ProtocolDisabled, "Torrent is not enabled for this series");
    }
    None
}

pub fn quality_allowed_by_profile(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let quality = s.parsed().quality.quality.quality_id;
    let index = crate::profile::index_of(s.profile, quality);
    let allowed = s.profile.items.get(index).is_some_and(|i| i.allowed);
    if !allowed {
        return permanent(
            R::QualityNotWanted,
            format!(
                "{} is not wanted in profile",
                crate::quality::Quality::by_id(quality).name
            ),
        );
    }
    None
}

pub fn queue(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let profile = s.profile;
    let config = &ctx.catalog.config;
    let episode_ids: Vec<i64> = s.remote.episodes.iter().map(|e| e.id).collect();
    for queued in ctx
        .catalog
        .queue
        .iter()
        .filter(|q| q.series_id == s.series.id && q.episode_ids.iter().any(|id| episode_ids.contains(id)))
    {
        if queued.failed_pending {
            continue;
        }
        let queued_formats = formats::queued_formats(ctx.catalog, queued, s.series);
        let new_quality = &s.parsed().quality.quality;
        if !upgradable::cutoff_not_met(profile, &queued.quality, &queued_formats, Some(new_quality)) {
            return permanent(
                R::QueueCutoffMet,
                format!(
                    "Release in queue already meets cutoff: {}",
                    quality_model_string(&queued.quality)
                ),
            );
        }
        let reason = upgradable::is_upgradable(
            config.download_propers_and_repacks,
            profile,
            &queued.quality,
            &queued_formats,
            new_quality,
            &s.remote.custom_formats,
        );
        match reason {
            UpgradeRejectReason::None => {}
            UpgradeRejectReason::BetterQuality => {
                return permanent(
                    R::QueueHigherPreference,
                    format!(
                        "Release in queue is of equal or higher preference: {}",
                        quality_model_string(&queued.quality)
                    ),
                )
            }
            UpgradeRejectReason::BetterRevision => {
                return permanent(
                    R::QueueHigherRevision,
                    format!(
                        "Release in queue is of equal or higher revision: {}",
                        revision_string(&queued.quality)
                    ),
                )
            }
            UpgradeRejectReason::QualityCutoff => {
                return permanent(
                    R::QueueCutoffMet,
                    format!(
                        "Release in queue meets quality cutoff: {}",
                        upgradable::profile_cutoff_string(profile)
                    ),
                )
            }
            UpgradeRejectReason::CustomFormatCutoff => {
                return permanent(
                    R::QueueCustomFormatCutoffMet,
                    format!(
                        "Release in queue meets Custom Format cutoff: {}",
                        profile.cutoff_format_score
                    ),
                )
            }
            UpgradeRejectReason::CustomFormatScore => {
                return permanent(
                    R::QueueCustomFormatScore,
                    format!(
                        "Release in queue has an equal or higher Custom Format score: {}",
                        upgradable::format_score(profile, &queued_formats)
                    ),
                )
            }
            UpgradeRejectReason::MinCustomFormatScore => {
                return permanent(
                    R::QueueCustomFormatScoreIncrement,
                    format!(
                        "Release in queue has Custom Format score within Custom Format score increment: {}",
                        profile.min_upgrade_format_score
                    ),
                )
            }
            UpgradeRejectReason::UpgradesNotAllowed => {
                return permanent(
                    R::QueueUpgradesNotAllowed,
                    format!(
                        "Release in queue and Quality Profile '{}' does not allow upgrades",
                        profile.name
                    ),
                )
            }
        }
        if upgradable::is_revision_upgrade(&queued.quality, new_quality)
            && config.download_propers_and_repacks == ProperDownloadTypes::DoNotUpgrade
        {
            return permanent(R::QueuePropersDisabled, "Proper downloading is disabled");
        }
    }
    None
}

static DISC_PATTERNS: [Lazy; 3] = [
    crate::net_regex!(
        r"(?:dis[ck])(?:[-_. ]\d+[-_. ])(?:(?:(?:480|720|1080|2160)[ip]|)[-_. ])?(?:Blu\-?ray)"
    ),
    crate::net_regex!(r"(?:(?:480|720|1080|2160)[ip]|)[-_. ](?:full)[-_. ](?:Blu\-?ray)"),
    crate::net_regex!(r"(?:\d?x?M?DVD-?[R59])(?:[ ._]|$)"),
];

pub fn raw_disk(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let release = s.release();
    if DISC_PATTERNS.iter().any(|re| re.is_match(&release.title)) {
        return permanent(R::Raw, "Raw Bluray/DVD release");
    }
    let container = release.container.as_deref().filter(|c| !c.trim().is_empty())?;
    match container.to_lowercase().as_str() {
        "vob" | "iso" => permanent(R::Raw, "Raw DVD release"),
        "m2ts" => permanent(R::Raw, "Raw Bluray release"),
        _ => None,
    }
}

/// `TermMatcherService.IsMatch`: `/pattern/flags` is a regex, anything else
/// a case-insensitive substring.
pub fn term_matches(term: &str, title: &str) -> bool {
    if let Some((pattern, modifiers)) = perl_regex(term) {
        let mut flags = String::new();
        for m in modifiers.chars() {
            match m {
                'i' => flags.push('i'),
                'm' => flags.push('m'),
                's' => flags.push('s'),
                'x' => flags.push('x'),
                'n' => {}
                _ => return false,
            }
        }
        let source = if flags.is_empty() {
            pattern.to_string()
        } else {
            format!("(?{flags}){pattern}")
        };
        return NetRegex::once(&source, false)
            .map(|re| re.is_match(title))
            .unwrap_or(false);
    }
    title.to_lowercase().contains(&term.to_lowercase())
}

/// `/(?<pattern>.*)/(?<modifiers>[a-z]*)`, matched anywhere in the term.
fn perl_regex(term: &str) -> Option<(&str, &str)> {
    let start = term.find('/')?;
    let rest = &term[start + 1..];
    let end = rest.rfind('/')?;
    let pattern = &rest[..end];
    let modifiers = &rest[end + 1..];
    if !modifiers.chars().all(|c| c.is_ascii_lowercase()) {
        // The originals' regex requires `[a-z]*` to the end of the string;
        // other trailing text means no regex.
        return None;
    }
    Some((pattern, modifiers))
}

pub fn release_restrictions(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let title = &s.release().title;
    let profiles = ctx
        .catalog
        .release_profiles_for(&s.series.tags, s.release().indexer_id);
    for r in profiles.iter().filter(|r| !r.required.is_empty()) {
        if !r.required.iter().any(|t| term_matches(t, title)) {
            return permanent(
                R::MustContainMissing,
                format!(
                    "Does not contain one of the required terms: {}",
                    r.required.join(", ")
                ),
            );
        }
    }
    for r in profiles.iter().filter(|r| !r.ignored.is_empty()) {
        let found: Vec<&str> = r
            .ignored
            .iter()
            .filter(|t| term_matches(t, title))
            .map(String::as_str)
            .collect();
        if !found.is_empty() {
            return permanent(
                R::MustNotContainPresent,
                format!("Contains these ignored terms: {}", found.join(", ")),
            );
        }
    }
    None
}

pub fn repack(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let quality = &s.parsed().quality.quality;
    if !quality.is_repack {
        return None;
    }
    let propers = ctx.catalog.config.download_propers_and_repacks;
    if propers == ProperDownloadTypes::DoNotPrefer {
        return None;
    }
    for file in s.files(ctx) {
        if upgradable::is_revision_upgrade(&file.quality, quality) {
            if propers == ProperDownloadTypes::DoNotUpgrade {
                return permanent(R::RepackDisabled, "Repack downloading is disabled");
            }
            let release_group = s.parsed().release_group.clone().unwrap_or_default();
            let file_group = file.release_group.clone().unwrap_or_default();
            if file_group.trim().is_empty() {
                return permanent(
                    R::RepackUnknownReleaseGroup,
                    "Unable to determine release group for the existing file",
                );
            }
            if release_group.trim().is_empty() {
                return permanent(
                    R::RepackUnknownReleaseGroup,
                    "Unable to determine release group for this release",
                );
            }
            if !file_group.eq_ignore_ascii_case(&release_group) {
                return permanent(
                    R::RepackReleaseGroupDoesNotMatch,
                    format!(
                        "Release is a repack for a different release group. Release Group: {release_group}. File release group: {file_group}"
                    ),
                );
            }
        }
    }
    None
}

pub fn retention(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.release().protocol != Protocol::Usenet {
        return None;
    }
    let retention = ctx.catalog.config.indexer.retention;
    if retention > 0 && age_days(ctx, s) > retention {
        return permanent(R::MaximumAge, "Older than configured retention");
    }
    None
}

// --- RSS sync

pub fn delay(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.search.is_some_and(|c| c.user_invoked_search) {
        return None;
    }
    let delay_profile = ctx.catalog.delay_profile_for(&s.series.tags)?;
    let protocol = s.release().protocol;
    let delay = match protocol {
        Protocol::Usenet => delay_profile.usenet_delay,
        Protocol::Torrent => delay_profile.torrent_delay,
        Protocol::Unknown => 0,
    };
    let preferred = protocol == delay_profile.preferred_protocol;
    if delay == 0 {
        return None;
    }
    let new_quality = &s.parsed().quality.quality;
    if preferred {
        for file in s.files(ctx) {
            if upgradable::compare_quality(s.profile, new_quality.quality_id, file.quality.quality_id).is_eq()
                && upgradable::compare_revision(new_quality, &file.quality).is_gt()
            {
                return None;
            }
        }
    }
    if delay_profile.bypass_if_highest_quality {
        let best = s
            .profile
            .items
            .iter()
            .rev()
            .find(|i| i.allowed)
            .and_then(|i| crate::profile::qualities_of(i).first().copied());
        if let Some(best) = best {
            let is_best = upgradable::compare_quality(s.profile, new_quality.quality_id, best).is_ge();
            if is_best && preferred {
                return None;
            }
        }
    }
    if delay_profile.bypass_if_above_custom_format_score
        && s.remote.custom_format_score >= delay_profile.minimum_custom_format_score
        && preferred
    {
        return None;
    }
    let episode_ids: Vec<i64> = s.remote.episodes.iter().map(|e| e.id).collect();
    if let Some(oldest) = ctx
        .lookup
        .oldest_pending_release_age_minutes(s.series.id, &episode_ids, ctx.now)
    {
        if oldest > delay as f64 {
            return None;
        }
    }
    if age_minutes(ctx, s) < delay as f64 {
        return temporary(R::MinimumAgeDelay, "Waiting for better quality release");
    }
    None
}

pub fn deleted_episode_file(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if !ctx.catalog.config.auto_unmonitor_previously_downloaded_episodes {
        return None;
    }
    if s.search.is_some() {
        return None;
    }
    let missing = s.files(ctx).into_iter().any(|f| {
        let full = format!("{}/{}", s.series.path.trim_end_matches('/'), f.relative_path);
        !ctx.lookup.file_exists(&full)
    });
    if missing {
        return temporary(R::SeriesNotMonitored, "Series is not monitored");
    }
    None
}

pub fn history(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.search.is_some() {
        return None;
    }
    let config = &ctx.catalog.config;
    let profile = s.profile;
    for episode in &s.remote.episodes {
        let Some(most_recent) = ctx.lookup.history_for_episode(episode.id).into_iter().next() else {
            continue;
        };
        if most_recent.event_type != HistoryEvent::Grabbed {
            continue;
        }
        let recent = most_recent.date > ctx.now - Duration::hours(12);
        if !recent && config.enable_completed_download_handling {
            continue;
        }
        let history_formats = formats::history_formats(ctx.catalog, &most_recent, s.series);
        let new_quality = &s.parsed().quality.quality;
        let cutoff_unmet =
            upgradable::cutoff_not_met(profile, &most_recent.quality, &history_formats, Some(new_quality));
        let reason = upgradable::is_upgradable(
            config.download_propers_and_repacks,
            profile,
            &most_recent.quality,
            &history_formats,
            new_quality,
            &s.remote.custom_formats,
        );
        if !cutoff_unmet {
            if recent {
                return permanent(
                    R::HistoryRecentCutoffMet,
                    format!(
                        "Recent grab event in history already meets cutoff: {}",
                        quality_model_string(&most_recent.quality)
                    ),
                );
            }
            return permanent(
                R::HistoryCdhDisabledCutoffMet,
                format!(
                    "CDH is disabled and grab event in history already meets cutoff: {}",
                    quality_model_string(&most_recent.quality)
                ),
            );
        }
        let subject = if recent { "Recent" } else { "CDH is disabled and" };
        match reason {
            UpgradeRejectReason::None => continue,
            UpgradeRejectReason::BetterQuality => {
                return permanent(
                    R::HistoryHigherPreference,
                    format!(
                        "{subject} grab event in history is of equal or higher preference: {}",
                        quality_model_string(&most_recent.quality)
                    ),
                )
            }
            UpgradeRejectReason::BetterRevision => {
                return permanent(
                    R::HistoryHigherRevision,
                    format!(
                        "{subject} grab event in history is of equal or higher revision: {}",
                        revision_string(&most_recent.quality)
                    ),
                )
            }
            UpgradeRejectReason::QualityCutoff => {
                return permanent(
                    R::HistoryCutoffMet,
                    format!(
                        "{subject} grab event in history meets quality cutoff: {}",
                        upgradable::profile_cutoff_string(profile)
                    ),
                )
            }
            UpgradeRejectReason::CustomFormatCutoff => {
                return permanent(
                    R::HistoryCustomFormatCutoffMet,
                    format!(
                        "{subject} grab event in history meets Custom Format cutoff: {}",
                        profile.cutoff_format_score
                    ),
                )
            }
            UpgradeRejectReason::CustomFormatScore => {
                return permanent(
                    R::HistoryCustomFormatScore,
                    format!(
                        "{subject} grab event in history has an equal or higher Custom Format score: {}",
                        upgradable::format_score(profile, &history_formats)
                    ),
                )
            }
            UpgradeRejectReason::MinCustomFormatScore => {
                return permanent(
                    R::HistoryCustomFormatScoreIncrement,
                    format!(
                        "{subject} grab event in history has Custom Format score within Custom Format score increment: {}",
                        profile.min_upgrade_format_score
                    ),
                )
            }
            UpgradeRejectReason::UpgradesNotAllowed => {
                return permanent(
                    R::HistoryUpgradesNotAllowed,
                    format!(
                        "{subject} grab event in history and Quality Profile '{}' does not allow upgrades",
                        profile.name
                    ),
                )
            }
        }
    }
    None
}

pub fn indexer_tag(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let indexer_id = s.release().indexer_id;
    if indexer_id == 0 {
        return None;
    }
    let Some(indexer) = ctx.catalog.indexer(indexer_id) else {
        tracing::debug!(indexer_id, "indexer does not exist, skipping indexer tags check");
        return None;
    };
    if !indexer.tags.is_empty() && !indexer.tags.iter().any(|t| s.series.tags.contains(t)) {
        return permanent(
            R::NoMatchingTag,
            "Series tags do not match any of the indexer tags",
        );
    }
    None
}

pub fn monitored_episode(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if let Some(search) = s.search {
        if !search.monitored_episodes_only {
            return None;
        }
    }
    if !s.series.monitored {
        return permanent(R::SeriesNotMonitored, "Series is not monitored");
    }
    let monitored = s.remote.episodes.iter().filter(|e| e.monitored).count();
    if monitored == s.remote.episodes.len() {
        return None;
    }
    if s.remote.episodes.len() == 1 {
        return permanent(R::EpisodeNotMonitored, "Episode is not monitored");
    }
    permanent(R::EpisodeNotMonitored, "One or more episodes is not monitored")
}

pub fn proper(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.search.is_some() {
        return None;
    }
    let propers = ctx.catalog.config.download_propers_and_repacks;
    if propers == ProperDownloadTypes::DoNotPrefer {
        return None;
    }
    let today = ctx.now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    for file in s.files(ctx) {
        if upgradable::is_revision_upgrade(&file.quality, &s.parsed().quality.quality) {
            if propers == ProperDownloadTypes::DoNotUpgrade {
                return permanent(R::PropersDisabled, "Proper downloading is disabled");
            }
            if file.date_added < today - Duration::days(7) {
                return permanent(R::ProperForOldFile, "Proper for old file");
            }
        }
    }
    None
}

/// `SameEpisodesSpecification.IsSatisfiedBy(episodes)`: no file of these
/// episodes holds an episode outside the set.
pub fn same_episodes(lookup: &dyn Lookup, episodes: &[Episode]) -> bool {
    let ids: Vec<i64> = episodes.iter().map(|e| e.id).collect();
    let mut file_ids: Vec<i64> = episodes
        .iter()
        .filter(|e| e.episode_file_id != 0)
        .map(|e| e.episode_file_id)
        .collect();
    file_ids.sort_unstable();
    file_ids.dedup();
    for file_id in file_ids {
        if lookup
            .episodes_by_file(file_id)
            .iter()
            .any(|e| !ids.contains(&e.id))
        {
            return false;
        }
    }
    true
}

pub fn same_episodes_grab(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if same_episodes(ctx.lookup, &s.remote.episodes) {
        return None;
    }
    permanent(
        R::ExistingFileHasMoreEpisodes,
        "Episode file on disk contains more episodes than this release contains",
    )
}

pub fn scene_mapping(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let mapping = s.remote.scene_mapping.as_ref()?;
    let origin = mapping.scene_origin.as_deref().filter(|o| !o.trim().is_empty())?;
    let kind = origin.split(':').next().unwrap_or("");
    if kind == "mixed" {
        // The originals pass the format string through unformatted.
        if mapping.comment.as_deref().is_some_and(|c| !c.trim().is_empty()) {
            return temporary(R::AmbiguousNumbering, "{0} has ambiguous numbering");
        }
        return temporary(R::AmbiguousNumbering, "Ambiguous numbering");
    }
    None
}

// --- search

pub fn episode_requested(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let search = s.search?;
    let requested: Vec<i64> = search.episodes.iter().map(|e| e.id).collect();
    if s.remote.episodes.iter().any(|e| requested.contains(&e.id)) {
        return None;
    }
    if s.remote.episodes.is_empty() {
        return permanent(R::WrongEpisode, "Episode wasn't requested");
    }
    let mut episodes: Vec<&Episode> = s.remote.episodes.iter().collect();
    episodes.sort_by_key(|e| (e.season_number, e.episode_number));
    let first = episodes[0];
    if episodes.len() > 1 {
        let last = episodes[episodes.len() - 1];
        return permanent(
            R::WrongEpisode,
            format!(
                "Episode wasn't requested: {}x{}-{}",
                first.season_number, first.episode_number, last.episode_number
            ),
        );
    }
    permanent(
        R::WrongEpisode,
        format!(
            "Episode wasn't requested: {}x{}",
            first.season_number, first.episode_number
        ),
    )
}

pub fn season_match(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let search = s.search?;
    if let SearchKind::Season { season_number } = search.kind {
        if season_number != s.parsed().season_number {
            return permanent(R::WrongSeason, "Wrong season");
        }
    }
    None
}

pub fn series_matches_search(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let search = s.search?;
    if s.series.id != search.series.id {
        return permanent(R::WrongSeries, "Wrong series");
    }
    None
}

pub fn single_episode_search_match(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let search = s.search?;
    let parsed = s.parsed();
    match &search.kind {
        SearchKind::SingleEpisode {
            season_number,
            episode_number,
        } => {
            if *season_number != parsed.season_number {
                return permanent(R::WrongSeason, "Wrong season");
            }
            if parsed.episode_numbers.is_empty() {
                return permanent(R::FullSeason, "Full season pack");
            }
            if !parsed.episode_numbers.contains(episode_number) {
                return permanent(R::WrongEpisode, "Wrong episode");
            }
            None
        }
        SearchKind::AnimeEpisode { is_season_search, .. } => {
            if parsed.full_season && !is_season_search {
                return permanent(R::FullSeason, "Full season pack");
            }
            None
        }
        _ => None,
    }
}

pub fn season_pack_only(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let search = s.search?;
    if search.episodes.len() == 1 {
        return None;
    }
    let max_age = s.release().season_search_maximum_single_episode_age;
    if max_age > 0
        && s.series.series_type == SeriesType::Standard
        && !s.parsed().full_season
        && !s.remote.episodes.is_empty()
    {
        let season = s.remote.episodes[0].season_number;
        let newest = search
            .episodes
            .iter()
            .filter(|e| e.season_number == season)
            .filter_map(|e| e.air_date_utc)
            .max();
        if let Some(newest) = newest {
            if newest < ctx.now - Duration::days(max_age) {
                return permanent(
                    R::NotSeasonPack,
                    format!("Last episode in this season aired more than {max_age} days ago, season pack required."),
                );
            }
        }
    }
    None
}

pub fn split_episode(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    if s.parsed().is_split_episode {
        return permanent(R::SplitEpisode, "Split episode releases are not supported");
    }
    None
}

pub fn torrent_seeding(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let release = s.release();
    if release.protocol != Protocol::Torrent || release.indexer_id == 0 {
        return None;
    }
    let Some(indexer) = ctx.catalog.indexer(release.indexer_id) else {
        tracing::debug!(
            indexer_id = release.indexer_id,
            "indexer does not exist, skipping seeders check"
        );
        return None;
    };
    let minimum = indexer.minimum_seeders?;
    if let Some(seeders) = release.seeders {
        if seeders < minimum {
            return permanent(
                R::MinimumSeeders,
                format!("Not enough seeders: {seeders}. Minimum seeders: {minimum}"),
            );
        }
    }
    None
}

pub fn upgrade_allowed(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    for file in s.files(ctx) {
        let file_formats = formats::file_formats(ctx.catalog, &file, s.series);
        if !upgradable::is_upgrade_allowed(
            s.profile,
            &file.quality,
            &file_formats,
            &s.parsed().quality.quality,
            &s.remote.custom_formats,
        ) {
            return permanent(
                R::QualityUpgradesDisabled,
                "Existing file and the Quality profile does not allow upgrades",
            );
        }
    }
    None
}

pub fn upgrade_disk(ctx: &Context<'_>, s: &Subject<'_>) -> Option<Rejection> {
    let profile = s.profile;
    let new_quality = &s.parsed().quality.quality;
    for file in s.files(ctx) {
        let file_formats = formats::file_formats(ctx.catalog, &file, s.series);
        if !upgradable::cutoff_not_met(profile, &file.quality, &file_formats, Some(new_quality)) {
            return permanent(
                R::DiskCutoffMet,
                format!(
                    "Existing file meets cutoff: {}",
                    upgradable::cutoff_item_string(profile)
                ),
            );
        }
        let reason = upgradable::is_upgradable(
            ctx.catalog.config.download_propers_and_repacks,
            profile,
            &file.quality,
            &file_formats,
            new_quality,
            &s.remote.custom_formats,
        );
        match reason {
            UpgradeRejectReason::None => continue,
            UpgradeRejectReason::BetterQuality => {
                return permanent(
                    R::DiskHigherPreference,
                    format!(
                        "Existing file on disk is of equal or higher preference: {}",
                        quality_model_string(&file.quality)
                    ),
                )
            }
            UpgradeRejectReason::BetterRevision => {
                return permanent(
                    R::DiskHigherRevision,
                    format!(
                        "Existing file on disk is of equal or higher revision: {}",
                        revision_string(&file.quality)
                    ),
                )
            }
            UpgradeRejectReason::QualityCutoff => {
                return permanent(
                    R::DiskCutoffMet,
                    format!(
                        "Existing file on disk meets quality cutoff: {}",
                        upgradable::profile_cutoff_string(profile)
                    ),
                )
            }
            UpgradeRejectReason::CustomFormatCutoff => {
                return permanent(
                    R::DiskCustomFormatCutoffMet,
                    format!(
                        "Existing file on disk meets Custom Format cutoff: {}",
                        profile.cutoff_format_score
                    ),
                )
            }
            UpgradeRejectReason::CustomFormatScore => {
                return permanent(
                    R::DiskCustomFormatScore,
                    format!(
                        "Existing file on disk has a equal or higher Custom Format score: {}",
                        upgradable::format_score(profile, &file_formats)
                    ),
                )
            }
            UpgradeRejectReason::MinCustomFormatScore => {
                return permanent(
                    R::DiskCustomFormatScoreIncrement,
                    format!(
                    "Existing file on disk has Custom Format score within Custom Format score increment: {}",
                    profile.min_upgrade_format_score
                ),
                )
            }
            UpgradeRejectReason::UpgradesNotAllowed => {
                return permanent(
                    R::DiskUpgradesNotAllowed,
                    format!(
                        "Existing file on disk and Quality Profile '{}' does not allow upgrades",
                        profile.name
                    ),
                )
            }
        }
    }
    None
}

/// `ReleaseInfo.AgeHours`, for the comparer.
pub fn release_age_hours(now: DateTime<Utc>, release: &crate::indexers::rss::ReleaseInfo) -> f64 {
    (now - release.publish_date).num_milliseconds() as f64 / 3_600_000.0
}

#[cfg(test)]
mod blocklist_title_tests {
    use super::blocklisted_title_text;

    #[test]
    fn matches_like_the_originals_sql_like() {
        assert!(blocklisted_title_text(
            Some("Show.S01E01.720p-GRP"),
            "Show.S01E01.720p-grp"
        ));
        assert!(blocklisted_title_text(
            Some("Show.S01E01.720p-GRP.mkv"),
            "Show.S01E01.720p-GRP"
        ));
        assert!(!blocklisted_title_text(
            Some("Show.S01E02.720p-GRP"),
            "Show.S01E01.720p-GRP"
        ));
        assert!(!blocklisted_title_text(None, "Show.S01E01.720p-GRP"));
    }
}
