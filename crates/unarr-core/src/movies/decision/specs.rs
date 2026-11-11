//! Radarr's `DecisionEngine.Specifications`, each answering `None`
//! (accepted) or the rejection the original raises, with its message. The
//! disk ones only run when nothing else rejected.

use chrono::{DateTime, Duration, Utc};

use crate::decision::lookup::ProperDownloadTypes;
use crate::decision::specs::term_matches;
use crate::decision::upgradable::{self, UpgradeRejectReason};
use crate::decision::{megabytes, revision_string, size_suffix, ReleaseSource};
use crate::indexers::rss::ReleaseInfo;
use crate::model::{CustomFormat, Protocol, QualityModel, QualityProfile};
use crate::movies::availability::is_available;
use crate::movies::decision::engine::{file_formats, history_formats, remote_formats, MovieCatalog};
use crate::movies::decision::library::MovieLookup;
use crate::movies::decision::{MovieRejection, MovieRejectionReason as R, MovieSearch, RemoteMovie};
use crate::movies::model::{Movie, MovieBlocklistEntry, MovieFile, MovieHistoryEvent, MovieStatus};
use crate::movies::parser::ParsedMovieInfo;
use crate::movies::quality::{indexer_flag_names, MovieLanguage, MovieQuality};
use crate::net_regex;
use crate::parser::netregex::Lazy;

/// `SpecificationPriority`: Default, Parsing and Database are one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Default = 0,
    Disk = 1,
}

pub struct Context<'a> {
    pub catalog: &'a MovieCatalog,
    pub lookup: &'a dyn MovieLookup,
    pub now: DateTime<Utc>,
}

/// What every specification receives: a parsed release mapped to a movie.
pub struct Subject<'a> {
    pub remote: &'a RemoteMovie,
    pub movie: &'a Movie,
    pub profile: &'a QualityProfile,
    pub search: Option<&'a MovieSearch>,
}

impl Subject<'_> {
    fn parsed(&self) -> &ParsedMovieInfo {
        self.remote.parsed.as_ref().expect("a mapped release was parsed")
    }

    fn quality(&self) -> &QualityModel {
        &self.parsed().quality.quality
    }

    fn release(&self) -> &ReleaseInfo {
        &self.remote.release
    }

    /// `Movie.MovieFile`: the file the movie has, if it is still there.
    fn file(&self, ctx: &Context<'_>) -> Option<MovieFile> {
        if self.movie.movie_file_id == 0 {
            return None;
        }
        ctx.lookup.movie_file(self.movie.movie_file_id)
    }

    fn user_invoked_search(&self) -> bool {
        self.search.is_some_and(|s| s.user_invoked)
    }
}

pub type Spec = fn(&Context<'_>, &Subject<'_>) -> Option<MovieRejection>;

/// Every specification with its priority, in the order the original
/// evaluates them: that of its source files, a folder's own before those
/// of its subfolders.
pub const SPECIFICATIONS: &[(&str, Priority, Spec)] = &[
    ("AcceptableSize", Priority::Default, acceptable_size),
    ("AlreadyImported", Priority::Default, already_imported),
    ("BlockedIndexer", Priority::Default, blocked_indexer),
    ("Blocklist", Priority::Default, blocklist),
    (
        "CustomFormatAllowedByProfile",
        Priority::Default,
        custom_format_allowed_by_profile,
    ),
    ("FreeSpace", Priority::Disk, free_space),
    ("HardcodeSubs", Priority::Default, hardcode_subs),
    ("Language", Priority::Default, language),
    ("MaximumSize", Priority::Default, maximum_size),
    ("MinimumAge", Priority::Default, minimum_age),
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
    ("RequiredIndexerFlags", Priority::Default, required_indexer_flags),
    ("Retention", Priority::Default, retention),
    ("TorrentSeeding", Priority::Default, torrent_seeding),
    ("UpgradeAllowed", Priority::Default, upgrade_allowed),
    ("UpgradeDisk", Priority::Default, upgrade_disk),
    // The original's `RssSync` and `Search` folders, after the rest.
    ("Availability", Priority::Default, availability),
    ("Delay", Priority::Default, delay),
    ("History", Priority::Default, history),
    ("IndexerTag", Priority::Default, indexer_tag),
    ("MonitoredMovie", Priority::Default, monitored_movie),
    ("Pending", Priority::Default, pending),
    ("Proper", Priority::Default, proper),
    ("Movie", Priority::Default, movie_matches_search),
];

