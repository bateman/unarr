//! Radarr's import specifications (`MediaFiles/MovieImport/Specifications`),
//! each a function from a local movie and its download client item to a
//! rejection or nothing, run in the original's order with its messages.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::local::{LocalMovie, MovieImportRejection, MovieRejectionReason as R};
use super::sample;
use crate::decision::lookup::ProperDownloadTypes;
use crate::decision::upgradable;
use crate::downloadclients::DownloadClientItem;
use crate::mediafiles::sample::DetectSampleResult;
use crate::mediafiles::specs::local_time;
use crate::mediafiles::{file_name, parent_path};
use crate::model::CustomFormat;
use crate::movies::decision::engine::file_formats;
use crate::movies::decision::specs::{movie_string, quality_string};
use crate::movies::decision::{MovieCatalog, MovieLookup};
use crate::movies::model::{MovieHistoryEntry, MovieHistoryEvent};
use crate::movies::parser::quality::UNKNOWN as UNKNOWN_QUALITY_ID;
use crate::movies::quality::MovieQuality;
use crate::movies::store;
use crate::net_regex;
use crate::parser::netregex::Lazy;
use crate::store::config::MediaManagementConfig;

/// What the specifications ask besides the library: the history of a
/// download and the files of a folder.
pub trait ImportSources {
    /// `HistoryService.FindByDownloadId`.
    fn history_by_download_id(&self, download_id: &str) -> Vec<MovieHistoryEntry>;
    /// `DiskProvider.GetFiles(path, false)`: the full paths of the files
    /// directly in a folder.
    fn files_in(&self, folder: &str) -> Vec<String>;
}

/// The history in the database and the folders on the disk.
pub struct StoreImportSources<'a> {
    pub conn: &'a Connection,
}

impl ImportSources for StoreImportSources<'_> {
    fn history_by_download_id(&self, download_id: &str) -> Vec<MovieHistoryEntry> {
        store::history_by_download_id(self.conn, download_id).unwrap_or_else(|e| {
            tracing::warn!("history lookup failed: {e:#}");
            Vec::new()
        })
    }

    fn files_in(&self, folder: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return Vec::new();
        };
        let mut files: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();
        files.sort();
        files
    }
}

/// Everything the specifications consult besides the file.
pub struct Context<'a> {
    pub lookup: &'a dyn MovieLookup,
    pub sources: &'a dyn ImportSources,
    pub catalog: &'a MovieCatalog,
    pub config: &'a MediaManagementConfig,
    pub now: DateTime<Utc>,
}

