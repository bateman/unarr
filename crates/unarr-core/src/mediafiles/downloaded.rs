//! `DownloadedEpisodesImportService`: a completed download's folder (or a
//! lone file, or a drop folder of them) turned into import decisions and
//! imported, the folder removed when only samples remain.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::approve::{self, Imported};
use super::decide::{self, Environment, Probes};
use super::local::{
    ImportDecision, ImportMode, ImportRejection, ImportResult, LocalEpisode, RejectionReason as R,
};
use super::sample::{self, DetectSampleResult};
use super::scan;
use crate::db::Db;
use crate::decision::lookup::StoreLookup;
use crate::decision::mapping;
use crate::downloadclients::DownloadClientItem;
use crate::model::Series;
use crate::parser;
use crate::parser::media_extensions::is_media_extension;
use crate::store;

/// `FileExtensions.ArchiveExtensions`.
pub const ARCHIVE_EXTENSIONS: &[&str] = &[
    ".7z", ".bz2", ".gz", ".r00", ".rar", ".tar.bz2", ".tar.gz", ".tar", ".tb2", ".tbz2", ".tgz", ".zip",
];
/// `FileExtensions.DangerousExtensions`.
pub const DANGEROUS_EXTENSIONS: &[&str] = &[".arj", ".lnk", ".lzh", ".ps1", ".scr", ".vbs", ".zipx"];
/// `FileExtensions.ExecutableExtensions`.
pub const EXECUTABLE_EXTENSIONS: &[&str] = &[".bat", ".cmd", ".exe", ".sh"];

