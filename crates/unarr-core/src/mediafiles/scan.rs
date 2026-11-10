//! `DiskScanService` and `MediaFileTableCleanupService`: a series folder
//! read, the rows for files that are gone removed, the files not yet in the
//! library imported, the sizes and media info of the known ones refreshed.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::approve;
use super::decide::{self, Environment, Probes};
use super::deletion;
use super::events::{Events, ImportEvent};
use super::local::{DeleteReason, ImportMode};
use super::mediainfo;
use super::transfer;
use crate::db::Db;
use crate::model::{EpisodeFile, Series};
use crate::parser::media_extensions::is_media_extension;
use crate::store;
use crate::store::config::MediaManagementConfig;

use fancy_regex::Regex;
use std::sync::OnceLock;

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).expect("a valid pattern"))
        }
    };
}

regex!(
    excluded_extras_subfolder,
    r"(?i)(?:\\|\/|^)(?:extras|extrafanart|behind the scenes|deleted scenes|featurettes|interviews|other|scenes|samples|shorts|trailers)(?:\\|\/)"
);
regex!(
    excluded_subfolders,
    r"(?i)(?:\\|\/|^)(?:@eadir|\.@__thumb|plex versions|\.[^\\/]+)(?:\\|\/)"
);
regex!(
    excluded_extra_files,
    r"(?i)(-(trailer|other|behindthescenes|deleted|featurette|interview|scene|short)\.[^.]+$)"
);
regex!(excluded_files, r"(?i)^\.(_|unmanic|DS_Store$)|^Thumbs\.db$");

fn matches(re: &Regex, s: &str) -> bool {
    re.is_match(s).unwrap_or(false)
}

/// `GetVideoFiles(path, allDirectories)`.
pub fn video_files(path: &str, all_directories: bool) -> Vec<String> {
    tracing::debug!("Scanning '{path}' for video files");
    let files = files_in(path, all_directories);
    let video: Vec<String> = files
        .iter()
        .filter(|f| is_media_extension(super::extension(f)))
        .cloned()
        .collect();
    tracing::trace!("{} files were found in {path}", files.len());
    tracing::debug!("{} video files were found in {path}", video.len());
    video
}

/// `GetNonVideoFiles(path, allDirectories)`.
pub fn non_video_files(path: &str, all_directories: bool) -> Vec<String> {
    files_in(path, all_directories)
        .into_iter()
        .filter(|f| !is_media_extension(super::extension(f)))
        .collect()
}

/// The files under `path`, sorted, as forward-slash strings.
pub fn files_in(path: &str, all_directories: bool) -> Vec<String> {
    let root = Path::new(path);
    let mut out: Vec<String> = if all_directories {
        transfer::files_under(root)
    } else {
        std::fs::read_dir(root)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect()
            })
            .unwrap_or_default()
    }
    .into_iter()
    .map(|p| p.to_string_lossy().into_owned())
    .collect();
    out.sort();
    out
}

/// `FilterPaths(basePath, paths, filterExtras)`.
pub fn filter_paths(base_path: &str, paths: &[String], filter_extras: bool) -> Vec<String> {
    paths
        .iter()
        .filter(|p| !matches(excluded_subfolders(), super::relative_path(base_path, p)))
        .filter(|p| !matches(excluded_files(), super::file_name(p)))
        .filter(|p| {
            !filter_extras
                || (!matches(excluded_extras_subfolder(), super::relative_path(base_path, p))
                    && !matches(excluded_extra_files(), super::file_name(p)))
        })
        .cloned()
        .collect()
}

