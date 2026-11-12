//! `DiskScanService` for movies, with `MediaFileTableCleanupService` and
//! `UpdateMediaInfoService`: a movie's folder walked, the files the
//! library does not know imported where they are, the rows whose file is
//! gone removed, the ones whose size changed probed again.

use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use fancy_regex::Regex;
use rusqlite::Connection;

use super::approve;
use super::decide::{self, MovieEnvironment};
use super::deletion;
use super::events::{MovieEvents, MovieImportEvent};
use crate::db::Db;
use crate::mediafiles::approve::best_root_folder_path;
use crate::mediafiles::decide::Probes;
use crate::mediafiles::local::{DeleteReason, ImportMode};
use crate::mediafiles::scan::{non_video_files, video_files};
use crate::mediafiles::{file_name, join, mediainfo, path_equals, relative_path, transfer};
use crate::movies::model::{Movie, MovieFile};
use crate::movies::store;

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).expect("a valid pattern"))
        }
    };
}

// Radarr's patterns: Sonarr's, with a `Sample` folder excluded as well as
// a `Samples` one.
regex!(
    excluded_extras_subfolder,
    r"(?i)(?:\\|\/|^)(?:extras|extrafanart|behind the scenes|deleted scenes|featurettes|interviews|other|scenes|sample[s]?|shorts|trailers)(?:\\|\/)"
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

/// `FilterPaths(basePath, paths, filterExtras)`.
pub fn filter_paths(base_path: &str, paths: &[String], filter_extras: bool) -> Vec<String> {
    paths
        .iter()
        .filter(|p| !matches(excluded_subfolders(), relative_path(base_path, p)))
        .filter(|p| !matches(excluded_files(), file_name(p)))
        .filter(|p| {
            !filter_extras
                || (!matches(excluded_extras_subfolder(), relative_path(base_path, p))
                    && !matches(excluded_extra_files(), file_name(p)))
        })
        .cloned()
        .collect()
}

/// `MediaFileTableCleanupService.Clean`: the rows whose file the disk no
/// longer has go, as `MissingFromDisk`.
pub fn clean_media_files(
    conn: &Connection,
    movie: &Movie,
    files_on_disk: &[String],
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieImportEvent>> {
    let mut events = Vec::new();
    for file in store::movie_files_of(conn, movie.id)? {
        let path = join(&movie.path, &file.relative_path);
        if !files_on_disk.iter().any(|f| path_equals(f, &path)) {
            tracing::debug!("File [{path}] no longer exists on disk, removing from db");
            let ev = deletion::delete_file(conn, movie, &file, DeleteReason::MissingFromDisk, now)?;
            events.push(MovieImportEvent::FileDeleted(Box::new(ev)));
        }
    }
    Ok(events)
}

/// `UpdateMediaInfoService.Update(movieFile, movie)`: the file probed
/// again and its row written; false when media info is off or the probe
/// gave nothing, and the caller writes the row.
fn update_media_info(
    db: &Db,
    env: &MovieEnvironment,
    file: &mut MovieFile,
    movie: &Movie,
) -> anyhow::Result<bool> {
    if !env.config.enable_media_info {
        tracing::debug!("MediaInfo is disabled");
        return Ok(false);
    }
    let path = join(&movie.path, &file.relative_path);
    match mediainfo::probe(Path::new(&path)) {
        Some(info) => {
            file.media_info = Some(info.model);
            let file2 = file.clone();
            db.call_blocking(move |conn| store::update_movie_file(conn, &file2))?;
            tracing::debug!("Updated MediaInfo for '{path}'");
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `UpdateMediaInfoService.Handle(MovieScannedEvent)`: the movie's files
/// that have no media info, or one of a schema older than the least still
/// read, are probed again after a scan that ran. Returns how many got
/// theirs. A file whose probe failed at import, or a row that came from
/// the original's database without media info, gets it this way.
pub fn backfill_media_info(
    db: &Db,
    env: &MovieEnvironment,
    movie: &Movie,
    events: &[MovieImportEvent],
) -> usize {
    let scanned = events
        .iter()
        .any(|e| matches!(e, MovieImportEvent::Scanned { movie_id, .. } if *movie_id == movie.id));
    if !scanned {
        return 0;
    }
    if !env.config.enable_media_info {
        tracing::debug!("MediaInfo is disabled");
        return 0;
    }
    let id = movie.id;
    let files = match db.call_blocking(move |conn| store::movie_files_of(conn, id)) {
        Ok(files) => files,
        Err(e) => {
            tracing::error!(
                error = %format!("{e:#}"),
                "UpdateMediaInfoService failed while processing [MovieScannedEvent]"
            );
            return 0;
        }
    };
    let stale: Vec<MovieFile> = files
        .into_iter()
        .filter(|file| media_info_is_stale(file.media_info.as_ref()))
        .collect();
    // Without ffprobe every probe would fail, and say so, at every scan.
    if stale.is_empty() || !mediainfo::ffprobe_available() {
        return 0;
    }
    let mut updated = 0;
    for mut file in stale {
        let path = join(&movie.path, &file.relative_path);
        if !Path::new(&path).is_file() {
            tracing::debug!("Can't update MediaInfo because '{path}' does not exist");
            continue;
        }
        match update_media_info(db, env, &mut file, movie) {
            Ok(true) => updated += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "could not store the media info of '{path}'"),
        }
    }
    updated
}

/// No media info, or one older than the least the probe's own schema
/// still reads. The probe is one for both applications and stamps what it
/// writes with Sonarr's revision (11), below Radarr's minimum (14): held
/// to Radarr's number, every file Unarr probed would be probed again at
/// each scan.
fn media_info_is_stale(info: Option<&serde_json::Value>) -> bool {
    let revision = info
        .filter(|info| !info.is_null())
        .map(|info| info.get("schemaRevision").and_then(|r| r.as_i64()).unwrap_or(0));
    revision.is_none_or(|r| r < mediainfo::MINIMUM_MEDIA_INFO_SCHEMA_REVISION)
}

/// `Scan(movie)`. File work: call it off the async runtime.
pub fn scan(
    db: &Db,
    env: &MovieEnvironment,
    movie: &Movie,
    now: DateTime<Utc>,
    progress: &mut dyn FnMut(String),
) -> anyhow::Result<MovieEvents> {
    let mut events = MovieEvents::default();
    let root_folders = db.call_blocking(|conn| crate::store::catalog::list_root_folders(conn))?;
    let root_folder = best_root_folder_path(&root_folders, &movie.path);
    let title = movie.metadata.title.as_str();
    let folder_exists = Path::new(&movie.path).is_dir();
    if !folder_exists {
        if !Path::new(&root_folder).is_dir() {
            tracing::warn!("Movie's root folder ({root_folder}) doesn't exist.");
            events.push(MovieImportEvent::ScanSkipped { movie_id: movie.id });
            return Ok(events);
        }
        if transfer::folder_empty(Path::new(&root_folder)) {
            tracing::warn!(
                "Movie's root folder ({root_folder}) is empty. Rescan will not update movies as a failsafe."
            );
            events.push(MovieImportEvent::ScanSkipped { movie_id: movie.id });
            return Ok(events);
        }
    }
    progress(format!("Scanning disk for {title}"));
    if !folder_exists {
        if env.config.create_empty_series_folders {
            if env.config.delete_empty_folders {
                tracing::debug!(
                    "Not creating missing movie folder: {} because delete empty movie folders is enabled",
                    movie.path
                );
            } else {
                tracing::debug!("Creating missing movie folder: {}", movie.path);
                if let Err(e) = transfer::create_folder(&env.config, Path::new(&movie.path)) {
                    tracing::warn!(error = %e, "unable to create the movie folder");
                }
            }
        } else {
            tracing::debug!("Movie's folder doesn't exist: {}", movie.path);
        }
        let movie2 = movie.clone();
        events
            .0
            .extend(db.call_blocking(move |conn| clean_media_files(conn, &movie2, &[], now))?);
        tracing::info!("Completed scanning disk for {title}");
        events.push(MovieImportEvent::Scanned {
            movie_id: movie.id,
            possible_extra_files: Vec::new(),
        });
        return Ok(events);
    }
    let media_files = filter_paths(&movie.path, &video_files(&movie.path, true), true);
    let (movie2, files2) = (movie.clone(), media_files.clone());
    let (cleanup_events, movie_files, fresh) = db.call_blocking(move |conn| {
        tracing::debug!("{} Cleaning up media files in DB", movie2.metadata.title);
        let ev = clean_media_files(conn, &movie2, &files2, now)?;
        Ok((
            ev,
            store::movie_files_of(conn, movie2.id)?,
            store::get_movie(conn, movie2.id)?,
        ))
    })?;
    for e in &cleanup_events {
        if let MovieImportEvent::FileDeleted(deleted) = e {
            deletion::clean_folders(&env.config, deleted);
        }
    }
    events.0.extend(cleanup_events);
    // The movie as the cleanup left it: no longer pointing at a file gone.
    let movie = fresh.as_ref().unwrap_or(movie);
    let unmapped = decide::filter_existing(&media_files, &movie_files, movie);
    let probes = Probes::probe_all(&unmapped);
    let (env2, movie2, unmapped2) = (env.clone(), movie.clone(), unmapped.clone());
    let decisions = db.call_blocking(move |conn| {
        decide::get_import_decisions(
            conn, &env2, &probes, &unmapped2, &movie2, None, None, false, false, None,
        )
    })?;
    let imported = approve::import(db, env, decisions, false, None, ImportMode::Auto, now);
    events.extend(imported.events);
    // Known files whose size changed get their size and media info back.
    let mut to_update = Vec::new();
    for mut file in movie_files {
        let path = join(&movie.path, &file.relative_path);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let size = meta.len() as i64;
        if file.size == size {
            continue;
        }
        file.size = size;
        if !update_media_info(db, env, &mut file, movie)? {
            to_update.push(file);
        }
    }
    if !to_update.is_empty() {
        db.call_blocking(move |conn| {
            for file in &to_update {
                store::update_movie_file(conn, file)?;
            }
            Ok(())
        })?;
    }
    let possible_extra_files = filter_paths(&movie.path, &non_video_files(&movie.path, true), true);
    // `RemoveEmptyMovieFolder`.
    if env.config.delete_empty_folders {
        transfer::remove_empty_subfolders(Path::new(&movie.path));
        if transfer::folder_empty(Path::new(&movie.path)) {
            if let Err(e) = std::fs::remove_dir_all(&movie.path) {
                tracing::debug!(error = %e, "unable to delete the empty movie folder {}", movie.path);
            }
        }
    }
    tracing::info!("Completed scanning disk for {title}");
    events.push(MovieImportEvent::Scanned {
        movie_id: movie.id,
        possible_extra_files,
    });
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::media_info_is_stale;
    use crate::mediafiles::mediainfo;
    use serde_json::json;

    #[test]
    fn files_without_media_info_or_with_an_old_one_are_probed_again() {
        assert!(media_info_is_stale(None));
        assert!(media_info_is_stale(Some(&json!(null))));
        assert!(media_info_is_stale(Some(&json!({"videoFormat": "h264"}))));
        assert!(media_info_is_stale(Some(&json!({
            "schemaRevision": mediainfo::MINIMUM_MEDIA_INFO_SCHEMA_REVISION - 1
        }))));
        // What the probe writes, and what the original's database holds.
        assert!(!media_info_is_stale(Some(&json!({
            "schemaRevision": mediainfo::CURRENT_MEDIA_INFO_SCHEMA_REVISION
        }))));
        assert!(!media_info_is_stale(Some(&json!({"schemaRevision": 14}))));
    }
}