fn has_extension(list: &[&str], path: &str) -> bool {
    let ext = super::extension(path);
    list.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

fn rejection_result(path: Option<&str>, reason: R, message: &str) -> ImportResult {
    let local = path.map(|p| LocalEpisode {
        path: p.to_string(),
        ..Default::default()
    });
    ImportResult::failed(
        ImportDecision::rejected(local, ImportRejection::new(reason, message)),
        vec![message.to_string()],
    )
}

/// `ProcessRootFolder(directoryInfo)`: every subfolder, then every loose
/// video file, each on its own.
pub fn process_root_folder(db: &Db, env: &Environment, path: &str, now: DateTime<Utc>) -> Imported {
    let mut out = Imported::default();
    let mut folders = super::transfer::directories_under(Path::new(path));
    folders.sort();
    for folder in folders {
        let folder = folder.to_string_lossy().into_owned();
        merge(
            &mut out,
            process_folder(db, env, &folder, ImportMode::Auto, None, None, now),
        );
    }
    for file in scan::video_files(path, false) {
        merge(
            &mut out,
            process_file(db, env, &file, ImportMode::Auto, None, None, now),
        );
    }
    out
}

fn merge(into: &mut Imported, from: Imported) {
    into.results.extend(from.results);
    into.events.extend(from.events);
    into.rescan_series.extend(from.rescan_series);
}

/// `ProcessPath(path, importMode, series, downloadClientItem)`.
pub fn process_path(
    db: &Db,
    env: &Environment,
    path: &str,
    mode: ImportMode,
    series: Option<&Series>,
    item: Option<&DownloadClientItem>,
    now: DateTime<Utc>,
) -> Imported {
    tracing::debug!("Processing path: {path}");
    if Path::new(path).is_dir() {
        return process_folder(db, env, path, mode, series, item, now);
    }
    if Path::new(path).is_file() {
        return process_file(db, env, path, mode, series, item, now);
    }
    tracing::error!(
        "Import failed, path does not exist or is not accessible by Sonarr: {path}. Ensure the path exists and the user running Sonarr has the correct permissions to access this file/folder"
    );
    Imported::default()
}

/// `ShouldDeleteFolder(directoryInfo, series)`: only when every video file
/// is a sample and no sizeable archive is left.
pub fn should_delete_folder(folder: &str, series: &Series) -> bool {
    let video_files = scan::video_files(folder, true);
    let rar_files: Vec<String> = scan::files_in(folder, true)
        .into_iter()
        .filter(|f| super::extension(f).eq_ignore_ascii_case(".rar"))
        .collect();
    for video in &video_files {
        let Some(parsed) = parser::parse_title(super::file_name(video)) else {
            tracing::warn!("Unable to parse file on import: [{video}]");
            return false;
        };
        if sample::is_sample_file(series, video, parsed.is_possible_special_episode())
            != DetectSampleResult::Sample
        {
            tracing::warn!("Non-sample file detected: [{video}]");
            return false;
        }
    }
    if rar_files.iter().any(|f| {
        std::fs::metadata(f)
            .map(|m| m.len() > 10 * 1024 * 1024)
            .unwrap_or(false)
    }) {
        tracing::warn!("RAR file detected, will require manual cleanup");
        return false;
    }
    true
}

/// `GetCleanedUpFolderName`.
fn cleaned_up_folder_name(folder: &str) -> String {
    folder.replace("_UNPACK_", "").replace("_FAILED_", "")
}

fn series_for_title(db: &Db, title: &str) -> Option<Series> {
    let title = title.to_string();
    db.call_blocking(move |conn| {
        let lookup = StoreLookup::new(conn);
        Ok(mapping::get_series(&lookup, &title).ok().flatten())
    })
    .ok()
    .flatten()
}

fn process_folder(
    db: &Db,
    env: &Environment,
    folder: &str,
    mode: ImportMode,
    series: Option<&Series>,
    item: Option<&DownloadClientItem>,
    now: DateTime<Utc>,
) -> Imported {
    let name = super::file_name(folder).to_string();
    let series = match series {
        Some(s) => s.clone(),
        None => {
            let cleaned = cleaned_up_folder_name(&name);
            match series_for_title(db, &cleaned) {
                Some(s) => s,
                None => {
                    tracing::debug!("Unknown Series {cleaned}");
                    return Imported {
                        results: vec![rejection_result(None, R::UnknownSeries, "Unknown Series")],
                        ..Default::default()
                    };
                }
            }
        }
    };
    let folder_owned = folder.to_string();
    let mapped = db
        .call_blocking(move |conn| store::series::path_exists(conn, &folder_owned))
        .unwrap_or(false);
    if mapped {
        tracing::warn!("Unable to process folder that is mapped to an existing series");
        return Imported {
            results: vec![rejection_result(
                None,
                R::SeriesFolder,
                "Import path is mapped to a series folder",
            )],
            ..Default::default()
        };
    }
    let folder_info = parser::parse_title(&name);
    let video_files = scan::filter_paths(folder, &scan::video_files(folder, true), true);
    let mut imported = import_files(
        db,
        env,
        &video_files,
        &series,
        item,
        folder_info.as_ref(),
        mode,
        now,
    );
    let effective = match mode {
        ImportMode::Auto => {
            if item.is_none_or(|i| i.can_move_files) {
                ImportMode::Move
            } else {
                ImportMode::Copy
            }
        }
        other => other,
    };
    let any_imported = imported.results.iter().any(|r| r.episode_file.is_some());
    if effective == ImportMode::Move && any_imported && should_delete_folder(folder, &series) {
        tracing::debug!("Deleting folder after importing valid files");
        if let Err(e) = std::fs::remove_dir_all(folder) {
            tracing::debug!(error = %e, "Unable to delete folder after importing: {folder}");
        }
    } else if imported.results.is_empty() {
        if let Some(r) = check_empty_result_for_issue(folder) {
            imported.results.push(r);
        }
    }
    imported
}

fn process_file(
    db: &Db,
    env: &Environment,
    file: &str,
    mode: ImportMode,
    series: Option<&Series>,
    item: Option<&DownloadClientItem>,
    now: DateTime<Utc>,
) -> Imported {
    let name = super::file_name(file);
    let series = match series {
        Some(s) => s.clone(),
        None => match series_for_title(db, super::file_stem(file)) {
            Some(s) => s,
            None => {
                tracing::debug!("Unknown Series for file: {name}");
                return Imported {
                    results: vec![ImportResult::failed(
                        ImportDecision::rejected(
                            Some(LocalEpisode {
                                path: file.to_string(),
                                ..Default::default()
                            }),
                            ImportRejection::new(R::UnknownSeries, "Unknown Series"),
                        ),
                        vec![format!("Unknown Series for file: {name}")],
                    )],
                    ..Default::default()
                };
            }
        },
    };
    if super::file_stem(file).starts_with("._") {
        tracing::debug!("[{file}] starts with '._', skipping");
        return Imported {
            results: vec![rejection_result(
                Some(file),
                R::InvalidFilePath,
                "Invalid video file, filename starts with '._'",
            )],
            ..Default::default()
        };
    }
    let extension = super::extension(file);
    if has_extension(DANGEROUS_EXTENSIONS, file) {
        let message = format!("Caution: Found potentially dangerous file with extension: {extension}");
        return Imported {
            results: vec![rejection_result(Some(file), R::DangerousFile, &message)],
            ..Default::default()
        };
    }
    if has_extension(EXECUTABLE_EXTENSIONS, file) {
        let message = format!("Caution: Found executable file with extension: '{extension}'");
        return Imported {
            results: vec![rejection_result(Some(file), R::ExecutableFile, &message)],
            ..Default::default()
        };
    }
    if extension.trim().is_empty() || !is_media_extension(extension) {
        tracing::debug!("[{file}] has an unsupported extension: '{extension}'");
        let message = format!("Invalid video file, unsupported extension: '{extension}'");
        return Imported {
            results: vec![rejection_result(Some(file), R::UnsupportedExtension, &message)],
            ..Default::default()
        };
    }
    import_files(db, env, &[file.to_string()], &series, item, None, mode, now)
}

/// The decisions for `files` (probed first, off the database thread), then
/// `ImportApprovedEpisodes.Import(decisions, true, item, mode)`.
#[allow(clippy::too_many_arguments)]
fn import_files(
    db: &Db,
    env: &Environment,
    files: &[String],
    series: &Series,
    item: Option<&DownloadClientItem>,
    folder_info: Option<&parser::title::ParsedEpisodeInfo>,
    mode: ImportMode,
    now: DateTime<Utc>,
) -> Imported {
    let probes = Probes::probe_all(files);
    let (files2, series2, item2, folder_info2) = (
        files.to_vec(),
        series.clone(),
        item.cloned(),
        folder_info.cloned(),
    );
    let (catalog, config, naming) = (env.catalog.clone(), env.config.clone(), env.naming.clone());
    let decisions = db.call_blocking(move |conn| {
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
            &files2,
            &series2,
            item2.as_ref(),
            folder_info2.as_ref(),
            true,
            true,
            None,
        )
    });
    let decisions = match decisions {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "unable to make import decisions");
            return Imported::default();
        }
    };
    approve::import(db, env, decisions, true, item, mode, now)
}

/// `CheckEmptyResultForIssue(folder)`.
fn check_empty_result_for_issue(folder: &str) -> Option<ImportResult> {
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
