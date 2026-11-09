//! The import specifications (`MediaFiles/EpisodeImport/Specifications`),
//! each a function from a local episode and its download client item to a
//! rejection or nothing, run in the originals' order with their messages.

use chrono::{DateTime, Duration, Utc};

use super::local::{ImportRejection, LocalEpisode, RejectionReason as R};
use super::sample::{self, DetectSampleResult};
use crate::decision::formats;
use crate::decision::lookup::{Catalog, Lookup, ProperDownloadTypes};
use crate::decision::mapping;
use crate::decision::specs::same_episodes;
use crate::decision::upgradable;
use crate::downloadclients::DownloadClientItem;
use crate::model::{Episode, HistoryEvent, NamingConfig, SeriesType};
use crate::naming;
use crate::store::config::{EpisodeTitleRequired, MediaManagementConfig};

/// Everything the specifications consult besides the file.
pub struct Context<'a> {
    pub lookup: &'a dyn Lookup,
    pub catalog: &'a Catalog,
    pub config: &'a MediaManagementConfig,
    pub naming: &'a NamingConfig,
    pub now: DateTime<Utc>,
}

type Spec = fn(&Context<'_>, &LocalEpisode, Option<&DownloadClientItem>) -> Option<ImportRejection>;

/// The specifications in the order the originals register them.
pub const SPECIFICATIONS: &[(&str, Spec)] = &[
    ("AbsoluteEpisodeNumberSpecification", absolute_episode_number),
    ("AlreadyImportedSpecification", already_imported),
    ("EpisodeTitleSpecification", episode_title),
    ("FreeSpaceSpecification", free_space),
    ("FullSeasonSpecification", full_season),
    ("HasAudioTrackSpecification", has_audio_track),
    ("MatchesFolderSpecification", matches_folder),
    ("MatchesGrabSpecification", matches_grab),
    ("NotSampleSpecification", not_sample),
    ("NotUnpackingSpecification", not_unpacking),
    ("SameEpisodesImportSpecification", same_episodes_import),
    ("SplitEpisodeSpecification", split_episode),
    (
        "UnverifiedSceneNumberingSpecification",
        unverified_scene_numbering,
    ),
    ("UpgradeSpecification", upgrade),
];

fn reject(reason: R, message: impl Into<String>) -> Option<ImportRejection> {
    Some(ImportRejection::new(reason, message))
}

fn format_episodes(episodes: &[&Episode]) -> String {
    episodes
        .iter()
        .map(|e| format!("{}x{:02}", e.season_number, e.episode_number))
        .collect::<Vec<_>>()
        .join(", ")
}

/// An anime episode that aired within the day needs an absolute number
/// when the anime format uses one.
fn absolute_episode_number(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.series.as_ref()?.series_type != SeriesType::Anime {
        return None;
    }
    if !naming::config_requires_absolute_episode_number(ctx.naming) {
        return None;
    }
    for episode in &local.episodes {
        if episode
            .air_date_utc
            .is_some_and(|a| a < ctx.now - Duration::days(1))
        {
            continue;
        }
        if episode.absolute_episode_number.is_none() {
            return reject(
                R::MissingAbsoluteEpisodeNumber,
                "Episode does not have an absolute episode number and recently aired",
            );
        }
    }
    None
}

/// The download was imported already, and not grabbed again since.
fn already_imported(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    item: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    let item = item?;
    for episode in &local.episodes {
        if episode.episode_file_id == 0 {
            continue;
        }
        let history = ctx.lookup.history_for_episode(episode.id);
        let last_imported = history.iter().find(|h| {
            h.download_id.as_deref() == Some(&item.download_id)
                && h.event_type == HistoryEvent::DownloadFolderImported
        });
        let last_grabbed = history.iter().find(|h| {
            h.download_id.as_deref() == Some(&item.download_id) && h.event_type == HistoryEvent::Grabbed
        });
        let Some(imported) = last_imported else {
            continue;
        };
        match last_grabbed {
            Some(grabbed) if grabbed.date > imported.date => continue,
            Some(grabbed) if imported.date > grabbed.date => {
                return reject(
                    R::EpisodeAlreadyImported,
                    format!("Episode file already imported at {}", local_time(imported.date)),
                );
            }
            Some(_) => {}
            None => {
                return reject(
                    R::EpisodeAlreadyImported,
                    format!("Episode file already imported at {}", local_time(imported.date)),
                );
            }
        }
    }
    None
}

/// `DateTime.ToLocalTime()` as .NET prints it; the daemon runs in UTC.
fn local_time(d: DateTime<Utc>) -> String {
    d.format("%m/%d/%Y %H:%M:%S").to_string()
}

/// Recently aired episodes without a title wait when the format names it.
fn episode_title(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.existing_file {
        return None;
    }
    let required = ctx.config.episode_title_required;
    if required == EpisodeTitleRequired::Never {
        return None;
    }
    let series = local.series.as_ref()?;
    let all_absolute = local.episodes.iter().all(|e| e.absolute_episode_number.is_some());
    if !naming::requires_episode_title(series.series_type, all_absolute, ctx.naming) {
        return None;
    }
    let first = local.episodes.first()?;
    // The originals look the season up by the first episode's number.
    let episodes_in_season = ctx
        .lookup
        .episodes_by_season(first.series_id, first.episode_number);
    let all_same_day = first.air_date_utc.is_some()
        && local
            .episodes
            .iter()
            .all(|e| e.air_date_utc.is_none() || e.air_date_utc == first.air_date_utc);
    if required == EpisodeTitleRequired::BulkSeasonReleases
        && all_same_day
        && episodes_in_season
            .iter()
            .filter(|e| e.air_date_utc.is_none() || e.air_date_utc == first.air_date_utc)
            .count()
            < 4
    {
        return None;
    }
    for episode in &local.episodes {
        if episode
            .air_date_utc
            .is_some_and(|a| a < ctx.now - Duration::hours(48))
        {
            continue;
        }
        let title = episode.title.as_deref().unwrap_or("");
        if title.trim().is_empty() {
            return reject(
                R::TitleMissing,
                "Episode does not have a title and recently aired",
            );
        }
        if title == "TBA" {
            return reject(R::TitleTba, "Episode has a TBA title and recently aired");
        }
    }
    None
}

/// The root folder must have room for the file plus the configured margin.
fn free_space(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if ctx.config.skip_free_space_check_when_importing {
        return None;
    }
    if local.existing_file {
        return None;
    }
    let series = local.series.as_ref()?;
    let parent = super::parent_path(&series.path)?;
    let Some(free) = ctx.lookup.free_space(parent) else {
        tracing::debug!("Free space check returned an invalid result for: {parent}");
        return None;
    };
    let needed = local.size + ctx.config.minimum_free_space_when_importing_mb * 1024 * 1024;
    if free < needed {
        tracing::warn!(
            "Not enough free space ({free}) to import: {} ({})",
            local.path,
            local.size
        );
        return reject(R::MinimumFreeSpace, "Not enough free space");
    }
    None
}

/// A single file whose name says it holds the whole season.
fn full_season(
    _: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.file_episode_info.as_ref()?.full_season {
        return reject(
            R::FullSeason,
            "Single episode file contains all episodes in seasons. Review file name or manually import",
        );
    }
    None
}

fn has_audio_track(
    _: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    let info = local.media_info.as_ref()?;
    if info.audio_stream_count() == 0 {
        return reject(R::NoAudio, "No audio tracks detected");
    }
    None
}

/// A file naming episodes the folder does not.
fn matches_folder(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.existing_file {
        return None;
    }
    let series = local.series.as_ref()?;
    let mut file_info = local.file_episode_info.clone();
    let mut folder_info = local.folder_episode_info.clone();
    if let Some(f) = &file_info {
        if f.is_possible_scene_season_special() {
            if let Ok(Some(special)) = mapping::parse_special_episode_title(
                ctx.lookup,
                Some(f),
                &f.release_title,
                series.tvdb_id,
                0,
                None,
                None,
            ) {
                file_info = Some(special);
            }
        }
    }
    if let Some(f) = &folder_info {
        if f.is_possible_scene_season_special() {
            if let Ok(Some(special)) = mapping::parse_special_episode_title(
                ctx.lookup,
                Some(f),
                &f.release_title,
                series.tvdb_id,
                0,
                None,
                None,
            ) {
                folder_info = Some(special);
            }
        }
    }
    let folder_info = folder_info?;
    let file_info = file_info?;
    let folder_episodes =
        mapping::get_episodes(ctx.lookup, &folder_info, series, true, None).unwrap_or_default();
    let file_episodes = mapping::get_episodes(ctx.lookup, &file_info, series, true, None).unwrap_or_default();
    if folder_episodes.is_empty() {
        return None;
    }
    let unexpected: Vec<&Episode> = file_episodes
        .iter()
        .filter(|e| !folder_episodes.iter().any(|o| o.id == e.id))
        .collect();
    if unexpected.is_empty() {
        return None;
    }
    let word = if unexpected.len() == 1 {
        "Episode"
    } else {
        "Episodes"
    };
    let verb = if unexpected.len() == 1 { "was" } else { "were" };
    reject(
        R::EpisodeUnexpected,
        format!(
            "{word} {} {verb} unexpected considering the {} folder name",
            format_episodes(&unexpected),
            folder_info.release_title
        ),
    )
}

/// A file naming episodes the grab did not.
fn matches_grab(
    _: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.existing_file {
        return None;
    }
    let release = local.release.as_ref()?;
    if release.episode_ids.is_empty() {
        return None;
    }
    let unexpected: Vec<&Episode> = local
        .episodes
        .iter()
        .filter(|e| !release.episode_ids.contains(&e.id))
        .collect();
    if unexpected.is_empty() {
        return None;
    }
    let word = if unexpected.len() == 1 {
        "Episode"
    } else {
        "Episodes"
    };
    let verb = if unexpected.len() == 1 { "was" } else { "were" };
    reject(
        R::EpisodeNotFoundInRelease,
        format!(
            "{word} {} {verb} not found in the grabbed release: {}",
            format_episodes(&unexpected),
            release.title
        ),
    )
}

fn not_sample(
    _: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.existing_file {
        return None;
    }
    if local.season_number().is_err() {
        // `InvalidSeasonException` is caught and logged by the originals.
        tracing::warn!(path = %local.path, "Invalid season detected during sample check");
        return None;
    }
    match sample::is_sample(local) {
        DetectSampleResult::Sample => reject(R::Sample, "Sample"),
        DetectSampleResult::Indeterminate => {
            reject(R::SampleIndeterminate, "Unable to determine if file is a sample")
        }
        DetectSampleResult::NotSample => None,
    }
}

/// A file inside a download client's working folder is still unpacking.
fn not_unpacking(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.existing_file {
        return None;
    }
    for working in ctx.config.download_client_working_folders.split('|') {
        if working.is_empty() {
            continue;
        }
        let mut parent = super::parent_path(&local.path);
        while let Some(p) = parent {
            if super::file_name(p).starts_with(working) {
                return reject(R::Unpacking, "File is still being unpacked");
            }
            if p == "/" {
                break;
            }
            parent = super::parent_path(p);
        }
    }
    None
}

fn same_episodes_import(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if same_episodes(ctx.lookup, &local.episodes) {
        return None;
    }
    reject(
        R::ExistingFileHasMoreEpisodes,
        "Episode file on disk contains more episodes than this file contains",
    )
}

fn split_episode(
    _: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.file_episode_info.as_ref()?.is_split_episode {
        return reject(R::SplitEpisode, "Single episode split into multiple files");
    }
    None
}

fn unverified_scene_numbering(
    _: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    if local.existing_file {
        return None;
    }
    if local.episodes.iter().any(|e| e.unverified_scene_numbering) {
        return reject(
            R::UnverifiedSceneMapping,
            "This show has individual episode mappings on TheXEM but the mapping for this episode has not been confirmed yet by their administrators. TheXEM needs manual input.",
        );
    }
    None
}

/// The file must be at least as good as what each episode already has.
fn upgrade(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    _: Option<&DownloadClientItem>,
) -> Option<ImportRejection> {
    use std::cmp::Ordering;
    let series = local.series.as_ref()?;
    let profile = ctx.catalog.profile(series.quality_profile_id)?;
    let propers = ctx.catalog.config.download_propers_and_repacks;
    for episode in local.episodes.iter().filter(|e| e.episode_file_id > 0) {
        let Some(file) = ctx.lookup.episode_file(episode.episode_file_id) else {
            continue;
        };
        let compare = upgradable::compare_quality(profile, local.quality.quality_id, file.quality.quality_id);
        if compare == Ordering::Less {
            return reject(
                R::NotQualityUpgrade,
                format!(
                    "Not an upgrade for existing episode file(s). Existing quality: {}. New Quality {}.",
                    crate::quality::Quality::by_id(file.quality.quality_id).name,
                    crate::quality::Quality::by_id(local.quality.quality_id).name
                ),
            );
        }
        if compare == Ordering::Equal
            && propers != ProperDownloadTypes::DoNotPrefer
            && upgradable::compare_revision(&local.quality, &file.quality) == Ordering::Less
        {
            return reject(
                R::NotRevisionUpgrade,
                "Not a quality revision upgrade for existing episode file(s)",
            );
        }
        let current_formats = formats::file_formats(ctx.catalog, &file, series);
        let current_score = upgradable::format_score(profile, &current_formats);
        let new_score = local.custom_format_score;
        if compare == Ordering::Equal && new_score < current_score {
            return reject(
                R::NotCustomFormatUpgrade,
                format!(
                    "Not a Custom Format upgrade for existing episode file(s). New: [{}] ({}) do not improve on Existing: [{}] ({})",
                    formats::names(&local.custom_formats),
                    new_score,
                    formats::names(&current_formats),
                    current_score
                ),
            );
        }
    }
    None
}

/// Every specification against one file; the rejections in order.
pub fn evaluate(
    ctx: &Context<'_>,
    local: &LocalEpisode,
    item: Option<&DownloadClientItem>,
) -> Vec<ImportRejection> {
    let mut rejections = Vec::new();
    for (_, spec) in SPECIFICATIONS {
        if let Some(r) = spec(ctx, local, item) {
            rejections.push(r);
        }
    }
    rejections
}

#[cfg(test)]
mod tests {
    //! The originals' specification fixtures
    //! (`MediaFiles/EpisodeImport/Specifications/*Fixture.cs`), which assert
    //! on mocks and so are ported as unit tests, the originals' cases grouped
    //! by specification.

    use chrono::{Duration, Utc};
    use serde_json::json;

    use super::*;
    use crate::decision::lookup::Catalog;
    use crate::decision::testing::{self, MemoryLookup};
    use crate::downloadclients::{ClientKind, ItemClientInfo};
    use crate::mediafiles::local::GrabbedRelease;
    use crate::mediafiles::mediainfo::MediaInfo;
    use crate::model::{CustomFormat, EpisodeFile, HistoryEntry, ProfileFormatItem, Protocol, QualityModel};
    use crate::parser;

    const SDTV: i64 = 1;
    const HDTV720P: i64 = 4;
    const BLURAY720P: i64 = 6;
    const BLURAY1080P: i64 = 7;

    struct Harness {
        lookup: MemoryLookup,
        catalog: Catalog,
        config: MediaManagementConfig,
        naming: NamingConfig,
        now: DateTime<Utc>,
    }

    impl Harness {
        fn new() -> Harness {
            let now = Utc::now();
            Harness {
                lookup: MemoryLookup::default(),
                catalog: Catalog {
                    now,
                    profiles: vec![testing::default_profile(&[], BLURAY1080P, true)],
                    ..Default::default()
                },
                config: MediaManagementConfig::default(),
                naming: NamingConfig::default(),
                now,
            }
        }

        fn run(
            &self,
            spec: Spec,
            local: &LocalEpisode,
            item: Option<&DownloadClientItem>,
        ) -> Option<ImportRejection> {
            let ctx = Context {
                lookup: &self.lookup,
                catalog: &self.catalog,
                config: &self.config,
                naming: &self.naming,
                now: self.now,
            };
            spec(&ctx, local, item)
        }

        fn accepts(&self, spec: Spec, local: &LocalEpisode, item: Option<&DownloadClientItem>) -> bool {
            self.run(spec, local, item).is_none()
        }
    }

    fn quality(id: i64, version: i64) -> QualityModel {
        QualityModel {
            quality_id: id,
            version,
            real: 0,
            is_repack: false,
        }
    }

    fn file(id: i64, q: QualityModel, name: &str) -> EpisodeFile {
        EpisodeFile {
            id,
            series_id: 1,
            season_number: 1,
            relative_path: format!("Season 01/{name}"),
            original_file_path: None,
            size: 100,
            date_added: Utc::now(),
            scene_name: None,
            release_group: None,
            release_hash: None,
            quality: q,
            languages: vec![1],
            media_info: None,
            indexer_flags: 0,
            release_type: Default::default(),
        }
    }

    fn item() -> DownloadClientItem {
        DownloadClientItem::new(
            ItemClientInfo {
                protocol: Protocol::Usenet,
                kind: ClientKind::Sabnzbd,
                id: 1,
                name: "SABnzbd".into(),
                remove_completed_downloads: true,
                has_post_import_category: false,
            },
            "DownloadId1",
            "Series.Title.S01E01.720p.HDTV-Sonarr",
        )
    }

    fn history(id: i64, episode_id: i64, event: HistoryEvent, date: DateTime<Utc>) -> HistoryEntry {
        HistoryEntry {
            id,
            series_id: 1,
            episode_id,
            source_title: None,
            date,
            event_type: event,
            quality: quality(HDTV720P, 1),
            languages: vec![1],
            data: json!({}),
            download_id: Some("DownloadId1".into()),
        }
    }

    /// `30.rock.s01e01.avi` under the unsorted folder, one episode of season
    /// one that aired just now, as most fixtures start from.
    fn local(h: &Harness) -> LocalEpisode {
        let series = testing::series(1, "30 Rock", 1);
        let mut episode = testing::episode(1, 1, 1, 1);
        episode.air_date_utc = Some(h.now);
        LocalEpisode {
            path: "/Test/Unsorted/30 Rock/30.rock.s01e01.avi".into(),
            size: 100,
            series: Some(series),
            episodes: vec![episode],
            quality: quality(HDTV720P, 1),
            languages: vec![4],
            ..Default::default()
        }
    }

    // AbsoluteEpisodeNumberSpecificationFixture

    fn anime() -> (Harness, LocalEpisode) {
        let mut h = Harness::new();
        h.naming.anime_episode_format = "{Series Title} - {absolute:000} - {Episode Title}".into();
        let mut l = local(&h);
        l.series.as_mut().unwrap().series_type = SeriesType::Anime;
        l.episodes[0].absolute_episode_number = None;
        (h, l)
    }

    #[test]
    fn absolute_number_rejects_when_missing() {
        let (h, l) = anime();
        let r = h.run(absolute_episode_number, &l, None).unwrap();
        assert_eq!(r.reason, R::MissingAbsoluteEpisodeNumber);
    }

    #[test]
    fn absolute_number_accepts_when_not_recent() {
        let (h, mut l) = anime();
        l.episodes[0].air_date_utc = Some(h.now - Duration::days(7));
        assert!(h.accepts(absolute_episode_number, &l, None));
    }

    #[test]
    fn absolute_number_accepts_when_not_required() {
        let (mut h, l) = anime();
        h.naming.anime_episode_format = NamingConfig::default().anime_episode_format;
        assert!(h.accepts(absolute_episode_number, &l, None));
    }

    // AlreadyImportedSpecificationFixture

    #[test]
    fn already_imported_accepts_without_a_client_item() {
        let h = Harness::new();
        let mut l = local(&h);
        l.episodes[0].episode_file_id = 1;
        assert!(h.accepts(already_imported, &l, None));
    }

    #[test]
    fn already_imported_accepts_an_episode_without_a_file() {
        let h = Harness::new();
        let l = local(&h);
        assert!(h.accepts(already_imported, &l, Some(&item())));
    }

    #[test]
    fn already_imported_accepts_when_never_imported() {
        let mut h = Harness::new();
        h.lookup.history = vec![history(1, 1, HistoryEvent::Grabbed, h.now)];
        let mut l = local(&h);
        l.episodes[0].episode_file_id = 1;
        assert!(h.accepts(already_imported, &l, Some(&item())));
    }

    #[test]
    fn already_imported_accepts_when_grabbed_again_after_the_import() {
        let mut h = Harness::new();
        h.lookup.history = vec![
            history(1, 1, HistoryEvent::Grabbed, h.now),
            history(
                2,
                1,
                HistoryEvent::DownloadFolderImported,
                h.now - Duration::days(1),
            ),
            history(3, 1, HistoryEvent::Grabbed, h.now - Duration::days(2)),
        ];
        let mut l = local(&h);
        l.episodes[0].episode_file_id = 1;
        assert!(h.accepts(already_imported, &l, Some(&item())));
    }

    #[test]
    fn already_imported_rejects_when_imported_after_the_grab() {
        let mut h = Harness::new();
        h.lookup.history = vec![
            history(
                1,
                1,
                HistoryEvent::DownloadFolderImported,
                h.now - Duration::days(1),
            ),
            history(2, 1, HistoryEvent::Grabbed, h.now - Duration::days(2)),
        ];
        let mut l = local(&h);
        l.episodes[0].episode_file_id = 1;
        let r = h.run(already_imported, &l, Some(&item())).unwrap();
        assert_eq!(r.reason, R::EpisodeAlreadyImported);
        assert!(r.message.starts_with("Episode file already imported at "));
    }

    // EpisodeTitleSpecificationFixture

    fn titled(h: &mut Harness) -> LocalEpisode {
        h.naming.rename_episodes = true;
        local(h)
    }

    #[test]
    fn episode_title_rejects_a_missing_title() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        l.episodes[0].title = None;
        assert_eq!(h.run(episode_title, &l, None).unwrap().reason, R::TitleMissing);
    }

    #[test]
    fn episode_title_rejects_tba() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        l.episodes[0].title = Some("TBA".into());
        assert_eq!(h.run(episode_title, &l, None).unwrap().reason, R::TitleTba);
    }

    #[test]
    fn episode_title_accepts_a_file_already_in_the_series_folder() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        l.existing_file = true;
        l.episodes[0].title = Some("TBA".into());
        assert!(h.accepts(episode_title, &l, None));
    }

    #[test]
    fn episode_title_accepts_tba_when_not_recent() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        l.episodes[0].air_date_utc = Some(h.now - Duration::days(7));
        l.episodes[0].title = Some("TBA".into());
        assert!(h.accepts(episode_title, &l, None));
    }

    #[test]
    fn episode_title_accepts_when_the_format_has_no_title() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        h.naming.standard_episode_format = "{Series Title} - S{season:00}E{episode:00}".into();
        l.episodes[0].title = Some("TBA".into());
        assert!(h.accepts(episode_title, &l, None));
    }

    #[test]
    fn episode_title_accepts_when_never_required() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        h.config.episode_title_required = EpisodeTitleRequired::Never;
        l.episodes[0].title = None;
        assert!(h.accepts(episode_title, &l, None));
    }

    fn season_episodes(h: &Harness, same_day: bool) -> Vec<Episode> {
        (1..=5)
            .map(|n| {
                let mut e = testing::episode(10 + n, 1, 1, n);
                e.air_date_utc = if same_day {
                    Some(h.now)
                } else {
                    Some(h.now - Duration::days(n))
                };
                e
            })
            .collect()
    }

    #[test]
    fn episode_title_for_bulk_releases_accepts_a_single_release() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        h.config.episode_title_required = EpisodeTitleRequired::BulkSeasonReleases;
        h.lookup.episodes = season_episodes(&h, false);
        l.episodes[0].title = None;
        assert!(h.accepts(episode_title, &l, None));
    }

    #[test]
    fn episode_title_for_bulk_releases_accepts_with_a_title() {
        let mut h = Harness::new();
        let l = titled(&mut h);
        h.config.episode_title_required = EpisodeTitleRequired::BulkSeasonReleases;
        h.lookup.episodes = season_episodes(&h, true);
        assert!(h.accepts(episode_title, &l, None));
    }

    #[test]
    fn episode_title_for_bulk_releases_rejects_tba() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        h.config.episode_title_required = EpisodeTitleRequired::BulkSeasonReleases;
        h.lookup.episodes = season_episodes(&h, true);
        l.episodes[0].title = Some("TBA".into());
        assert_eq!(h.run(episode_title, &l, None).unwrap().reason, R::TitleTba);
    }

    #[test]
    fn episode_title_for_bulk_releases_counts_episodes_without_an_air_date() {
        let mut h = Harness::new();
        let mut l = titled(&mut h);
        h.config.episode_title_required = EpisodeTitleRequired::BulkSeasonReleases;
        let mut episodes = season_episodes(&h, true);
        for (i, e) in episodes.iter_mut().enumerate() {
            e.title = Some("TBA".into());
            if i > 0 {
                e.air_date_utc = None;
            }
        }
        h.lookup.episodes = episodes;
        l.episodes[0].title = Some("TBA".into());
        assert_eq!(h.run(episode_title, &l, None).unwrap().reason, R::TitleTba);
    }

    // FreeSpaceSpecificationFixture

    const MB: i64 = 1024 * 1024;

    #[test]
    fn free_space_rejects_when_the_disk_is_short() {
        let mut h = Harness::new();
        h.config.minimum_free_space_when_importing_mb = 0;
        h.lookup.free_space.insert("/tv".into(), 80 * MB);
        let mut l = local(&h);
        l.size = 100 * MB;
        assert_eq!(h.run(free_space, &l, None).unwrap().reason, R::MinimumFreeSpace);
    }

    #[test]
    fn free_space_counts_the_configured_margin() {
        let mut h = Harness::new();
        h.config.minimum_free_space_when_importing_mb = 100;
        h.lookup.free_space.insert("/tv".into(), 150 * MB);
        let mut l = local(&h);
        l.size = 100 * MB;
        assert_eq!(h.run(free_space, &l, None).unwrap().reason, R::MinimumFreeSpace);
    }

    #[test]
    fn free_space_accepts_with_room_measured_at_the_series_parent() {
        let mut h = Harness::new();
        h.lookup.free_space.insert("/tv".into(), 1024 * MB);
        let mut l = local(&h);
        l.size = 100 * MB;
        assert!(h.accepts(free_space, &l, None));
    }

    #[test]
    fn free_space_passes_when_unknown_or_skipped() {
        let mut h = Harness::new();
        let mut l = local(&h);
        l.size = 100 * MB;
        assert!(h.accepts(free_space, &l, None), "no free space reading");
        h.lookup.free_space.insert("/tv".into(), 1);
        l.existing_file = true;
        assert!(h.accepts(free_space, &l, None), "a file already in the library");
        l.existing_file = false;
        h.config.skip_free_space_check_when_importing = true;
        assert!(h.accepts(free_space, &l, None), "the check is skipped");
    }

    // FullSeasonSpecificationFixture

    #[test]
    fn full_season_only_rejects_a_season_pack_file() {
        let h = Harness::new();
        let mut l = local(&h);
        assert!(h.accepts(full_season, &l, None), "no file info");
        l.file_episode_info = parser::parse_title("Series.Title.S01.720p.HDTV-Sonarr");
        assert_eq!(h.run(full_season, &l, None).unwrap().reason, R::FullSeason);
        l.file_episode_info = parser::parse_title("Series.Title.S01E01.720p.HDTV-Sonarr");
        assert!(h.accepts(full_season, &l, None));
    }

    // HasAudioTrackSpecificationFixture

    #[test]
    fn audio_track_rejects_only_a_probed_file_without_audio() {
        let h = Harness::new();
        let mut l = local(&h);
        assert!(h.accepts(has_audio_track, &l, None), "no media info");
        l.media_info = Some(MediaInfo {
            model: json!({"audioStreamCount": 0}),
            title: None,
        });
        assert_eq!(h.run(has_audio_track, &l, None).unwrap().reason, R::NoAudio);
        l.media_info = Some(MediaInfo {
            model: json!({"audioStreamCount": 1}),
            title: None,
        });
        assert!(h.accepts(has_audio_track, &l, None));
    }

    // MatchesFolderSpecificationFixture

    fn in_folder(h: &mut Harness, folder: &str, file_name: &str) -> LocalEpisode {
        let series = testing::series(1, "Series Title", 1);
        h.lookup.series = vec![series.clone()];
        h.lookup.episodes = (1..=6).map(|n| testing::episode(10 + n, 1, 1, n)).collect();
        LocalEpisode {
            path: format!("/Test/Unsorted/{folder}/{file_name}"),
            file_episode_info: parser::parse_title(file_name),
            folder_episode_info: parser::parse_title(folder),
            series: Some(series),
            ..Default::default()
        }
    }

    #[test]
    fn matches_folder_accepts_an_existing_file_and_unparsed_names() {
        let mut h = Harness::new();
        let mut l = in_folder(&mut h, "Series.Title.S01E01.720p.HDTV-Sonarr", "S01E05.mkv");
        l.existing_file = true;
        assert!(h.accepts(matches_folder, &l, None));
        l.existing_file = false;
        l.folder_episode_info = None;
        assert!(h.accepts(matches_folder, &l, None), "folder not parseable");
        let mut l = in_folder(&mut h, "Series.Title.S01E01", "AFDAFD.mkv");
        l.file_episode_info = None;
        assert!(h.accepts(matches_folder, &l, None), "file not parseable");
    }

    #[test]
    fn matches_folder_accepts_a_season_pack_and_matching_episodes() {
        let mut h = Harness::new();
        let l = in_folder(&mut h, "Series.Title.S01", "Series.Title.S01E01.mkv");
        assert!(h.accepts(matches_folder, &l, None), "full season folder");
        let l = in_folder(
            &mut h,
            "Series.Title.S01E01.720p.HDTV-Sonarr",
            "Series.Title.S01E01.mkv",
        );
        assert!(h.accepts(matches_folder, &l, None), "same episode");
        let l = in_folder(
            &mut h,
            "Series.Title.S01E01E02.720p.HDTV-Sonarr",
            "Series.Title.S01E01.mkv",
        );
        assert!(
            h.accepts(matches_folder, &l, None),
            "one episode of the folder's two"
        );
    }

    #[test]
    fn matches_folder_rejects_episodes_the_folder_does_not_name() {
        let mut h = Harness::new();
        let l = in_folder(
            &mut h,
            "Series.Title.S01E01.720p.HDTV-Sonarr",
            "Series.Title.S01E05.mkv",
        );
        let r = h.run(matches_folder, &l, None).unwrap();
        assert_eq!(r.reason, R::EpisodeUnexpected);
        assert_eq!(
            r.message,
            "Episode 1x05 was unexpected considering the Series.Title.S01E01.720p.HDTV-Sonarr folder name"
        );
        let l = in_folder(
            &mut h,
            "Series.Title.S01E01E02.720p.HDTV-Sonarr",
            "Series.Title.S01E05E06.mkv",
        );
        let r = h.run(matches_folder, &l, None).unwrap();
        assert!(r.message.starts_with("Episodes 1x05, 1x06 were unexpected"));
    }

    // MatchesGrabSpecificationFixture

    fn grabbed(ids: &[i64]) -> GrabbedRelease {
        GrabbedRelease {
            title: "Series.Title.S01E01.720p.HDTV-Sonarr".into(),
            indexer: None,
            size: 0,
            release_type: Default::default(),
            episode_ids: ids.to_vec(),
        }
    }

    #[test]
    fn matches_grab_accepts_existing_files_and_grabs_without_episodes() {
        let h = Harness::new();
        let mut l = local(&h);
        l.existing_file = true;
        l.release = Some(grabbed(&[2]));
        assert!(h.accepts(matches_grab, &l, Some(&item())));
        l.existing_file = false;
        l.release = None;
        assert!(h.accepts(matches_grab, &l, Some(&item())), "no grabbed release");
        l.release = Some(grabbed(&[]));
        assert!(
            h.accepts(matches_grab, &l, Some(&item())),
            "a grab without episodes"
        );
    }

    #[test]
    fn matches_grab_accepts_the_grabbed_episode_and_rejects_another() {
        let h = Harness::new();
        let mut l = local(&h);
        l.release = Some(grabbed(&[1]));
        assert!(h.accepts(matches_grab, &l, Some(&item())));
        l.release = Some(grabbed(&[1, 2]));
        assert!(h.accepts(matches_grab, &l, Some(&item())));
        l.release = Some(grabbed(&[2]));
        let r = h.run(matches_grab, &l, Some(&item())).unwrap();
        assert_eq!(r.reason, R::EpisodeNotFoundInRelease);
        assert_eq!(
            r.message,
            "Episode 1x01 was not found in the grabbed release: Series.Title.S01E01.720p.HDTV-Sonarr"
        );
        l.release = Some(grabbed(&[2, 3]));
        assert!(h.run(matches_grab, &l, Some(&item())).is_some());
    }

    // NotSampleSpecificationFixture

    #[test]
    fn not_sample_accepts_existing_and_stream_files_and_rejects_an_unprobed_video() {
        let h = Harness::new();
        let mut l = local(&h);
        l.existing_file = true;
        assert!(h.accepts(not_sample, &l, None));
        l.existing_file = false;
        l.path = "/Test/Unsorted/30 Rock/30.rock.s01e01.strm".into();
        assert!(h.accepts(not_sample, &l, None), "a stream file is never a sample");
        l.path = "/Test/Unsorted/30 Rock/30.rock.s01e01.mkv".into();
        assert_eq!(
            h.run(not_sample, &l, None).unwrap().reason,
            R::SampleIndeterminate
        );
    }

    // NotUnpackingSpecificationFixture

    #[test]
    fn not_unpacking_rejects_a_file_under_a_working_folder() {
        let mut h = Harness::new();
        h.config.download_client_working_folders = "_UNPACK_|_FAILED_".into();
        let mut l = local(&h);
        l.path = "/Test/Unsorted TV/30.rock/30.rock.s01e01.avi".into();
        assert!(h.accepts(not_unpacking, &l, None));
        l.path = "/Test/Unsorted TV/_UNPACK_30.rock/someSubFolder/30.rock.s01e01.avi".into();
        assert_eq!(h.run(not_unpacking, &l, None).unwrap().reason, R::Unpacking);
        l.existing_file = true;
        assert!(h.accepts(not_unpacking, &l, None));
    }

    // SameEpisodesSpecificationFixture (DecisionEngineTests): the rule the
    // import specification shares with the grab one.

    /// Two episodes of one season; `file_ids` gives each its file.
    fn two_episodes(file_ids: [i64; 2]) -> Vec<Episode> {
        (0..2)
            .map(|i| {
                let mut e = testing::episode(1 + i, 1, 1, 1 + i);
                e.episode_file_id = file_ids[i as usize];
                e
            })
            .collect()
    }

    #[test]
    fn same_episodes_rejects_a_file_with_fewer_episodes_than_on_disk() {
        let mut h = Harness::new();
        let episodes = two_episodes([1, 1]);
        h.lookup.episodes = episodes.clone();
        let mut l = local(&h);
        l.episodes = vec![episodes[0].clone()];
        let r = h.run(same_episodes_import, &l, None).unwrap();
        assert_eq!(r.reason, R::ExistingFileHasMoreEpisodes);
    }

    #[test]
    fn same_episodes_accepts_a_file_with_more_episodes_than_on_disk() {
        let mut h = Harness::new();
        let episodes = two_episodes([1, 0]);
        h.lookup.episodes = vec![episodes[0].clone()];
        let mut l = local(&h);
        l.episodes = episodes;
        assert!(h.accepts(same_episodes_import, &l, None));
    }

    #[test]
    fn same_episodes_accepts_the_same_episodes() {
        let mut h = Harness::new();
        let episodes = two_episodes([1, 1]);
        h.lookup.episodes = episodes.clone();
        let mut l = local(&h);
        l.episodes = episodes;
        assert!(h.accepts(same_episodes_import, &l, None));
    }

    #[test]
    fn same_episodes_accepts_the_same_episodes_held_in_separate_files() {
        let mut h = Harness::new();
        let episodes = two_episodes([1, 2]);
        h.lookup.episodes = episodes.clone();
        let mut l = local(&h);
        l.episodes = episodes;
        assert!(h.accepts(same_episodes_import, &l, None));
    }

    // SplitEpisodeSpecification and UnverifiedSceneNumberingSpecification

    #[test]
    fn split_episodes_and_unverified_scene_numbering_are_rejected() {
        let h = Harness::new();
        let mut l = local(&h);
        l.file_episode_info = parser::parse_title("Series.Title.S01E01a.720p.HDTV-Sonarr");
        assert!(l.file_episode_info.as_ref().unwrap().is_split_episode);
        assert_eq!(h.run(split_episode, &l, None).unwrap().reason, R::SplitEpisode);
        l.episodes[0].unverified_scene_numbering = true;
        assert_eq!(
            h.run(unverified_scene_numbering, &l, None).unwrap().reason,
            R::UnverifiedSceneMapping
        );
        l.existing_file = true;
        assert!(h.accepts(unverified_scene_numbering, &l, None));
    }

    // UpgradeSpecificationFixture

    fn with_file(h: &mut Harness, l: &mut LocalEpisode, q: QualityModel) {
        h.lookup.files.push(file(1, q, "30 Rock - S01E01.mkv"));
        for e in &mut l.episodes {
            e.episode_file_id = 1;
        }
    }

    #[test]
    fn upgrade_accepts_without_an_existing_file() {
        let h = Harness::new();
        let mut l = local(&h);
        assert!(h.accepts(upgrade, &l, None));
        l.episodes.push(testing::episode(2, 1, 1, 2));
        assert!(h.accepts(upgrade, &l, None), "multi-episode");
        l.episodes[0].episode_file_id = 99;
        assert!(h.accepts(upgrade, &l, None), "a file the store no longer has");
    }

    #[test]
    fn upgrade_accepts_a_better_or_equal_quality() {
        let mut h = Harness::new();
        let mut l = local(&h);
        with_file(&mut h, &mut l, quality(SDTV, 1));
        assert!(h.accepts(upgrade, &l, None));
        h.lookup.files[0].quality = quality(HDTV720P, 1);
        assert!(h.accepts(upgrade, &l, None), "same quality, other language");
    }

    #[test]
    fn upgrade_rejects_a_worse_quality() {
        let mut h = Harness::new();
        let mut l = local(&h);
        with_file(&mut h, &mut l, quality(BLURAY1080P, 1));
        let r = h.run(upgrade, &l, None).unwrap();
        assert_eq!(r.reason, R::NotQualityUpgrade);
        assert_eq!(
            r.message,
            "Not an upgrade for existing episode file(s). Existing quality: Bluray-1080p. New Quality HDTV-720p."
        );
        // One of two episodes already has a better file.
        let mut l = local(&h);
        l.episodes.push(testing::episode(2, 1, 1, 2));
        l.episodes[1].episode_file_id = 1;
        assert!(h.run(upgrade, &l, None).is_some());
    }

    #[test]
    fn upgrade_rejects_a_lower_revision_only_when_propers_are_preferred() {
        let mut h = Harness::new();
        let mut l = local(&h);
        with_file(&mut h, &mut l, quality(HDTV720P, 2));
        h.catalog.config.download_propers_and_repacks = ProperDownloadTypes::PreferAndUpgrade;
        assert_eq!(h.run(upgrade, &l, None).unwrap().reason, R::NotRevisionUpgrade);
        h.catalog.config.download_propers_and_repacks = ProperDownloadTypes::DoNotPrefer;
        assert!(h.accepts(upgrade, &l, None));
        // A proper of a lower quality does not beat a higher quality.
        h.catalog.config.download_propers_and_repacks = ProperDownloadTypes::PreferAndUpgrade;
        h.lookup.files[0].quality = quality(BLURAY720P, 2);
        l.quality = quality(BLURAY1080P, 1);
        assert!(h.accepts(upgrade, &l, None));
    }

    fn with_format_scores(h: &mut Harness, existing_score: i64) {
        // The existing file's name matches the one custom format.
        h.catalog.formats = vec![CustomFormat {
            id: 1,
            name: "Group".into(),
            include_when_renaming: false,
            specifications: json!([{"type": "ReleaseTitleSpecification", "body": {"value": "GRP", "negate": false, "required": false}}]),
        }];
        h.catalog.profiles[0].format_items = vec![ProfileFormatItem {
            format: 1,
            score: existing_score,
        }];
        h.catalog.config.download_propers_and_repacks = ProperDownloadTypes::DoNotPrefer;
        h.lookup
            .files
            .push(file(1, quality(BLURAY1080P, 1), "30 Rock - S01E01-GRP.mkv"));
    }

    #[test]
    fn upgrade_compares_custom_format_scores_at_equal_quality() {
        let mut h = Harness::new();
        with_format_scores(&mut h, 10);
        let mut l = local(&h);
        l.episodes[0].episode_file_id = 1;
        l.quality = quality(BLURAY1080P, 1);
        l.custom_format_score = 20;
        assert!(h.accepts(upgrade, &l, None), "20 beats 10");

        let mut h = Harness::new();
        with_format_scores(&mut h, 50);
        h.lookup.files[0].quality = quality(BLURAY720P, 1);
        assert!(
            h.accepts(upgrade, &l, None),
            "a quality upgrade with a lower score"
        );

        let mut h = Harness::new();
        with_format_scores(&mut h, 50);
        let r = h.run(upgrade, &l, None).unwrap();
        assert_eq!(r.reason, R::NotCustomFormatUpgrade);
        assert_eq!(
            r.message,
            "Not a Custom Format upgrade for existing episode file(s). New: [] (20) do not improve on Existing: [Group] (50)"
        );
    }

    #[test]
    fn an_existing_file_passes_every_specification() {
        let h = Harness::new();
        let mut l = local(&h);
        l.existing_file = true;
        let ctx = Context {
            lookup: &h.lookup,
            catalog: &h.catalog,
            config: &h.config,
            naming: &h.naming,
            now: h.now,
        };
        assert!(evaluate(&ctx, &l, None).is_empty());
    }
}