fn permanent(reason: R, message: impl Into<String>) -> Option<MovieRejection> {
    Some(MovieRejection::permanent(reason, message))
}

fn temporary(reason: R, message: impl Into<String>) -> Option<MovieRejection> {
    Some(MovieRejection::temporary(reason, message))
}

fn age_minutes(now: DateTime<Utc>, release: &ReleaseInfo) -> f64 {
    (now - release.publish_date).num_milliseconds() as f64 / 60_000.0
}

fn age_days(now: DateTime<Utc>, release: &ReleaseInfo) -> i64 {
    (now - release.publish_date).num_days()
}

/// `QualityModel.ToString()`, with Radarr's quality names.
pub fn quality_string(q: &QualityModel) -> String {
    format!(
        "{} {}",
        MovieQuality::by_id(q.quality_id).name,
        revision_string(q)
    )
}

/// `QualityProfileQualityItem.ToString()`, with Radarr's quality names.
pub fn profile_item_string(profile: &QualityProfile, quality_or_group: i64) -> String {
    let Some(item) = profile
        .items
        .get(crate::profile::index_of(profile, quality_or_group))
    else {
        return String::new();
    };
    let joined = crate::profile::qualities_of(item)
        .into_iter()
        .map(|id| MovieQuality::by_id(id).name)
        .collect::<Vec<_>>()
        .join(", ");
    match item.name.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(name) => format!("{name} ({joined})"),
        None => joined,
    }
}

