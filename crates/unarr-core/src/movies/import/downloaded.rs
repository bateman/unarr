//! `DownloadedMovieImportService`: a download's folder or file, or a drop
//! folder, turned into import decisions and imported.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::approve::{self, ImportedMovies};
use super::decide::{self, MovieEnvironment};
use super::local::{
    LocalMovie, MovieImportDecision, MovieImportRejection, MovieImportResult, MovieRejectionReason as R,
};
use super::sample;
use crate::db::Db;
use crate::downloadclients::DownloadClientItem;
use crate::mediafiles::decide::Probes;
use crate::mediafiles::downloaded::{ARCHIVE_EXTENSIONS, DANGEROUS_EXTENSIONS, EXECUTABLE_EXTENSIONS};
use crate::mediafiles::local::{ImportMode, ImportResultType};
use crate::mediafiles::sample::DetectSampleResult;
use crate::mediafiles::{extension, file_name, file_stem, scan};
use crate::movies::decision::StoreMovieLookup;
use crate::movies::downloads::monitor::movie_for_title;
use crate::movies::model::Movie;
use crate::movies::parser::parse_movie_title;
use crate::movies::store;
use crate::parser::media_extensions::is_media_extension;

fn has_extension(list: &[&str], path: &str) -> bool {
    let ext = extension(path);
    list.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

fn rejection_result(path: Option<&str>, reason: R, message: &str) -> MovieImportResult {
    let local = path.map(|p| LocalMovie {
        path: p.to_string(),
        ..Default::default()
    });
    MovieImportResult::new(
        MovieImportDecision::rejected(local, MovieImportRejection::new(reason, message)),
        vec![message.to_string()],
    )
}

fn only(result: MovieImportResult) -> ImportedMovies {
    ImportedMovies {
        results: vec![result],
        ..Default::default()
    }
}

/// `UnknownMovieResult`: the rejection always says "Unknown Movie"; the
/// result's own message may say which file.
fn unknown_movie(message: &str, file: Option<&str>) -> ImportedMovies {
    let local = file.map(|p| LocalMovie {
        path: p.to_string(),
        ..Default::default()
    });
    only(MovieImportResult::new(
        MovieImportDecision::rejected(local, MovieImportRejection::new(R::UnknownMovie, "Unknown Movie")),
        vec![message.to_string()],
    ))
}

/// `GetCleanedUpFolderName`.
fn cleaned_up_folder_name(folder: &str) -> String {
    folder.replace("_UNPACK_", "").replace("_FAILED_", "")
}

/// `ParsingService.GetMovie(title)`.
fn movie_named(db: &Db, title: &str) -> Option<Movie> {
    let title = title.to_string();
    db.call_blocking(move |conn| Ok(movie_for_title(&StoreMovieLookup { conn }, &title)))
        .ok()
        .flatten()
}

/// `ProcessRootFolder(directoryInfo)`: every subfolder, then every loose
/// video file, each on its own.
pub fn process_root_folder(
    db: &Db,
    env: &MovieEnvironment,
    path: &str,
    now: DateTime<Utc>,
) -> ImportedMovies {
    let mut out = ImportedMovies::default();
    let mut folders: Vec<String> = std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    folders.sort();
    for folder in folders {
        let done = process_folder(db, env, &folder, ImportMode::Auto, None, None, None, now);
        out.results.extend(done.results);
        out.events.extend(done.events);
    }
    for file in scan::video_files(path, false) {
        let done = process_file(db, env, &file, ImportMode::Auto, None, None, None, now);
        out.results.extend(done.results);
        out.events.extend(done.events);
    }
    out
}

/// `ProcessPath(path, importMode, movie, downloadClientItem)`.
/// `tracked_flags` are the indexer flags of the release the download was
/// tracked with, when it has one.
#[allow(clippy::too_many_arguments)]
pub fn process_path(
    db: &Db,
    env: &MovieEnvironment,
    path: &str,
    mode: ImportMode,
    movie: Option<&Movie>,
    item: Option<&DownloadClientItem>,
    tracked_flags: Option<i64>,
    now: DateTime<Utc>,
) -> ImportedMovies {
    tracing::debug!("Processing path: {path}");
    if Path::new(path).is_dir() {
        return process_folder(db, env, path, mode, movie, item, tracked_flags, now);
    }
    if Path::new(path).is_file() {
        return process_file(db, env, path, mode, movie, item, tracked_flags, now);
    }
    tracing::error!(
        "Import failed, path does not exist or is not accessible by Radarr: {path}. Ensure the path exists and the user running Radarr has the correct permissions to access this file/folder"
    );
    ImportedMovies::default()
}

/// `ShouldDeleteFolder`: nothing left in it but samples and small
/// archives.
pub fn should_delete_folder(folder: &str, movie: &Movie) -> bool {
    let video_files = scan::video_files(folder, true);
    let rar_files: Vec<String> = scan::files_in(folder, true)
        .into_iter()
        .filter(|f| extension(f).eq_ignore_ascii_case(".rar"))
        .collect();
    for video in &video_files {
        if parse_movie_title(file_name(video), false).is_none() {
            tracing::warn!("Unable to parse file on import: [{video}]");
            return false;
        }
        let probe = crate::mediafiles::mediainfo::probe(Path::new(video));
        if sample::is_sample(&movie.metadata, video, probe.as_ref()) != DetectSampleResult::Sample {
            tracing::warn!("Non-sample file detected: [{video}]");
            return false;
        }
    }
    if rar_files
        .iter()
        .any(|f| std::fs::metadata(f).is_ok_and(|m| m.len() > 10 * 1024 * 1024))
    {
        tracing::warn!("RAR file detected, will require manual cleanup");
        return false;
    }
    true
}

/// `CheckEmptyResultForIssue`: why a folder gave nothing to import.
fn check_empty_result_for_issue(folder: &str) -> Option<MovieImportResult> {
    let files = scan::files_in(folder, true);
    if files.iter().any(|f| has_extension(DANGEROUS_EXTENSIONS, f)) {
        return Some(rejection_result(
            None,
            R::DangerousFile,
            "Caution: Found potentially dangerous file",
        ));
    }
    if files.iter().any(|f| has_extension(EXECUTABLE_EXTENSIONS, f)) {
        return Some(rejection_result(
            None,
            R::ExecutableFile,
            "Caution: Found executable file",
        ));
    }
    if files.iter().any(|f| has_extension(ARCHIVE_EXTENSIONS, f)) {
        return Some(rejection_result(
            None,
            R::ArchiveFile,
            "Found archive file, might need to be extracted",
        ));
    }
    None
}

/// The decisions for `files`, their probes taken off the database thread.
#[allow(clippy::too_many_arguments)]
fn decide_and_import(
    db: &Db,
    env: &MovieEnvironment,
    files: Vec<String>,
    movie: &Movie,
    item: Option<&DownloadClientItem>,
    folder_title: Option<&str>,
    mode: ImportMode,
    tracked_flags: Option<i64>,
    now: DateTime<Utc>,
) -> ImportedMovies {
    let probes = Probes::probe_all(&files);
    let folder_info = folder_title.and_then(|t| parse_movie_title(t, false));
    let (env2, movie2, item2) = (env.clone(), movie.clone(), item.cloned());
    let decisions = db.call_blocking(move |conn| {
        decide::get_import_decisions(
            conn,
            &env2,
            &probes,
            &files,
            &movie2,
            item2.as_ref(),
            folder_info.as_ref(),
            true,
            true,
            tracked_flags,
        )
    });
    match decisions {
        Ok(decisions) => approve::import(db, env, decisions, true, item, mode, now),
        Err(e) => {
            tracing::error!(error = %e, "failed to decide the import of {}", movie.path);
            ImportedMovies::default()
        }
    }
}

/// `ProcessFolder`, with the movie known or found from the folder's name.
#[allow(clippy::too_many_arguments)]
fn process_folder(
    db: &Db,
    env: &MovieEnvironment,
    folder: &str,
    mode: ImportMode,
    movie: Option<&Movie>,
    item: Option<&DownloadClientItem>,
    tracked_flags: Option<i64>,
    now: DateTime<Utc>,
) -> ImportedMovies {
    let cleaned_up_name = cleaned_up_folder_name(file_name(folder.trim_end_matches('/')));
    let found;
    let movie = match movie {
        Some(m) => m,
        None => match movie_named(db, &cleaned_up_name) {
            Some(m) => {
                found = m;
                &found
            }
            None => {
                tracing::debug!("Unknown Movie {cleaned_up_name}");
                return unknown_movie("Unknown Movie", None);
            }
        },
    };
    let folder2 = folder.to_string();
    if db
        .call_blocking(move |conn| store::movie_path_exists(conn, &folder2))
        .unwrap_or(false)
    {
        tracing::warn!("Unable to process folder that is mapped to an existing movie");
        return only(rejection_result(
            None,
            R::MovieFolder,
            "Import path is mapped to a movie folder",
        ));
    }
    let video_files = super::scan::filter_paths(folder, &scan::video_files(folder, true), true);
    let mut imported = decide_and_import(
        db,
        env,
        video_files,
        movie,
        item,
        Some(&cleaned_up_name),
        mode,
        tracked_flags,
        now,
    );
    let mode = match mode {
        ImportMode::Auto if item.is_none_or(|i| i.can_move_files) => ImportMode::Move,
        ImportMode::Auto => ImportMode::Copy,
        other => other,
    };
    let any_imported = imported
        .results
        .iter()
        .any(|r| r.result() == ImportResultType::Imported);
    if mode == ImportMode::Move && any_imported && should_delete_folder(folder, movie) {
        tracing::debug!("Deleting folder after importing valid files");
        if let Err(e) = std::fs::remove_dir_all(folder) {
            tracing::debug!("Unable to delete folder after importing: {e}");
        }
    } else if imported.results.is_empty() {
        imported.results.extend(check_empty_result_for_issue(folder));
    }
    imported
}

/// `ProcessFile`, with the movie known or found from the file's name.
#[allow(clippy::too_many_arguments)]
fn process_file(
    db: &Db,
    env: &MovieEnvironment,
    file: &str,
    mode: ImportMode,
    movie: Option<&Movie>,
    item: Option<&DownloadClientItem>,
    tracked_flags: Option<i64>,
    now: DateTime<Utc>,
) -> ImportedMovies {
    let found;
    let movie = match movie {
        Some(m) => m,
        None => match movie_named(db, file_stem(file)) {
            Some(m) => {
                found = m;
                &found
            }
            None => {
                tracing::debug!("Unknown Movie for file: {}", file_name(file));
                return unknown_movie(
                    &format!("Unknown Movie for file: {}", file_name(file)),
                    Some(file),
                );
            }
        },
    };
    if file_stem(file).starts_with("._") {
        tracing::debug!("[{file}] starts with '._', skipping");
        return only(rejection_result(
            Some(file),
            R::InvalidFilePath,
            "Invalid video file, filename starts with '._'",
        ));
    }
    let ext = extension(file);
    if has_extension(DANGEROUS_EXTENSIONS, file) {
        return only(rejection_result(
            Some(file),
            R::DangerousFile,
            &format!("Caution: Found potentially dangerous file with extension: {ext}"),
        ));
    }
    if has_extension(EXECUTABLE_EXTENSIONS, file) {
        return only(rejection_result(
            Some(file),
            R::ExecutableFile,
            &format!("Caution: Found executable file with extension: '{ext}'"),
        ));
    }
    if ext.trim().is_empty() || !is_media_extension(ext) {
        tracing::debug!("[{file}] has an unsupported extension: '{ext}'");
        return only(rejection_result(
            Some(file),
            R::UnsupportedExtension,
            &format!("Invalid video file, unsupported extension: '{ext}'"),
        ));
    }
    decide_and_import(
        db,
        env,
        vec![file.to_string()],
        movie,
        item,
        None,
        mode,
        tracked_flags,
        now,
    )
}
