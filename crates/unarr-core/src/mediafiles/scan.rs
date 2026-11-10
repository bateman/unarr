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
            return Ok(events);
        }
        if transfer::folder_empty(Path::new(&root_folder)) {
            tracing::warn!("Series' root folder ({root_folder}) is empty.");
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