/// `ConcatToString` of custom formats: their names.
fn format_names(formats: &[CustomFormat]) -> String {
    formats
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn language_names(languages: &[i64]) -> String {
    languages
        .iter()
        .map(|id| MovieLanguage::name_of(*id))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `Movie.ToString()`.
fn movie_string(movie: &Movie) -> String {
    format!(
        "[{} ({})][{}, {}]",
        movie.metadata.title,
        movie.metadata.year,
        movie.metadata.imdb_id.as_deref().unwrap_or(""),
        movie.metadata.tmdb_id
    )
}

// --- the specifications

pub fn acceptable_size(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let size = s.release().size;
    if size == 0 {
        return None;
    }
    let definition = ctx.catalog.definition(s.quality().quality_id);
    // A movie without a runtime is weighed as one of 110 minutes.
    let runtime = match s.movie.metadata.runtime {
        0 => 110,
        minutes => minutes,
    };
    if let Some(min) = definition.min_size {
        let min_size = (min * 1024.0 * 1024.0) as i64 * runtime;
        if size < min_size {
            return permanent(
                R::BelowMinimumSize,
                format!(
                    "{} is smaller than minimum allowed {} (for {})",
                    size_suffix(size),
                    size_suffix(min_size),
                    s.movie.metadata.title
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
                    "{} is larger than maximum allowed {} (for {})",
                    size_suffix(size),
                    size_suffix(max_size),
                    s.movie.metadata.title
                ),
            );
        }
    }
    None
}

pub fn already_imported(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if !ctx.catalog.base.config.enable_completed_download_handling {
        return None;
    }
    if s.movie.movie_file_id == 0 {
        return None;
    }
    let history = ctx.lookup.history(s.movie.id);
    let last_grabbed = history
        .iter()
        .find(|h| h.event_type == MovieHistoryEvent::Grabbed)?;
    let imported = history.iter().find(|h| {
        h.event_type == MovieHistoryEvent::DownloadFolderImported && h.download_id == last_grabbed.download_id
    })?;
    // Only a guard against grabbing the same release again and again when
    // the grabbed and the imported qualities differ.
    if last_grabbed.quality == imported.quality {
        return None;
    }
    let release = s.release();
    if release.protocol == Protocol::Torrent {
        if let (Some(hash), Some(download_id)) =
            (release.info_hash.as_deref(), last_grabbed.download_id.as_deref())
        {
            if hash.to_uppercase() == download_id {
                return permanent(
                    R::AlreadyImportedSameHash,
                    "Has same torrent hash as a grabbed and imported release",
                );
            }
        }
    }
    let same_title = last_grabbed
        .source_title
        .as_deref()
        .is_some_and(|t| t.to_lowercase() == release.title.to_lowercase());
    if same_title {
        return permanent(
            R::AlreadyImportedSameName,
            "Has same release name as a grabbed and imported release",
        );
    }
    None
}

pub fn blocked_indexer(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let status = ctx.catalog.base.blocked_indexers.get(&s.release().indexer_id)?;
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

/// `BlocklistService.Blocklisted`.
pub fn blocklisted(entries: &[MovieBlocklistEntry], release: &ReleaseInfo) -> bool {
    let titled = |b: &&MovieBlocklistEntry| {
        b.source_title
            .as_deref()
            .is_some_and(|t| t.to_lowercase().contains(&release.title.to_lowercase()))
    };
    let same_indexer = |item: &MovieBlocklistEntry| {
        item.indexer
            .as_deref()
            .is_none_or(|i| i.trim().is_empty() || i.eq_ignore_ascii_case(&release.indexer))
    };
    if release.protocol == Protocol::Torrent {
        let hash = release.info_hash.as_deref().filter(|h| !h.trim().is_empty());
        let same_torrent = |item: &MovieBlocklistEntry| match hash {
            Some(hash) => item
                .torrent_info_hash
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(hash)),
            None => same_indexer(item),
        };
        return match hash {
            Some(hash) => entries
                .iter()
                .filter(|b| {
                    b.torrent_info_hash
                        .as_deref()
                        .is_some_and(|h| h.to_lowercase().contains(&hash.to_lowercase()))
                })
                .any(same_torrent),
            None => entries
                .iter()
                .filter(titled)
                .filter(|b| b.protocol == Protocol::Torrent)
                .any(same_torrent),
        };
    }
    let same_nzb = |item: &MovieBlocklistEntry| {
        if item.published_date == Some(release.publish_date) {
            return true;
        }
        let same_date = item.published_date.is_none_or(|d| {
            d - Duration::minutes(2) <= release.publish_date
                && d + Duration::minutes(2) >= release.publish_date
        });
        let same_size = item
            .size
            .is_none_or(|size| (size - release.size).abs() <= megabytes(2));
        !same_indexer(item) && same_date && same_size
    };
    entries
        .iter()
        .filter(titled)
        .filter(|b| b.protocol == Protocol::Usenet)
        .any(same_nzb)
}

pub fn blocklist(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if blocklisted(&ctx.lookup.blocklist(s.movie.id), s.release()) {
        return permanent(R::Blocklisted, "Release is blocklisted");
    }
    None
}

pub fn custom_format_allowed_by_profile(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let minimum = s.profile.min_format_score;
    let score = s.remote.custom_format_score;
    if score < minimum {
        return permanent(
            R::CustomFormatMinimumScore,
            format!(
                "Custom Formats {} have score {score} below Movie's profile minimum {minimum}",
                format_names(&s.remote.custom_formats)
            ),
        );
    }
    None
}

pub fn free_space(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let config = &ctx.catalog.base.config;
    if config.skip_free_space_check_when_importing {
        return None;
    }
    let free = ctx.lookup.free_space(&s.movie.path)?;
    let minimum = megabytes(config.minimum_free_space_when_importing_mb);
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

pub fn hardcode_subs(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let subs = s
        .parsed()
        .hardcoded_subs
        .as_deref()
        .filter(|h| !h.trim().is_empty())?;
    if ctx.catalog.allow_hardcoded_subs {
        return None;
    }
    let lower = subs.to_lowercase();
    let whitelisted = ctx
        .catalog
        .whitelisted_hardcoded_subs
        .split(',')
        .any(|t| !t.trim().is_empty() && lower.contains(&t.to_lowercase()));
    if whitelisted {
        return None;
    }
    permanent(R::HardcodeSubtitles, format!("Hardcode subs found: {subs}"))
}

pub fn language(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    use crate::movies::parser::language::lang;
    let wanted = ctx.catalog.profile_language(s.profile.id);
    if wanted == lang::ANY {
        return None;
    }
    let original = s.movie.metadata.original_language_id;
    if wanted == lang::ORIGINAL {
        if !s.remote.languages.contains(&original) {
            return permanent(
                R::WantedLanguage,
                format!(
                    "Original Language ({}) is wanted, but found {}",
                    MovieLanguage::name_of(original),
                    language_names(&s.remote.languages)
                ),
            );
        }
        return None;
    }
    if !s.remote.languages.contains(&wanted) {
        return permanent(
            R::WantedLanguage,
            format!(
                "{} is wanted, but found {}",
                MovieLanguage::name_of(wanted),
                language_names(&s.remote.languages)
            ),
        );
    }
    None
}

pub fn maximum_size(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let size = s.release().size;
    let maximum = megabytes(ctx.catalog.base.config.indexer.maximum_size);
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

pub fn minimum_age(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.release().protocol != Protocol::Usenet {
        return None;
    }
    let minimum = ctx.catalog.base.config.indexer.minimum_age;
    if minimum == 0 {
        return None;
    }
    let age = age_minutes(ctx.now, s.release());
    if age < minimum as f64 {
        let rounded = (age * 10.0).round() / 10.0;
        return temporary(
            R::MinimumAge,
            format!("Only {rounded} minutes old, minimum age is {minimum} minutes"),
        );
    }
    None
}

pub fn not_sample(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let release = s.release();
    if release.title.to_lowercase().contains("sample") && release.size < megabytes(70) {
        return permanent(R::Sample, "Sample");
    }
    None
}

pub fn protocol(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let delay = ctx.catalog.base.delay_profile_for(&s.movie.tags)?;
    match s.release().protocol {
        Protocol::Usenet if !delay.enable_usenet => {
            permanent(R::ProtocolDisabled, "Usenet is not enabled for this movie")
        }
        Protocol::Torrent if !delay.enable_torrent => {
            permanent(R::ProtocolDisabled, "Torrent is not enabled for this movie")
        }
        _ => None,
    }
}

pub fn quality_allowed_by_profile(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let quality = s.quality().quality_id;
    let item = s
        .profile
        .items
        .get(crate::profile::index_of(s.profile, quality))?;
    if !item.allowed {
        return permanent(
            R::QualityNotWanted,
            format!("{} is not wanted in profile", MovieQuality::by_id(quality).name),
        );
    }
    None
}

/// What an `UpgradeableRejectReason` says about something the movie
/// already has (in the queue, on disk, grabbed lately).
struct UpgradeWords {
    higher_preference: (R, &'static str),
    higher_revision: (R, &'static str),
    quality_cutoff: (R, &'static str),
    format_cutoff: (R, &'static str),
    format_score: (R, &'static str),
    format_increment: (R, &'static str),
    not_allowed: (R, &'static str),
}

fn upgrade_rejection(
    words: &UpgradeWords,
    reason: UpgradeRejectReason,
    profile: &QualityProfile,
    current: &QualityModel,
    current_formats: &[CustomFormat],
) -> Option<MovieRejection> {
    let say = |(reason, text): (R, &str), value: String| permanent(reason, text.replace("{}", &value));
    match reason {
        UpgradeRejectReason::None => None,
        UpgradeRejectReason::BetterQuality => say(words.higher_preference, quality_string(current)),
        UpgradeRejectReason::BetterRevision => say(words.higher_revision, revision_string(current)),
        UpgradeRejectReason::QualityCutoff => {
            say(words.quality_cutoff, profile_item_string(profile, profile.cutoff))
        }
        UpgradeRejectReason::CustomFormatCutoff => {
            say(words.format_cutoff, profile.cutoff_format_score.to_string())
        }
        UpgradeRejectReason::CustomFormatScore => say(
            words.format_score,
            upgradable::format_score(profile, current_formats).to_string(),
        ),
        UpgradeRejectReason::MinCustomFormatScore => say(
            words.format_increment,
            profile.min_upgrade_format_score.to_string(),
        ),
        UpgradeRejectReason::UpgradesNotAllowed => say(words.not_allowed, profile.name.clone()),
    }
}

const QUEUE_WORDS: UpgradeWords = UpgradeWords {
    higher_preference: (
        R::QueueHigherPreference,
        "Release in queue is of equal or higher preference: {}",
    ),
    higher_revision: (
        R::QueueHigherRevision,
        "Release in queue is of equal or higher revision: {}",
    ),
    quality_cutoff: (R::QueueCutoffMet, "Release in queue meets quality cutoff: {}"),
    format_cutoff: (
        R::QueueCustomFormatCutoffMet,
        "Release in queue meets Custom Format cutoff: {}",
    ),
    format_score: (
        R::QueueCustomFormatScore,
        "Release in queue has an equal or higher Custom Format score: {}",
    ),
    format_increment: (
        R::QueueCustomFormatScoreIncrement,
        "Release in queue has Custom Format score within Custom Format score increment: {}",
    ),
    not_allowed: (
        R::QueueUpgradesNotAllowed,
        "Release in queue and Quality Profile '{}' does not allow upgrades",
    ),
};

const DISK_WORDS: UpgradeWords = UpgradeWords {
    higher_preference: (
        R::DiskHigherPreference,
        "Existing file on disk is of equal or higher preference: {}",
    ),
    higher_revision: (
        R::DiskHigherRevision,
        "Existing file on disk is of equal or higher revision: {}",
    ),
    quality_cutoff: (R::DiskCutoffMet, "Existing file on disk meets quality cutoff: {}"),
    format_cutoff: (
        R::DiskCustomFormatCutoffMet,
        "Existing file on disk meets Custom Format cutoff: {}",
    ),
    format_score: (
        R::DiskCustomFormatScore,
        "Existing file on disk has a equal or higher Custom Format score: {}",
    ),
    format_increment: (
        R::DiskCustomFormatScoreIncrement,
        "Existing file on disk has Custom Format score within Custom Format score increment: {}",
    ),
    not_allowed: (
        R::DiskUpgradesNotAllowed,
        "Existing file on disk and Quality Profile '{}' does not allow upgrades",
    ),
};

pub fn queue(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let propers = ctx.catalog.base.config.download_propers_and_repacks;
    for item in &ctx.catalog.queue {
        if item.remote.movie.as_ref().map(|m| m.id) != Some(s.movie.id) || item.failed_pending {
            continue;
        }
        let Some(queued) = item.remote.parsed.as_ref().map(|p| p.quality.quality) else {
            continue;
        };
        let queued_formats = remote_formats(ctx.catalog, &item.remote, item.size);
        if !upgradable::cutoff_not_met(s.profile, &queued, &queued_formats, Some(s.quality())) {
            return permanent(
                R::QueueCutoffMet,
                format!(
                    "Quality for release in queue already meets cutoff: {}",
                    quality_string(&queued)
                ),
            );
        }
        let reason = upgradable::is_upgradable(
            propers,
            s.profile,
            &queued,
            &queued_formats,
            s.quality(),
            &s.remote.custom_formats,
        );
        if let Some(rejection) = upgrade_rejection(&QUEUE_WORDS, reason, s.profile, &queued, &queued_formats)
        {
            return Some(rejection);
        }
        if upgradable::is_revision_upgrade(&queued, s.quality())
            && propers == ProperDownloadTypes::DoNotUpgrade
        {
            return permanent(R::QueuePropersDisabled, "Proper downloading is disabled");
        }
    }
    None
}

static DISC_PATTERNS: [Lazy; 3] = [
    net_regex!(r"(?:dis[ck])(?:[-_. ]\d+[-_. ])(?:(?:(?:480|720|1080|2160)[ip]|)[-_. ])?(?:Blu\-?ray)"),
    net_regex!(r"(?:(?:480|720|1080|2160)[ip]|)[-_. ](?:full)[-_. ](?:Blu\-?ray)"),
    net_regex!(r"(?:\d?x?M?DVD-?[R59])(?:[ ._]|$)"),
];

pub fn raw_disk(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
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

pub fn release_restrictions(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let title = &s.release().title;
    let profiles = ctx
        .catalog
        .base
        .release_profiles_for(&s.movie.tags, s.release().indexer_id);
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

pub fn repack(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let propers = ctx.catalog.base.config.download_propers_and_repacks;
    if !s.quality().is_repack || propers == ProperDownloadTypes::DoNotPrefer {
        return None;
    }
    let file = s.file(ctx)?;
    if !upgradable::is_revision_upgrade(&file.quality, s.quality()) {
        return None;
    }
    if propers == ProperDownloadTypes::DoNotUpgrade {
        return permanent(R::RepackDisabled, "Repack downloading is disabled");
    }
    let Some(file_group) = file.release_group.as_deref().filter(|g| !g.trim().is_empty()) else {
        return permanent(
            R::RepackUnknownReleaseGroup,
            "Unable to determine release group for the existing file",
        );
    };
    let Some(group) = s
        .parsed()
        .release_group
        .as_deref()
        .filter(|g| !g.trim().is_empty())
    else {
        return permanent(
            R::RepackUnknownReleaseGroup,
            "Unable to determine release group for this release",
        );
    };
    if file_group.to_lowercase() != group.to_lowercase() {
        return permanent(
            R::RepackReleaseGroupDoesNotMatch,
            format!(
                "Release is a repack for a different release group. Release Group: {group}. File release group: {file_group}"
            ),
        );
    }
    None
}

pub fn required_indexer_flags(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let required = ctx.catalog.required_flags.get(&s.release().indexer_id)?;
    if required.is_empty() {
        return None;
    }
    let found = s.release().indexer_flags;
    // `HasFlag`: every bit of the flag, so a flag of no bit is always there.
    if required.iter().any(|flag| found & flag == *flag) {
        return None;
    }
    let all = required.iter().fold(0, |acc, flag| acc | flag);
    let names = |flags: i64| match indexer_flag_names(flags).join(", ") {
        text if text.is_empty() => "0".to_string(),
        text => text,
    };
    permanent(
        R::RequiredFlags,
        format!(
            "None of the required indexer flags {} where found. Found flags: {}",
            names(all),
            names(found)
        ),
    )
}

pub fn retention(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.release().protocol != Protocol::Usenet {
        return None;
    }
    let retention = ctx.catalog.base.config.indexer.retention;
    if retention > 0 && age_days(ctx.now, s.release()) > retention {
        return permanent(R::MaximumAge, "Older than configured retention");
    }
    None
}

pub fn availability(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.user_invoked_search() {
        return None;
    }
    let delay = ctx.catalog.availability_delay;
    if is_available(s.movie, ctx.now, delay) {
        return None;
    }
    let status = match s.movie.minimum_availability {
        MovieStatus::Deleted => "Deleted",
        MovieStatus::Tba => "TBA",
        MovieStatus::Announced => "Announced",
        MovieStatus::InCinemas => "InCinemas",
        MovieStatus::Released => "Released",
    };
    permanent(
        R::Availability,
        format!(
            "Movie {} will only be considered available {delay} days after {status}",
            movie_string(s.movie)
        ),
    )
}

pub fn delay(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.user_invoked_search() {
        return None;
    }
    let profile = s.profile;
    let delay_profile = ctx.catalog.base.delay_profile_for(&s.movie.tags)?;
    let protocol = s.release().protocol;
    let delay = match protocol {
        Protocol::Usenet => delay_profile.usenet_delay,
        _ => delay_profile.torrent_delay,
    };
    let preferred = protocol == delay_profile.preferred_protocol;
    if delay == 0 {
        return None;
    }
    if preferred {
        if let Some(file) = s.file(ctx) {
            let formats = file_formats(ctx.catalog, &file, s.movie);
            let reason = upgradable::is_upgradable(
                ctx.catalog.base.config.download_propers_and_repacks,
                profile,
                &file.quality,
                &formats,
                s.quality(),
                &s.remote.custom_formats,
            );
            if reason == UpgradeRejectReason::None
                && upgradable::is_revision_upgrade(&file.quality, s.quality())
            {
                return None;
            }
        }
    }
    if delay_profile.bypass_if_highest_quality {
        // `LastAllowedQuality`: the last quality of the last allowed item.
        let best = profile
            .items
            .iter()
            .rev()
            .find(|i| i.allowed)
            .and_then(|i| crate::profile::qualities_of(i).last().copied());
        let is_best = best.is_some_and(|best| {
            upgradable::compare_quality(profile, s.quality().quality_id, best) != std::cmp::Ordering::Less
        });
        if is_best && preferred {
            return None;
        }
    }
    if delay_profile.bypass_if_above_custom_format_score
        && s.remote.custom_format_score >= delay_profile.minimum_custom_format_score
        && preferred
    {
        return None;
    }
    if let Some(oldest) = ctx.lookup.oldest_pending(s.movie.id) {
        let age = (ctx.now - oldest).num_milliseconds() as f64 / 60_000.0;
        if age > delay as f64 {
            return None;
        }
    }
    if age_minutes(ctx.now, s.release()) < delay as f64 {
        return temporary(R::MinimumAgeDelay, "Waiting for better quality release");
    }
    None
}

pub fn history(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.search.is_some() {
        return None;
    }
    let cdh_enabled = ctx.catalog.base.config.enable_completed_download_handling;
    let most_recent = ctx.lookup.history(s.movie.id).into_iter().next()?;
    if most_recent.event_type != MovieHistoryEvent::Grabbed {
        return None;
    }
    let recent = most_recent.date > ctx.now - Duration::hours(12);
    if !recent && cdh_enabled {
        return None;
    }
    let formats = history_formats(ctx.catalog, &most_recent, s.movie);
    let grabbed = &most_recent.quality;
    let cutoff_unmet = upgradable::cutoff_not_met(s.profile, grabbed, &formats, Some(s.quality()));
    let reason = upgradable::is_upgradable(
        ctx.catalog.base.config.download_propers_and_repacks,
        s.profile,
        grabbed,
        &formats,
        s.quality(),
        &s.remote.custom_formats,
    );
    if !cutoff_unmet {
        if recent {
            return permanent(
                R::HistoryRecentCutoffMet,
                format!(
                    "Recent grab event in history already meets cutoff: {}",
                    quality_string(grabbed)
                ),
            );
        }
        return permanent(
            R::HistoryCdhDisabledCutoffMet,
            format!(
                "CDH is disabled and grab event in history already meets cutoff: {}",
                quality_string(grabbed)
            ),
        );
    }
    let subject = if recent { "Recent" } else { "CDH is disabled and" };
    let say = |reason: R, rest: String| permanent(reason, format!("{subject} grab event in history {rest}"));
    match reason {
        UpgradeRejectReason::None => None,
        UpgradeRejectReason::BetterQuality => say(
            R::HistoryHigherPreference,
            format!("is of equal or higher preference: {}", quality_string(grabbed)),
        ),
        UpgradeRejectReason::BetterRevision => say(
            R::HistoryHigherRevision,
            format!("is of equal or higher revision: {}", revision_string(grabbed)),
        ),
        UpgradeRejectReason::QualityCutoff => say(
            R::HistoryCutoffMet,
            format!(
                "meets quality cutoff: {}",
                profile_item_string(s.profile, s.profile.cutoff)
            ),
        ),
        UpgradeRejectReason::CustomFormatCutoff => say(
            R::HistoryCustomFormatCutoffMet,
            format!("meets Custom Format cutoff: {}", s.profile.cutoff_format_score),
        ),
        UpgradeRejectReason::CustomFormatScore => say(
            R::HistoryCustomFormatScore,
            format!(
                "has an equal or higher Custom Format score: {}",
                upgradable::format_score(s.profile, &formats)
            ),
        ),
        UpgradeRejectReason::MinCustomFormatScore => say(
            R::HistoryCustomFormatScoreIncrement,
            format!(
                "has Custom Format score within Custom Format score increment: {}",
                s.profile.min_upgrade_format_score
            ),
        ),
        UpgradeRejectReason::UpgradesNotAllowed => say(
            R::HistoryUpgradesNotAllowed,
            format!("and Quality Profile '{}' does not allow upgrades", s.profile.name),
        ),
    }
}

pub fn indexer_tag(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let release = s.release();
    if release.indexer_id == 0 {
        return None;
    }
    let indexer = ctx.catalog.base.indexer(release.indexer_id)?;
    if !indexer.tags.is_empty() && !indexer.tags.iter().any(|t| s.movie.tags.contains(t)) {
        return permanent(
            R::NoMatchingTag,
            "Movie tags do not match any of the indexer tags",
        );
    }
    None
}

pub fn monitored_movie(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.user_invoked_search() || s.movie.monitored {
        return None;
    }
    permanent(R::MovieNotMonitored, "Movie is not monitored")
}

pub fn pending(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.remote.release_source == ReleaseSource::Rss || s.user_invoked_search() {
        return None;
    }
    if ctx.lookup.pending_movie_ids().contains(&s.movie.id) {
        return temporary(
            R::MinimumAgeDelayPushed,
            "Release containing at least one matching movie is already pending, delaying pushed release",
        );
    }
    None
}

pub fn proper(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    if s.search.is_some() {
        return None;
    }
    let propers = ctx.catalog.base.config.download_propers_and_repacks;
    if propers == ProperDownloadTypes::DoNotPrefer {
        return None;
    }
    let file = s.file(ctx)?;
    if !upgradable::is_revision_upgrade(&file.quality, s.quality()) {
        return None;
    }
    if propers == ProperDownloadTypes::DoNotUpgrade {
        return permanent(R::PropersDisabled, "Proper downloading is disabled");
    }
    // `DateTime.Today.AddDays(-7)`: midnight a week ago.
    let today = ctx.now.date_naive().and_hms_opt(0, 0, 0)?.and_utc();
    if file.date_added < today - Duration::days(7) {
        return permanent(R::ProperForOldFile, "Proper for old file");
    }
    None
}

pub fn movie_matches_search(_ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let search = s.search?;
    if s.movie.id != search.movie.movie.id {
        return permanent(R::WrongMovie, "Wrong movie");
    }
    None
}

pub fn torrent_seeding(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let release = s.release();
    if release.protocol != Protocol::Torrent || release.indexer_id == 0 {
        return None;
    }
    let minimum = ctx.catalog.base.indexer(release.indexer_id)?.minimum_seeders?;
    let seeders = release.seeders?;
    if seeders < minimum {
        return permanent(
            R::MinimumSeeders,
            format!("Not enough seeders: {seeders}. Minimum seeders: {minimum}"),
        );
    }
    None
}

pub fn upgrade_allowed(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let file = s.file(ctx)?;
    let formats = file_formats(ctx.catalog, &file, s.movie);
    if !upgradable::is_upgrade_allowed(
        s.profile,
        &file.quality,
        &formats,
        s.quality(),
        &s.remote.custom_formats,
    ) {
        return permanent(
            R::QualityUpgradesDisabled,
            "Existing file and the Quality profile does not allow upgrades",
        );
    }
    None
}

pub fn upgrade_disk(ctx: &Context<'_>, s: &Subject<'_>) -> Option<MovieRejection> {
    let file = s.file(ctx)?;
    let formats = file_formats(ctx.catalog, &file, s.movie);
    if !upgradable::cutoff_not_met(s.profile, &file.quality, &formats, Some(s.quality())) {
        return permanent(
            R::DiskCutoffMet,
            format!(
                "Existing file meets cutoff: {} [{}]",
                profile_item_string(s.profile, crate::profile::effective_cutoff(s.profile)),
                format_names(&formats)
            ),
        );
    }
    let reason = upgradable::is_upgradable(
        ctx.catalog.base.config.download_propers_and_repacks,
        s.profile,
        &file.quality,
        &formats,
        s.quality(),
        &s.remote.custom_formats,
    );
    upgrade_rejection(&DISK_WORDS, reason, s.profile, &file.quality, &formats)
}