type Spec = fn(&Context<'_>, &LocalMovie, Option<&DownloadClientItem>) -> Option<MovieImportRejection>;

/// The specifications in the order the original registers them: that of
/// their source files in `MovieImport/Specifications`.
pub const SPECIFICATIONS: &[(&str, Spec)] = &[
    ("AlreadyImportedSpecification", already_imported),
    ("FreeSpaceSpecification", free_space),
    ("GrabbedReleaseQualitySpecification", grabbed_release_quality),
    ("HasAudioTrackSpecification", has_audio_track),
    ("MatchesFolderSpecification", matches_folder),
    ("MatchesGrabSpecification", matches_grab),
    ("NotMultiPartSpecification", not_multi_part),
    ("NotSampleSpecification", not_sample),
    ("NotUnpackingSpecification", not_unpacking),
    ("UpgradeSpecification", upgrade),
];

fn reject(reason: R, message: impl Into<String>) -> Option<MovieImportRejection> {
    Some(MovieImportRejection::new(reason, message))
}

/// `ConcatToString` of custom formats: their names.
fn format_names(formats: &[CustomFormat]) -> String {
    formats
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The download was imported already, and not grabbed again since.
fn already_imported(
    ctx: &Context<'_>,
    local: &LocalMovie,
    item: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    let Some(item) = item else {
        tracing::debug!("No download client information is available, skipping");
        return None;
    };
    let movie = local.movie.as_ref()?;
    if movie.movie_file_id <= 0 {
        tracing::debug!("Skipping already imported check for movie without file");
        return None;
    }
    let history = ctx.lookup.history(movie.id);
    let of_download = |event: MovieHistoryEvent| {
        history
            .iter()
            .find(|h| h.download_id.as_deref() == Some(item.download_id.as_str()) && h.event_type == event)
    };
    let last_imported = of_download(MovieHistoryEvent::DownloadFolderImported);
    let last_grabbed = of_download(MovieHistoryEvent::Grabbed);
    let Some(imported) = last_imported else {
        tracing::trace!("Movie file has not been imported");
        return None;
    };
    // `After` is "not before": a grab at the very time of the import
    // counts as a grab made again.
    if last_grabbed.is_some_and(|grabbed| grabbed.date >= imported.date) {
        tracing::trace!("Movie file was grabbed again after importing");
        return None;
    }
    tracing::debug!("Movie file previously imported at {}", imported.date);
    reject(
        R::MovieAlreadyImported,
        format!("Movie file already imported at {}", local_time(imported.date)),
    )
}

/// The root folder must have room for the file plus the configured margin.
fn free_space(
    ctx: &Context<'_>,
    local: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    if ctx.config.skip_free_space_check_when_importing {
        tracing::debug!("Skipping free space check when importing");
        return None;
    }
    if local.existing_file {
        tracing::debug!("Skipping free space check for existing movie");
        return None;
    }
    let movie = local.movie.as_ref()?;
    let parent = parent_path(&movie.path)?;
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

/// The quality the grab recorded against the file's: only logged, the
/// original no longer rejects on it.
fn grabbed_release_quality(
    ctx: &Context<'_>,
    local: &LocalMovie,
    item: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    let item = item?;
    let grabbed: Vec<MovieHistoryEntry> = ctx
        .sources
        .history_by_download_id(&item.download_id)
        .into_iter()
        .filter(|h| h.event_type == MovieHistoryEvent::Grabbed)
        .collect();
    if grabbed.is_empty() {
        tracing::debug!("No grabbed history for this download client item");
        return None;
    }
    for history in &grabbed {
        if history.quality.quality_id != UNKNOWN_QUALITY_ID && history.quality != local.quality {
            // Log only for info, spec removed due to common webdl/webrip mismatches
            tracing::debug!(
                "Quality for grabbed release ({}) does not match the quality of the file ({})",
                quality_string(&history.quality),
                quality_string(&local.quality)
            );
        }
    }
    None
}

fn has_audio_track(
    _: &Context<'_>,
    local: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    let Some(info) = &local.media_info else {
        tracing::debug!(
            "Failed to get media info from the file, make sure ffprobe is available, skipping check"
        );
        return None;
    };
    if info.audio_stream_count() == 0 {
        tracing::debug!("No audio tracks found in file");
        return reject(R::NoAudio, "No audio tracks detected");
    }
    None
}

/// The original accepts every file: what it would compare the folder's
/// name with was never written.
fn matches_folder(
    _: &Context<'_>,
    _: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    None
}

/// A file of a movie the grab was not for.
fn matches_grab(
    _: &Context<'_>,
    local: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    if local.existing_file {
        return None;
    }
    let release = local.release.as_ref()?;
    if release.movie_ids.is_empty() {
        return None;
    }
    let movie = local.movie.as_ref()?;
    if release.movie_ids.iter().all(|id| *id != movie.id) {
        let movie = movie_string(movie);
        tracing::debug!("Unexpected movie(s) in file: {movie}");
        return reject(
            R::MovieNotFoundInRelease,
            format!(
                "Movie {movie} was not found in the grabbed release: {}",
                release.title
            ),
        );
    }
    None
}

/// `MovieMultiPartRegex`: `cd1`, `dvd 2`, `pt3`, `part.4`, `disc5` and
/// `cd a`, `part b`...
static MULTI_PART_RES: [Lazy; 2] = [
    net_regex!(r"(?<!^)(?<identifier>[ _.-]*(?:cd|dvd|p(?:(?:ar)?t)?|dis[ck])[ _.-]*[0-9]+)"),
    net_regex!(r"(?<!^)(?<identifier>[ _.-]*(?:cd|dvd|p(?:(?:ar)?t)?|dis[ck])[ _.-]*[a-d]+)"),
];

/// A file whose folder holds another part of the same name.
fn not_multi_part(
    ctx: &Context<'_>,
    local: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    if !MULTI_PART_RES.iter().any(|re| re.is_match(&local.path)) {
        return None;
    }
    let files = ctx.sources.files_in(parent_path(&local.path).unwrap_or(""));
    for re in &MULTI_PART_RES {
        let own = re.replace_all(&local.path, "");
        if files.iter().filter(|f| re.replace_all(f, "") == own).count() > 1 {
            tracing::debug!("Rejected Multi-Part File: {}", local.path);
            return reject(
                R::MultiPartMovie,
                "File is suspected multi-part file, Radarr doesn't support this",
            );
        }
    }
    None
}

fn not_sample(
    _: &Context<'_>,
    local: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    if local.existing_file {
        tracing::debug!("Existing file, skipping sample check");
        return None;
    }
    let movie = local.movie.as_ref()?;
    match sample::is_sample(&movie.metadata, &local.path, local.media_info.as_ref()) {
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
    local: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    if local.existing_file {
        tracing::debug!("{} is in movie folder, skipping unpacking check", local.path);
        return None;
    }
    for working in ctx.config.download_client_working_folders.split('|') {
        if working.is_empty() {
            continue;
        }
        let mut parent = parent_path(&local.path);
        while let Some(p) = parent {
            if file_name(p).starts_with(working) {
                tracing::debug!("{} is still being unpacked", local.path);
                return reject(R::Unpacking, "File is still being unpacked");
            }
            if p == "/" {
                break;
            }
            parent = parent_path(p);
        }
    }
    None
}

/// The file must be at least as good as the one the movie already has.
fn upgrade(
    ctx: &Context<'_>,
    local: &LocalMovie,
    _: Option<&DownloadClientItem>,
) -> Option<MovieImportRejection> {
    use std::cmp::Ordering;
    let movie = local.movie.as_ref()?;
    let profile = ctx.catalog.base.profile(movie.quality_profile_id)?;
    let propers = ctx.catalog.base.config.download_propers_and_repacks;
    if movie.movie_file_id <= 0 {
        return None;
    }
    let Some(file) = ctx.lookup.movie_file(movie.movie_file_id) else {
        tracing::trace!(
            "Unable to get movie file details from the DB. MovieId: {} MovieFileId: {}",
            movie.id,
            movie.movie_file_id
        );
        return None;
    };
    let name = |quality_id: i64| MovieQuality::by_id(quality_id).name;
    let compare = upgradable::compare_quality(profile, local.quality.quality_id, file.quality.quality_id);
    if compare == Ordering::Less {
        tracing::debug!(
            "This file isn't a quality upgrade for movie. Existing quality: {}. New Quality {}. Skipping {}",
            name(file.quality.quality_id),
            name(local.quality.quality_id),
            local.path
        );
        return reject(
            R::NotQualityUpgrade,
            format!(
                "Not an upgrade for existing movie file. Existing quality: {}. New Quality {}.",
                name(file.quality.quality_id),
                name(local.quality.quality_id)
            ),
        );
    }
    // Same quality, propers/repacks are preferred and it is not a revision
    // update. Reject revision downgrade.
    if compare == Ordering::Equal
        && propers != ProperDownloadTypes::DoNotPrefer
        && upgradable::compare_revision(&local.quality, &file.quality) == Ordering::Less
    {
        tracing::debug!(
            "This file isn't a quality revision upgrade for movie. Skipping {}",
            local.path
        );
        return reject(
            R::NotRevisionUpgrade,
            "Not a quality revision upgrade for existing movie file(s)",
        );
    }
    let current_formats = file_formats(ctx.catalog, &file, movie);
    let current_score = upgradable::format_score(profile, &current_formats);
    let new_score = local.custom_format_score;
    if compare == Ordering::Equal && new_score < current_score {
        tracing::debug!(
            "New item's custom formats [{}] ({new_score}) do not improve on [{}] ({current_score}), skipping",
            format_names(&local.custom_formats),
            format_names(&current_formats)
        );
        // Radarr 7: the name the file had did improve on the existing one;
        // the name the import gives it does not.
        let score_before_rename = local.original_file_name_custom_format_score;
        if score_before_rename > current_score {
            return reject(
                R::NotCustomFormatUpgradeAfterRename,
                format!(
                    "Not a Custom Format upgrade for existing movie file(s). AfterRename: [{}] ({new_score}) do not improve on Existing: [{}] ({current_score}) even though BeforeRename: [{}] ({score_before_rename}) did.",
                    format_names(&local.custom_formats),
                    format_names(&current_formats),
                    format_names(&local.original_file_name_custom_formats)
                ),
            );
        }
        return reject(
            R::NotCustomFormatUpgrade,
            format!(
                "Not a Custom Format upgrade for existing movie file(s). New: [{}] ({new_score}) do not improve on Existing: [{}] ({current_score})",
                format_names(&local.custom_formats),
                format_names(&current_formats)
            ),
        );
    }
    tracing::debug!(
        "New item's custom formats [{}] ({new_score}) do improve on [{}] ({current_score}), accepting",
        format_names(&local.custom_formats),
        format_names(&current_formats)
    );
    None
}

/// Every specification against one file; the rejections in order.
pub fn evaluate(
    ctx: &Context<'_>,
    local: &LocalMovie,
    item: Option<&DownloadClientItem>,
) -> Vec<MovieImportRejection> {
    let mut rejections = Vec::new();
    for (_, spec) in SPECIFICATIONS {
        if let Some(r) = spec(ctx, local, item) {
            rejections.push(r);
        }
    }
    rejections
}

#[cfg(test)]
pub(crate) mod testing {
    //! A library, a history and a disk held in memory, for the tests of
    //! the specifications and of the decision maker.

    use std::cell::RefCell;

    use chrono::{DateTime, Utc};

    use super::ImportSources;
    use crate::movies::decision::{LibraryMovie, MovieLookup};
    use crate::movies::model::{Movie, MovieBlocklistEntry, MovieFile, MovieHistoryEntry};

    #[derive(Debug, Default)]
    pub(crate) struct Library {
        pub files: Vec<MovieFile>,
        /// The history rows, of every movie and download.
        pub history: Vec<MovieHistoryEntry>,
        pub free_space: Option<i64>,
        /// The paths `free_space` was asked about.
        pub free_space_asked: RefCell<Vec<String>>,
        /// The files on the disk, by full path.
        pub disk: Vec<String>,
    }

    fn newest_first(mut rows: Vec<MovieHistoryEntry>) -> Vec<MovieHistoryEntry> {
        rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
        rows
    }

    impl MovieLookup for Library {
        fn find_by_titles(&self, _: &[String]) -> Vec<LibraryMovie> {
            Vec::new()
        }

        fn find_by_imdb_id(&self, _: &str) -> Option<Movie> {
            None
        }

        fn find_by_tmdb_id(&self, _: i64) -> Option<Movie> {
            None
        }

        fn movie_file(&self, id: i64) -> Option<MovieFile> {
            self.files.iter().find(|f| f.id == id).cloned()
        }

        fn history(&self, movie_id: i64) -> Vec<MovieHistoryEntry> {
            newest_first(
                self.history
                    .iter()
                    .filter(|h| h.movie_id == movie_id)
                    .cloned()
                    .collect(),
            )
        }

        fn blocklist(&self, _: i64) -> Vec<MovieBlocklistEntry> {
            Vec::new()
        }

        fn oldest_pending(&self, _: i64) -> Option<DateTime<Utc>> {
            None
        }

        fn pending_movie_ids(&self) -> Vec<i64> {
            Vec::new()
        }

        fn free_space(&self, path: &str) -> Option<i64> {
            self.free_space_asked.borrow_mut().push(path.to_string());
            self.free_space
        }
    }

    impl ImportSources for Library {
        fn history_by_download_id(&self, download_id: &str) -> Vec<MovieHistoryEntry> {
            newest_first(
                self.history
                    .iter()
                    .filter(|h| h.download_id.as_deref() == Some(download_id))
                    .cloned()
                    .collect(),
            )
        }

        fn files_in(&self, folder: &str) -> Vec<String> {
            self.disk
                .iter()
                .filter(|f| crate::mediafiles::parent_path(f) == Some(folder))
                .cloned()
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    //! The original's specification fixtures
    //! (`MediaFiles/MovieImport/Specifications/*Fixture.cs`), which assert
    //! on mocks and so are ported as unit tests, grouped by specification
    //! and named after the original's test methods.

    use chrono::{Duration, Utc};
    use serde_json::json;

    use super::testing::Library;
    use super::*;
    use crate::downloadclients::{ClientKind, ItemClientInfo};
    use crate::mediafiles::mediainfo::{format_timespan, MediaInfo};
    use crate::model::{ProfileFormatItem, Protocol, QualityModel};
    use crate::movies::decision::testing;
    use crate::movies::import::aggregate::grabbed_release;
    use crate::movies::model::MovieFile;
    use crate::movies::parser::quality::{BLURAY1080P, BLURAY720P, HDTV720P, SDTV};

    struct Harness {
        library: Library,
        catalog: MovieCatalog,
        config: MediaManagementConfig,
        now: DateTime<Utc>,
    }

    impl Harness {
        fn new() -> Harness {
            let now = Utc::now();
            let mut catalog = MovieCatalog::default();
            catalog.base.now = now;
            catalog.base.profiles = vec![testing::profile(&[], BLURAY1080P, true)];
            Harness {
                library: Library::default(),
                catalog,
                config: MediaManagementConfig::default(),
                now,
            }
        }

        fn run(
            &self,
            spec: Spec,
            local: &LocalMovie,
            item: Option<&DownloadClientItem>,
        ) -> Option<MovieImportRejection> {
            let ctx = Context {
                lookup: &self.library,
                sources: &self.library,
                catalog: &self.catalog,
                config: &self.config,
                now: self.now,
            };
            spec(&ctx, local, item)
        }

        fn accepts(&self, spec: Spec, local: &LocalMovie, item: Option<&DownloadClientItem>) -> bool {
            self.run(spec, local, item).is_none()
        }
    }

    fn quality(id: i64, version: i64) -> QualityModel {
        testing::quality(id, version)
    }

    fn file(id: i64, q: QualityModel, name: &str) -> MovieFile {
        MovieFile {
            id,
            movie_id: 1,
            relative_path: name.to_string(),
            original_file_path: None,
            size: 100,
            date_added: Utc::now(),
            scene_name: None,
            release_group: None,
            edition: None,
            quality: q,
            languages: vec![1],
            media_info: None,
            indexer_flags: 0,
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
            "Casablanca.1942.720p.HDTV-Radarr",
        )
    }

    fn history(id: i64, movie_id: i64, event: MovieHistoryEvent, date: DateTime<Utc>) -> MovieHistoryEntry {
        MovieHistoryEntry {
            id,
            movie_id,
            source_title: Some("Casablanca.1942.720p.HDTV-Radarr".into()),
            date,
            event_type: event,
            quality: quality(HDTV720P, 1),
            languages: vec![1],
            data: json!({}),
            download_id: Some("DownloadId1".into()),
        }
    }

    /// `Casablanca.1942.avi` under the unsorted folder, for a movie that
    /// has a file (as the object builder of the fixtures gives every movie
    /// the file id 1).
    fn local() -> LocalMovie {
        let mut movie = testing::movie(1, "Casablanca", 1942);
        movie.path = "/C/Test/Movies/Casablanca".into();
        movie.movie_file_id = 1;
        LocalMovie {
            path: "/C/Test/Unsorted/Casablanca/Casablanca.1942.avi".into(),
            size: 100,
            movie: Some(movie),
            quality: quality(HDTV720P, 1),
            languages: vec![1],
            ..Default::default()
        }
    }

    fn movie_of(local: &mut LocalMovie) -> &mut crate::movies::model::Movie {
        local.movie.as_mut().expect("a movie")
    }

    // AlreadyImportedSpecificationFixture

    #[test]
    fn should_accepted_if_download_client_item_is_null() {
        let h = Harness::new();
        assert!(h.accepts(already_imported, &local(), None));
    }

    #[test]
    fn should_accept_if_episode_does_not_have_file() {
        let mut h = Harness::new();
        h.library.history = vec![history(1, 1, MovieHistoryEvent::DownloadFolderImported, h.now)];
        let mut l = local();
        movie_of(&mut l).movie_file_id = 0;
        assert!(h.accepts(already_imported, &l, Some(&item())));
    }

    #[test]
    fn should_accept_if_episode_has_not_been_imported() {
        let mut h = Harness::new();
        h.library.history = vec![history(1, 1, MovieHistoryEvent::Grabbed, h.now)];
        assert!(h.accepts(already_imported, &local(), Some(&item())));
    }

    #[test]
    fn should_accept_if_episode_was_grabbed_after_being_imported() {
        let mut h = Harness::new();
        h.library.history = vec![
            history(1, 1, MovieHistoryEvent::Grabbed, h.now),
            history(
                2,
                1,
                MovieHistoryEvent::DownloadFolderImported,
                h.now - Duration::days(1),
            ),
            history(3, 1, MovieHistoryEvent::Grabbed, h.now - Duration::days(2)),
        ];
        assert!(h.accepts(already_imported, &local(), Some(&item())));
    }

    #[test]
    fn should_reject_if_episode_imported_after_being_grabbed() {
        let mut h = Harness::new();
        let imported = h.now - Duration::days(1);
        h.library.history = vec![
            history(1, 1, MovieHistoryEvent::DownloadFolderImported, imported),
            history(2, 1, MovieHistoryEvent::Grabbed, h.now - Duration::days(2)),
        ];
        let r = h.run(already_imported, &local(), Some(&item())).unwrap();
        assert_eq!(r.reason, R::MovieAlreadyImported);
        assert_eq!(
            r.message,
            format!("Movie file already imported at {}", local_time(imported))
        );
    }

    #[test]
    fn already_imported_reads_the_history_of_this_download_only() {
        let mut h = Harness::new();
        let mut imported = history(
            1,
            1,
            MovieHistoryEvent::DownloadFolderImported,
            h.now - Duration::days(1),
        );
        imported.download_id = Some("AnotherDownload".into());
        h.library.history = vec![imported.clone()];
        assert!(h.accepts(already_imported, &local(), Some(&item())));
        // Imported, never grabbed: rejected.
        imported.download_id = Some("DownloadId1".into());
        h.library.history = vec![imported.clone()];
        assert!(h.run(already_imported, &local(), Some(&item())).is_some());
        // A grab at the very time of the import is a grab made again.
        h.library.history = vec![history(2, 1, MovieHistoryEvent::Grabbed, imported.date), imported];
        assert!(h.accepts(already_imported, &local(), Some(&item())));
    }

    // FreeSpaceSpecificationFixture

    const MB: i64 = 1024 * 1024;

    /// `30.rock.s01e01.avi` for a movie under the root folder `/C/Test/TV`.
    fn rooted(size: i64) -> LocalMovie {
        let mut l = local();
        movie_of(&mut l).path = "/C/Test/TV/30 Rock".into();
        l.path = "/C/Test/Unsorted/30 Rock/30.rock.s01e01.avi".into();
        l.size = size;
        l
    }

    #[test]
    fn should_reject_when_there_isnt_enough_disk_space() {
        let mut h = Harness::new();
        h.config.minimum_free_space_when_importing_mb = 0;
        h.library.free_space = Some(80 * MB);
        let r = h.run(free_space, &rooted(100 * MB), None).unwrap();
        assert_eq!(r.reason, R::MinimumFreeSpace);
        assert_eq!(r.message, "Not enough free space");
    }

    #[test]
    fn should_reject_when_there_isnt_enough_space_for_file_plus_min_free_space() {
        let mut h = Harness::new();
        h.config.minimum_free_space_when_importing_mb = 100;
        h.library.free_space = Some(150 * MB);
        assert_eq!(
            h.run(free_space, &rooted(100 * MB), None).unwrap().reason,
            R::MinimumFreeSpace
        );
    }

    #[test]
    fn should_accept_when_there_is_enough_disk_space() {
        let mut h = Harness::new();
        h.library.free_space = Some(1024 * MB);
        assert!(h.accepts(free_space, &rooted(100 * MB), None));
    }

    #[test]
    fn should_use_series_paths_parent_for_free_space_check() {
        let mut h = Harness::new();
        h.library.free_space = Some(1024 * MB);
        assert!(h.accepts(free_space, &rooted(100 * MB), None));
        assert_eq!(*h.library.free_space_asked.borrow(), vec!["/C/Test/TV"]);
    }

    #[test]
    fn should_pass_if_free_space_is_null() {
        let h = Harness::new();
        assert!(h.accepts(free_space, &rooted(100 * MB), None));
    }

    #[test]
    fn should_skip_check_for_files_under_series_folder() {
        let mut h = Harness::new();
        h.library.free_space = Some(1);
        let mut l = rooted(100 * MB);
        l.existing_file = true;
        assert!(h.accepts(free_space, &l, None));
        assert!(h.library.free_space_asked.borrow().is_empty());
    }

    #[test]
    fn should_return_true_if_free_space_is_null() {
        let h = Harness::new();
        assert!(h.accepts(free_space, &local(), None));
    }

    #[test]
    fn should_return_true_when_skip_check_is_enabled() {
        let mut h = Harness::new();
        h.library.free_space = Some(1);
        h.config.skip_free_space_check_when_importing = true;
        assert!(h.accepts(free_space, &rooted(100 * MB), None));
        assert!(h.library.free_space_asked.borrow().is_empty());
    }

    // GrabbedReleaseQualityFixture

    fn bluray() -> LocalMovie {
        let mut l = local();
        l.quality = quality(BLURAY720P, 1);
        l
    }

    #[test]
    fn should_be_accepted_when_download_client_item_is_null() {
        let h = Harness::new();
        assert!(h.accepts(grabbed_release_quality, &bluray(), None));
    }

    #[test]
    fn should_be_accepted_if_no_history_for_download_id() {
        let h = Harness::new();
        assert!(h.accepts(grabbed_release_quality, &bluray(), Some(&item())));
    }

    #[test]
    fn should_be_accepted_if_no_grabbed_history_for_download_id() {
        let mut h = Harness::new();
        h.library.history = vec![history(1, 1, MovieHistoryEvent::Unknown, h.now)];
        assert!(h.accepts(grabbed_release_quality, &bluray(), Some(&item())));
    }

    #[test]
    fn should_be_accepted_if_grabbed_history_quality_is_unknown() {
        let mut h = Harness::new();
        let mut grabbed = history(1, 1, MovieHistoryEvent::Grabbed, h.now);
        grabbed.quality = quality(UNKNOWN_QUALITY_ID, 1);
        h.library.history = vec![grabbed];
        assert!(h.accepts(grabbed_release_quality, &bluray(), Some(&item())));
    }

    #[test]
    fn should_be_accepted_if_grabbed_history_quality_matches() {
        let mut h = Harness::new();
        let mut grabbed = history(1, 1, MovieHistoryEvent::Grabbed, h.now);
        grabbed.quality = quality(BLURAY720P, 1);
        h.library.history = vec![grabbed];
        assert!(h.accepts(grabbed_release_quality, &bluray(), Some(&item())));
    }

    #[test]
    fn should_be_accepted_if_grabbed_history_quality_does_not_match_log_only() {
        let mut h = Harness::new();
        h.library.history = vec![history(1, 1, MovieHistoryEvent::Grabbed, h.now)];
        assert!(h.accepts(grabbed_release_quality, &bluray(), Some(&item())));
    }

    // HasAudioTrackSpecificationFixture

    fn audio_streams(count: i64) -> MediaInfo {
        MediaInfo {
            model: json!({"audioStreamCount": count}),
            title: None,
        }
    }

    #[test]
    fn should_accept_if_media_info_is_null() {
        let h = Harness::new();
        assert!(h.accepts(has_audio_track, &local(), None));
    }

    #[test]
    fn should_reject_if_audio_stream_count_is_0() {
        let h = Harness::new();
        let mut l = local();
        l.media_info = Some(audio_streams(0));
        let r = h.run(has_audio_track, &l, None).unwrap();
        assert_eq!(r.reason, R::NoAudio);
        assert_eq!(r.message, "No audio tracks detected");
    }

    /// Named as the original names it; the count it gives is 1.
    #[test]
    fn should_accept_if_audio_stream_count_is_0() {
        let h = Harness::new();
        let mut l = local();
        l.media_info = Some(audio_streams(1));
        assert!(h.accepts(has_audio_track, &l, None));
    }

    // MatchesFolderSpecificationFixture: every test of the original is
    // commented out, as is the body of the specification.

    #[test]
    fn matches_folder_accepts_whatever_the_folder_is_named() {
        let h = Harness::new();
        let mut l = local();
        l.path = "/C/Test/Unsorted/Series.Title.S01E01.720p.HDTV-Sonarr/S01E05.mkv".into();
        assert!(h.accepts(matches_folder, &l, None));
        l.existing_file = true;
        assert!(h.accepts(matches_folder, &l, None));
    }

    // MatchesGrabSpecificationFixture

    /// `GivenHistoryForMovies`: a grab for each of these movies.
    fn grabbed_for(h: &Harness, l: &mut LocalMovie, movie_ids: &[i64]) {
        if movie_ids.is_empty() {
            return;
        }
        let grabs: Vec<MovieHistoryEntry> = movie_ids
            .iter()
            .enumerate()
            .map(|(i, id)| history(i as i64 + 1, *id, MovieHistoryEvent::Grabbed, h.now))
            .collect();
        l.release = Some(grabbed_release(&grabs));
    }

    #[test]
    fn should_be_accepted_for_existing_file() {
        let h = Harness::new();
        let mut l = local();
        grabbed_for(&h, &mut l, &[2]);
        l.existing_file = true;
        assert!(h.accepts(matches_grab, &l, None));
    }

    #[test]
    fn should_be_accepted_if_no_download_client_item() {
        let h = Harness::new();
        assert!(h.accepts(matches_grab, &local(), None));
    }

    #[test]
    fn should_be_accepted_if_no_grab_release_info() {
        let h = Harness::new();
        let mut l = local();
        grabbed_for(&h, &mut l, &[]);
        assert!(h.accepts(matches_grab, &l, Some(&item())));
    }

    #[test]
    fn should_be_accepted_if_file_episode_matches_single_grab_release_info() {
        let h = Harness::new();
        let mut l = local();
        grabbed_for(&h, &mut l, &[1]);
        assert!(h.accepts(matches_grab, &l, Some(&item())));
        grabbed_for(&h, &mut l, &[2, 1]);
        assert!(h.accepts(matches_grab, &l, Some(&item())), "one of the grabs");
    }

    #[test]
    fn should_be_rejected_if_file_episode_does_not_match_single_grab_release_info() {
        let h = Harness::new();
        let mut l = local();
        grabbed_for(&h, &mut l, &[2]);
        let r = h.run(matches_grab, &l, Some(&item())).unwrap();
        assert_eq!(r.reason, R::MovieNotFoundInRelease);
        assert_eq!(
            r.message,
            "Movie [Casablanca (1942)][tt0001001, 1001] was not found in the grabbed release: Casablanca.1942.720p.HDTV-Radarr"
        );
    }

    // NotMultiPartSpecificationFixture

    /// The first path is the file; all of them are in its folder.
    fn multi_part(paths: &[&str]) -> Option<MovieImportRejection> {
        let mut h = Harness::new();
        h.library.disk = paths.iter().map(|p| p.to_string()).collect();
        let mut l = local();
        l.path = paths[0].to_string();
        h.run(not_multi_part, &l, None)
    }

    #[test]
    fn should_be_accepted_for_legitimate_files() {
        for path in [
            "/C/Test/Downloaded/x.men.2018.avi",
            "/C/Test/Downloaded/Captain.Phillips.2013.MULTi.1080p.BluRay.x264-LOST/lost-captainphillips.2013.1080p.mkv",
            "/C/Test/Downloaded/Harry.Potter.And.The.Deathly.Hallows.Part.1.2010.1080p.BluRay.x264-EbP/Harry.Potter.And.The.Deathly.Hallows.Part.1.2010.1080p.BluRay.x264-EbP.mkv",
        ] {
            assert_eq!(multi_part(&[path]), None, "{path}");
        }
    }

    #[test]
    fn should_be_rejected_for_multi_part_files() {
        for paths in [
            [
                "/C/Test/Downloaded/Bad Boys (2006) part1.mkv",
                "/C/Test/Downloaded/Bad Boys (2006) part2.mkv",
            ],
            [
                "/C/Test/Downloaded/Bad Boys (2006) pt1.mkv",
                "/C/Test/Downloaded/Bad Boys (2006) pt2.mkv",
            ],
            [
                "/C/Test/Downloaded/Bad Boys (2006) P1.mkv",
                "/C/Test/Downloaded/Bad Boys (2006) P2.mkv",
            ],
            [
                "/C/Test/Downloaded/blah blah - cd 1.mvk",
                "/C/Test/Downloaded/blah blah - cd 2.mvk",
            ],
            [
                "/C/Test/Downloaded/blah blah - dvd a.mvk",
                "/C/Test/Downloaded/blah blah - dvd b.mvk",
            ],
        ] {
            let r = multi_part(&paths).unwrap_or_else(|| panic!("{} is accepted", paths[0]));
            assert_eq!(r.reason, R::MultiPartMovie);
            assert_eq!(
                r.message,
                "File is suspected multi-part file, Radarr doesn't support this"
            );
        }
    }

    #[test]
    fn should_not_reject_if_multi_part_schemes_mixed() {
        assert_eq!(
            multi_part(&[
                "/C/Test/Downloaded/blah blah - cd a.mvk",
                "/C/Test/Downloaded/blah blah - cd 2.mvk",
            ]),
            None
        );
    }

    #[test]
    fn should_not_reject_if_file_names_mixed() {
        assert_eq!(
            multi_part(&[
                "/C/Test/Downloaded/blah blah - cd a.mvk",
                "/C/Test/Downloaded/ping pong - cd b.mvk",
            ]),
            None
        );
    }

    #[test]
    fn the_files_of_a_folder_are_read_from_the_disk() {
        let dir = std::env::temp_dir().join(format!("unarr-multi-part-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for name in ["Bad Boys (2006) part1.mkv", "Bad Boys (2006) part2.mkv"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let conn = Connection::open_in_memory().unwrap();
        let sources = StoreImportSources { conn: &conn };
        let folder = dir.to_string_lossy().into_owned();
        let files = sources.files_in(&folder);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            files,
            vec![
                format!("{folder}/Bad Boys (2006) part1.mkv"),
                format!("{folder}/Bad Boys (2006) part2.mkv"),
            ]
        );
        assert!(sources.files_in("/no/such/folder").is_empty());
    }

    // NotSampleSpecificationFixture

    #[test]
    fn should_return_true_for_existing_file() {
        let h = Harness::new();
        let mut l = local();
        l.path = "/C/Test/30 Rock/30.rock.s01e01.avi".into();
        l.existing_file = true;
        assert!(h.accepts(not_sample, &l, None));
    }

    #[test]
    fn not_sample_rejects_a_short_file_and_one_it_cannot_measure() {
        let h = Harness::new();
        let mut l = local();
        let r = h.run(not_sample, &l, None).unwrap();
        assert_eq!(r.reason, R::SampleIndeterminate);
        assert_eq!(r.message, "Unable to determine if file is a sample");
        let runtime = |seconds: f64| MediaInfo {
            model: json!({"runTime": format_timespan(seconds)}),
            title: None,
        };
        l.media_info = Some(runtime(60.0));
        let r = h.run(not_sample, &l, None).unwrap();
        assert_eq!((r.reason, r.message.as_str()), (R::Sample, "Sample"));
        l.media_info = Some(runtime(6000.0));
        assert!(h.accepts(not_sample, &l, None));
        l.media_info = None;
        l.path = "/C/Test/Unsorted/Casablanca/Casablanca.1942.iso".into();
        assert!(h.accepts(not_sample, &l, None), "a disc image is never a sample");
    }

    // NotUnpackingSpecificationFixture

    fn unpacking() -> (Harness, LocalMovie) {
        let mut h = Harness::new();
        h.config.download_client_working_folders = "_UNPACK_|_FAILED_".into();
        let mut l = local();
        l.path = "/C/Test/Unsorted TV/30.rock/30.rock.s01e01.avi".into();
        (h, l)
    }

    const IN_WORKING_FOLDER: &str = "/C/Test/Unsorted TV/_UNPACK_30.rock/someSubFolder/30.rock.s01e01.avi";

    #[test]
    fn should_return_true_if_not_in_working_folder() {
        let (h, l) = unpacking();
        assert!(h.accepts(not_unpacking, &l, None));
    }

    #[test]
    fn should_return_false_if_in_working_folder_and_last_write_time_was_recent() {
        let (h, mut l) = unpacking();
        l.path = IN_WORKING_FOLDER.into();
        let r = h.run(not_unpacking, &l, None).unwrap();
        assert_eq!(r.reason, R::Unpacking);
        assert_eq!(r.message, "File is still being unpacked");
    }

    /// However old the file is: only Windows looks at when it was written.
    #[test]
    fn should_return_false_if_unopacking_on_linux() {
        let (h, mut l) = unpacking();
        l.path = IN_WORKING_FOLDER.into();
        assert_eq!(h.run(not_unpacking, &l, None).unwrap().reason, R::Unpacking);
        l.path = "/C/Test/_FAILED_30.rock/30.rock.s01e01.avi".into();
        assert_eq!(h.run(not_unpacking, &l, None).unwrap().reason, R::Unpacking);
        l.existing_file = true;
        assert!(h.accepts(not_unpacking, &l, None), "a file of the movie's folder");
    }

    // UpgradeSpecificationFixture

    /// `30.rock.s01e01.avi`, an HDTV-720p, for a movie whose file has the
    /// id 1 and this quality.
    fn upgrading(h: &mut Harness, existing: Option<QualityModel>) -> LocalMovie {
        let mut l = local();
        l.path = "/C/Test/30 Rock/30.rock.s01e01.avi".into();
        if let Some(q) = existing {
            h.library.files.push(file(1, q, "30 Rock (2006).mkv"));
        }
        l
    }

    fn propers(h: &mut Harness, propers: ProperDownloadTypes) {
        h.catalog.base.config.download_propers_and_repacks = propers;
    }

    #[test]
    fn should_return_true_if_no_existing_episode_file() {
        let mut h = Harness::new();
        let mut l = upgrading(&mut h, None);
        movie_of(&mut l).movie_file_id = 0;
        assert!(h.accepts(upgrade, &l, None));
    }

    #[test]
    fn should_return_true_if_upgrade_for_existing_episode_file() {
        let mut h = Harness::new();
        let l = upgrading(&mut h, Some(quality(SDTV, 1)));
        assert!(h.accepts(upgrade, &l, None));
    }

    #[test]
    fn should_return_false_if_not_an_upgrade_for_existing_episode_file() {
        let mut h = Harness::new();
        let l = upgrading(&mut h, Some(quality(BLURAY720P, 1)));
        let r = h.run(upgrade, &l, None).unwrap();
        assert_eq!(r.reason, R::NotQualityUpgrade);
        assert_eq!(
            r.message,
            "Not an upgrade for existing movie file. Existing quality: Bluray-720p. New Quality HDTV-720p."
        );
    }

    #[test]
    fn should_return_false_if_not_a_revision_upgrade_and_prefers_propers() {
        let mut h = Harness::new();
        propers(&mut h, ProperDownloadTypes::PreferAndUpgrade);
        let l = upgrading(&mut h, Some(quality(HDTV720P, 2)));
        let r = h.run(upgrade, &l, None).unwrap();
        assert_eq!(r.reason, R::NotRevisionUpgrade);
        assert_eq!(
            r.message,
            "Not a quality revision upgrade for existing movie file(s)"
        );
    }

    #[test]
    fn should_return_true_if_not_a_revision_upgrade_and_does_not_prefer_propers() {
        let mut h = Harness::new();
        propers(&mut h, ProperDownloadTypes::DoNotPrefer);
        let l = upgrading(&mut h, Some(quality(HDTV720P, 2)));
        assert!(h.accepts(upgrade, &l, None));
    }

    #[test]
    fn should_return_true_when_comparing_to_a_lower_quality_proper() {
        let mut h = Harness::new();
        propers(&mut h, ProperDownloadTypes::DoNotPrefer);
        let mut l = upgrading(&mut h, Some(quality(BLURAY1080P, 2)));
        l.quality = quality(BLURAY1080P, 1);
        assert!(h.accepts(upgrade, &l, None));
    }

    #[test]
    fn should_return_true_if_movie_file_is_null() {
        let mut h = Harness::new();
        let l = upgrading(&mut h, None);
        assert_eq!(l.movie_ref().movie_file_id, 1);
        assert!(h.accepts(upgrade, &l, None));
    }

    /// A file of `existing` quality whose name matches the one custom
    /// format, which the profile scores `existing_score`; the new file is a
    /// Bluray-1080p with another format and the score 20.
    fn with_format_scores(h: &mut Harness, existing: i64, existing_score: i64) -> LocalMovie {
        let format = |id: i64, name: &str, value: &str| CustomFormat {
            id,
            name: name.into(),
            include_when_renaming: false,
            specifications: json!([{"type": "ReleaseTitleSpecification", "body": {"value": value, "negate": false, "required": false}}]),
        };
        h.catalog.base.formats = vec![format(1, "Name1", "GRP")].into();
        h.catalog.base.profiles[0].format_items = vec![ProfileFormatItem {
            format: 1,
            score: existing_score,
        }];
        propers(h, ProperDownloadTypes::DoNotPrefer);
        h.library
            .files
            .push(file(1, quality(existing, 1), "30 Rock (2006) Bluray-GRP.mkv"));
        let mut l = local();
        l.path = "/C/Test/30 Rock/30.rock.s01e01.avi".into();
        l.quality = quality(BLURAY1080P, 1);
        l.custom_formats = vec![format(2, "Name2", "OTHER")];
        l.custom_format_score = 20;
        l
    }

    #[test]
    fn should_return_true_if_upgrade_to_custom_format_score() {
        let mut h = Harness::new();
        let l = with_format_scores(&mut h, BLURAY1080P, 10);
        assert!(h.accepts(upgrade, &l, None));
    }

    #[test]
    fn should_return_true_if_not_upgrade_to_custom_format_score_but_is_upgrade_to_quality() {
        let mut h = Harness::new();
        let l = with_format_scores(&mut h, BLURAY720P, 50);
        assert!(h.accepts(upgrade, &l, None));
    }

    /// Radarr 7: the name the file had scored above the existing file, the
    /// name the import gives it does not.
    #[test]
    fn should_return_false_and_a_specific_reason_if_not_upgrade_to_custom_format_score_after_local_file_rename_but_was_before(
    ) {
        let mut h = Harness::new();
        let mut l = with_format_scores(&mut h, BLURAY1080P, 50);
        l.original_file_name_custom_formats = l.custom_formats.clone();
        l.original_file_name_custom_format_score = 60;
        let r = h.run(upgrade, &l, None).unwrap();
        assert_eq!(r.reason, R::NotCustomFormatUpgradeAfterRename);
        assert_eq!(
            r.message,
            "Not a Custom Format upgrade for existing movie file(s). AfterRename: [Name2] (20) do not improve on Existing: [Name1] (50) even though BeforeRename: [Name2] (60) did."
        );
    }

    #[test]
    fn should_return_false_if_not_upgrade_to_custom_format_score() {
        let mut h = Harness::new();
        let l = with_format_scores(&mut h, BLURAY1080P, 50);
        let r = h.run(upgrade, &l, None).unwrap();
        assert_eq!(r.reason, R::NotCustomFormatUpgrade);
        assert_eq!(
            r.message,
            "Not a Custom Format upgrade for existing movie file(s). New: [Name2] (20) do not improve on Existing: [Name1] (50)"
        );
    }

    // All of them

    #[test]
    fn the_specifications_run_in_the_order_of_their_source_files() {
        let names: Vec<&str> = SPECIFICATIONS.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), 10);
    }

    #[test]
    fn every_rejection_is_reported_in_order() {
        let mut h = Harness::new();
        h.config.download_client_working_folders = "_UNPACK_".into();
        h.library.free_space = Some(1);
        h.library
            .files
            .push(file(1, quality(BLURAY1080P, 1), "Casablanca (1942).mkv"));
        let mut l = local();
        l.path = "/C/Test/Unsorted/_UNPACK_Casablanca/Casablanca.1942.avi".into();
        l.media_info = Some(MediaInfo {
            model: json!({"audioStreamCount": 0, "runTime": format_timespan(30.0)}),
            title: None,
        });
        let ctx = Context {
            lookup: &h.library,
            sources: &h.library,
            catalog: &h.catalog,
            config: &h.config,
            now: h.now,
        };
        let reasons: Vec<R> = evaluate(&ctx, &l, None).into_iter().map(|r| r.reason).collect();
        assert_eq!(
            reasons,
            vec![
                R::MinimumFreeSpace,
                R::NoAudio,
                R::Sample,
                R::Unpacking,
                R::NotQualityUpgrade
            ]
        );
        // A file of the movie's folder is spared all but the upgrade check.
        l.existing_file = true;
        l.media_info = None;
        let reasons: Vec<R> = evaluate(&ctx, &l, None).into_iter().map(|r| r.reason).collect();
        assert_eq!(reasons, vec![R::NotQualityUpgrade]);
    }
}