/// `MediaFileTableCleanupService.Clean`: rows for files not on disk, or
/// linked to no episode, are removed; episodes pointing at a missing row
/// are detached.
pub fn clean_media_files(
    conn: &Connection,
    config: &MediaManagementConfig,
    series: &Series,
    files_on_disk: &[String],
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<ImportEvent>> {
    let mut events = Vec::new();
    let series_files = store::episodes::list_files(conn, series.id)?;
    let episodes = store::episodes::list_episodes(conn, series.id, None)?;
    for file in &series_files {
        let path = super::join(&series.path, &file.relative_path);
        if !files_on_disk.iter().any(|f| super::path_equals(f, &path)) {
            tracing::debug!("File [{path}] no longer exists on disk, removing from db");
            let ev = deletion::delete_file(conn, config, series, file, DeleteReason::MissingFromDisk, now)?;
            events.push(ImportEvent::EpisodeFileDeleted(Box::new(ev)));
            continue;
        }
        if !episodes.iter().any(|e| e.episode_file_id == file.id) {
            tracing::debug!("File [{path}] is not assigned to any episodes, removing from db");
            let ev = deletion::delete_file(conn, config, series, file, DeleteReason::NoLinkedEpisodes, now)?;
            events.push(ImportEvent::EpisodeFileDeleted(Box::new(ev)));
        }
    }
    for e in &episodes {
        if e.episode_file_id > 0 && !series_files.iter().any(|f| f.id == e.episode_file_id) {
            store::episodes::set_episode_file_id(conn, e.id, 0)?;
        }
    }
    Ok(events)
}

/// `UpdateMediaInfoService.Handle(SeriesScannedEvent)`: the files of the
/// series that have no media info, or one of a schema older than the
/// least still read, are probed again. Returns how many got theirs. The
/// probing runs here, not on the database's thread: a library imported
/// without media info has thousands of such files.
pub fn backfill_media_info(
    db: &Db,
    config: &MediaManagementConfig,
    series: &Series,
) -> anyhow::Result<usize> {
    if !config.enable_media_info {
        tracing::debug!("MediaInfo is disabled");
        return Ok(0);
    }
    let series_id = series.id;
    let stale: Vec<EpisodeFile> = db
        .call_blocking(move |conn| store::episodes::list_files(conn, series_id))?
        .into_iter()
        .filter(|file| {
            let revision = file
                .media_info
                .as_ref()
                .filter(|info| !info.is_null())
                .map(|info| info.get("schemaRevision").and_then(|r| r.as_i64()).unwrap_or(0));
            revision.is_none_or(|r| r < mediainfo::MINIMUM_MEDIA_INFO_SCHEMA_REVISION)
        })
        .collect();
    let mut updated = 0;
    for mut file in stale {
        let path = super::join(&series.path, &file.relative_path);
        if !Path::new(&path).is_file() {
            tracing::debug!("Can't update MediaInfo because '{path}' does not exist");
            continue;
        }
        let Some(info) = mediainfo::probe(Path::new(&path)) else {
            continue;
        };
        file.media_info = Some(info.model);
        db.call_blocking(move |conn| store::episodes::update_file(conn, &file))?;
        tracing::debug!("Updated MediaInfo for '{path}'");
        updated += 1;
    }
    Ok(updated)
}

/// `UpdateMediaInfoService.Update(episodeFile, series)`: the media info
/// read again; `true` when the row was written.
pub fn update_media_info(
    conn: &Connection,
    config: &MediaManagementConfig,
    file: &mut EpisodeFile,
    series: &Series,
) -> anyhow::Result<bool> {
    if !config.enable_media_info {
        tracing::debug!("MediaInfo is disabled");
        return Ok(false);
    }
    let path = super::join(&series.path, &file.relative_path);
    match mediainfo::probe(Path::new(&path)) {
        Some(info) => {
            file.media_info = Some(info.model);
            store::episodes::update_file(conn, file)?;
            tracing::debug!("Updated MediaInfo for '{path}'");
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `Scan(series)`. `progress` receives the originals' `ProgressInfo`
/// messages (the command shows the last one).
pub fn scan(
    db: &Db,
    env: &Environment,
    series: &Series,
    now: DateTime<Utc>,
    progress: &mut dyn FnMut(String),
) -> anyhow::Result<Events> {
    let mut events = Events::default();
    let root_folders = db.call_blocking(|conn| store::catalog::list_root_folders(conn))?;
    let root_folder = approve::best_root_folder_path(&root_folders, &series.path);
    let series_folder_exists = Path::new(&series.path).is_dir();
    if !series_folder_exists {
        if !Path::new(&root_folder).is_dir() {
            tracing::warn!("Series' root folder ({root_folder}) doesn't exist.");
            events.push(ImportEvent::SeriesScanSkipped { series_id: series.id });
            return Ok(events);
        }
        if transfer::folder_empty(Path::new(&root_folder)) {
            tracing::warn!("Series' root folder ({root_folder}) is empty.");
            events.push(ImportEvent::SeriesScanSkipped { series_id: series.id });
            return Ok(events);
        }
    }
    progress(format!("Scanning {}", series.title));
    if !series_folder_exists {
        if env.config.create_empty_series_folders {
            if env.config.delete_empty_folders {
                tracing::debug!(
                    "Not creating missing series folder: {} because delete empty series folders is enabled",
                    series.path
                );
            } else {
                tracing::debug!("Creating missing series folder: {}", series.path);
                if let Err(e) = transfer::create_folder(&env.config, Path::new(&series.path)) {
                    tracing::warn!(error = %e, "unable to create the series folder");
                }
            }
        } else {
            tracing::debug!("Series folder doesn't exist: {}", series.path);
        }
        let (config, series2) = (env.config.clone(), series.clone());
        events
            .0
            .extend(db.call_blocking(move |conn| clean_media_files(conn, &config, &series2, &[], now))?);
        // `ExistingExtraFileService`: the subtitles and other extras in the
        // folder are remembered with the episode files they belong to.
        if Path::new(&series.path).is_dir() {
            let extras: Vec<String> = files_in(&series.path, true)
                .into_iter()
                .filter(|f| !crate::parser::media_extensions::is_media_extension(super::extension(f)))
                .collect();
            let possible = filter_paths(&series.path, &extras, true);
            let series2 = series.clone();
            db.call_blocking(move |conn| super::extras::import_existing(conn, &series2, &possible, now))?;
        }
        tracing::info!("Completed scanning disk for {}", series.title);
        unmonitor_missing(db, &env.config, &events)?;
        events.push(ImportEvent::SeriesScanned { series_id: series.id });
        return Ok(events);
    }
    let started = std::time::Instant::now();
    let media_files = filter_paths(&series.path, &video_files(&series.path, true), true);
    tracing::trace!(
        "Finished getting episode files for: {} [{:?}]",
        series.title,
        started.elapsed()
    );
    let (config, series2, files2) = (env.config.clone(), series.clone(), media_files.clone());
    let (cleanup_events, series_files) = db.call_blocking(move |conn| {
        tracing::debug!("{} Cleaning up media files in DB", series2.title);
        let ev = clean_media_files(conn, &config, &series2, &files2, now)?;
        Ok((ev, store::episodes::list_files(conn, series2.id)?))
    })?;
    events.0.extend(cleanup_events);
    let unmapped = decide::filter_existing(&media_files, &series_files, series);
    let probes = Probes::probe_all(&unmapped);
    let (env_series, unmapped2) = (series.clone(), unmapped.clone());
    let decisions = {
        let catalog = env.catalog.clone();
        let config = env.config.clone();
        let naming = env.naming.clone();
        db.call_blocking(move |conn| {
            let env = Environment {
                catalog,
                config,
                naming,
                now,
            };
            decide::get_import_decisions(
                conn,
                &env,
                &probes,
                &unmapped2,
                &env_series,
                None,
                None,
                false,
                false,
                None,
            )
        })?
    };
    tracing::trace!("Import decisions complete for: {}", series.title);
    let imported = approve::import(db, env, decisions, false, None, ImportMode::Auto, now);
    events.extend(imported.events);
    // Known files whose size changed get their size and media info back.
    let mut to_update = Vec::new();
    for mut file in series_files {
        let path = super::join(&series.path, &file.relative_path);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let size = meta.len() as i64;
        if file.size == size {
            continue;
        }
        file.size = size;
        to_update.push(file);
    }
    if !to_update.is_empty() {
        let (config, series2) = (env.config.clone(), series.clone());
        db.call_blocking(move |conn| {
            for mut file in to_update {
                if !update_media_info(conn, &config, &mut file, &series2)? {
                    store::episodes::update_file(conn, &file)?;
                }
            }
            Ok(())
        })?;
    }
    backfill_media_info(db, &env.config, series)?;
    if env.config.delete_empty_folders {
        transfer::remove_empty_subfolders(Path::new(&series.path));
        if transfer::folder_empty(Path::new(&series.path)) {
            let _ = std::fs::remove_dir_all(&series.path);
        }
    }
    tracing::info!("Completed scanning disk for {}", series.title);
    unmonitor_missing(db, &env.config, &events)?;
    events.push(ImportEvent::SeriesScanned { series_id: series.id });
    Ok(events)
}

/// `EpisodeService.HandleAsync(SeriesScannedEvent)`: with "unmonitor
/// deleted episodes" on, the episodes whose file went missing from disk
/// during the scan, and that the scan did not link to another file, are
/// unmonitored.
fn unmonitor_missing(db: &Db, config: &MediaManagementConfig, events: &Events) -> anyhow::Result<()> {
    if !config.auto_unmonitor_previously_downloaded_episodes {
        return Ok(());
    }
    let ids: Vec<i64> = events
        .0
        .iter()
        .filter_map(|e| match e {
            ImportEvent::EpisodeFileDeleted(d) if d.reason == DeleteReason::MissingFromDisk => Some(d),
            _ => None,
        })
        .flat_map(|d| d.episodes.iter().map(|e| e.id))
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    db.call_blocking(move |conn| {
        let still_missing: Vec<i64> = store::episodes::list_episodes_by_ids(conn, &ids)?
            .into_iter()
            .filter(|e| e.episode_file_id == 0)
            .map(|e| e.id)
            .collect();
        store::episodes::set_monitored(conn, &still_missing, false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files that came without media info, or with one too old to read,
    /// get it; those that have it are left alone.
    #[test]
    fn files_without_media_info_are_probed_again() {
        if !mediainfo::ffprobe_available() {
            eprintln!("ffprobe not available, skipping");
            return;
        }
        use crate::model::{QualityModel, ReleaseType};
        let folder = std::env::temp_dir().join(format!("unarr_backfill_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/media/episode.mkv");
        for name in ["none.mkv", "old.mkv", "current.mkv"] {
            std::fs::copy(&clip, folder.join(name)).unwrap();
        }
        let db = Db::open_in_memory().unwrap();
        let mut series = crate::store::series::tests::sample(1);
        series.path = folder.to_string_lossy().into_owned();
        let stored = series.clone();
        db.call_blocking(move |conn| {
            // The sample series' profile and tag, for the foreign keys.
            conn.execute_batch(
                "INSERT INTO quality_profiles (id, name, cutoff, items) VALUES (1, 'Any', 1, '[]');
                 INSERT INTO tags (id, label) VALUES (1, 'drama');",
            )?;
            crate::store::series::insert(conn, &stored)?;
            let rows = [
                (1, "none.mkv", None),
                (
                    2,
                    "old.mkv",
                    Some(serde_json::json!({"schemaRevision": 5, "videoFormat": "stale"})),
                ),
                (
                    3,
                    "current.mkv",
                    Some(serde_json::json!({"schemaRevision": 11, "videoFormat": "kept"})),
                ),
                (4, "gone.mkv", None),
            ];
            for (id, name, media_info) in rows {
                store::episodes::insert_file(
                    conn,
                    &EpisodeFile {
                        id,
                        series_id: 1,
                        season_number: 1,
                        relative_path: name.into(),
                        original_file_path: None,
                        size: 1,
                        date_added: Utc::now(),
                        scene_name: None,
                        release_group: None,
                        release_hash: None,
                        quality: QualityModel {
                            quality_id: 7,
                            version: 1,
                            real: 0,
                            is_repack: false,
                        },
                        languages: vec![1],
                        media_info,
                        indexer_flags: 0,
                        release_type: ReleaseType::SingleEpisode,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();

        let mut config = MediaManagementConfig {
            enable_media_info: false,
            ..Default::default()
        };
        assert_eq!(
            backfill_media_info(&db, &config, &series).unwrap(),
            0,
            "switched off"
        );
        config.enable_media_info = true;
        assert_eq!(backfill_media_info(&db, &config, &series).unwrap(), 2);
        let files = db
            .call_blocking(|conn| store::episodes::list_files(conn, 1))
            .unwrap();
        let format = |id: i64| {
            files
                .iter()
                .find(|f| f.id == id)
                .and_then(|f| f.media_info.as_ref())
                .map(|info| info["videoFormat"].as_str().unwrap_or_default().to_string())
        };
        assert_eq!(format(1).as_deref(), Some("h264"));
        assert_eq!(format(2).as_deref(), Some("h264"));
        assert_eq!(format(3).as_deref(), Some("kept"));
        assert_eq!(format(4), None, "a file that is not there keeps none");
        // Nothing left to do.
        assert_eq!(backfill_media_info(&db, &config, &series).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// A series whose root folder is missing or empty is not scanned, and
    /// says so: the series' post-add actions hang on that.
    #[test]
    fn a_scan_that_cannot_run_is_reported_as_skipped() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now();
        let root = std::env::temp_dir().join(format!("unarr_scan_skipped_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut series = crate::store::series::tests::sample(1);
        series.path = root.join("Series").to_string_lossy().into_owned();
        let env = db
            .call_blocking(move |conn| {
                conn.execute_batch(
                    "INSERT INTO quality_profiles (id, name, cutoff, items) VALUES (1, 'Any', 1, '[]');",
                )?;
                Environment::load(conn, now)
            })
            .unwrap();
        let skipped = |events: Events| {
            events
                .0
                .iter()
                .any(|e| matches!(e, ImportEvent::SeriesScanSkipped { series_id: 1 }))
        };
        // No root folder at all, then one with nothing in it.
        assert!(skipped(scan(&db, &env, &series, now, &mut |_| {}).unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(skipped(scan(&db, &env, &series, now, &mut |_| {}).unwrap()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn filters_extras_and_hidden_folders_like_the_originals() {
        let base = "/tv/Show";
        let paths: Vec<String> = [
            "/tv/Show/Season 01/Show.S01E01.mkv",
            "/tv/Show/Season 01/Extras/Show.S01E02.mkv",
            "/tv/Show/.hidden/Show.S01E03.mkv",
            "/tv/Show/Season 01/Show.S01E04-trailer.mkv",
            "/tv/Show/Season 01/._Show.S01E05.mkv",
            "/tv/Show/@eaDir/Show.S01E06.mkv",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            filter_paths(base, &paths, true),
            vec!["/tv/Show/Season 01/Show.S01E01.mkv".to_string()]
        );
        assert_eq!(filter_paths(base, &paths, false).len(), 3);
    }
}
