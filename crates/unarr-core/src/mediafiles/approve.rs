//! `ImportApprovedEpisodes` with `UpgradeMediaFileService` and
//! `EpisodeFileMovingService`: the approved decisions become episode files.
//! The old files go to the recycling bin, the new one is moved (or linked,
//! or copied) under the name the naming settings build, the row is added,
//! the episodes linked, the history written. File work happens on the
//! caller's thread; each database step is one call on the database thread.

use std::path::Path;

use chrono::{DateTime, NaiveDate, Timelike, Utc};
use rusqlite::Connection;

use super::decide::Environment;
use super::deletion;
use super::events::{self, EpisodeImportFailed, EpisodeImported, Events, ImportEvent};
use super::local::{
    DeleteReason, DeletedEpisodeFile, ImportDecision, ImportMode, ImportResult, LocalEpisode,
};
use super::transfer::{self, TransferError, TransferMode};
use crate::db::Db;
use crate::decision::upgradable;
use crate::downloadclients::DownloadClientItem;
use crate::downloads::grab;
use crate::model::{
    CustomFormat, Episode, EpisodeFile, HistoryEvent, NamingConfig, ReleaseType, RootFolder, Series,
};
use crate::naming::{self, NamingCustomFormat, NamingEpisode, NamingFile, NamingMediaInfo, NamingSeries};
use crate::store;
use crate::store::config::{FileDateType, MediaManagementConfig};

/// What `Import` hands back: the results in the originals' order, the
/// events for the relays, and the series a destination clash asks to
/// rescan (`RescanSeriesCommand`).
#[derive(Debug, Default)]
pub struct Imported {
    pub results: Vec<ImportResult>,
    pub events: Events,
    pub rescan_series: Vec<i64>,
}

/// The exceptions `Import` catches, with their result messages; the
/// renames tell `SameFile` and `DestinationExists` apart too.
#[derive(Debug)]
pub enum Failure {
    RootFolderMissing(String),
    DestinationExists(String),
    SameFile(String),
    RecycleBin,
    Other(String),
}

impl Failure {
    /// What the log says.
    pub fn detail(&self) -> String {
        match self {
            Failure::RootFolderMissing(m)
            | Failure::Other(m)
            | Failure::DestinationExists(m)
            | Failure::SameFile(m) => m.clone(),
            other => other.message().to_string(),
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Failure::RootFolderMissing(_) => "Failed to import episode, Root folder missing.",
            Failure::DestinationExists(_) => "Failed to import episode, Destination already exists.",
            Failure::RecycleBin => {
                "Failed to import episode, unable to move existing file to the Recycle Bin."
            }
            Failure::Other(_) | Failure::SameFile(_) => "Failed to import episode",
        }
    }
}

impl From<TransferError> for Failure {
    fn from(e: TransferError) -> Failure {
        match e {
            TransferError::DestinationExists(p) => Failure::DestinationExists(p),
            TransferError::SameFile(p) => Failure::SameFile(p),
            TransferError::RootFolderMissing(m) => Failure::RootFolderMissing(m),
            TransferError::RecycleBin { .. } => Failure::RecycleBin,
            other => Failure::Other(other.to_string()),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Failure {
        Failure::Other(format!("{e:#}"))
    }
}

/// `NamingSeries` for a series.
pub fn naming_series(series: &Series) -> NamingSeries<'_> {
    NamingSeries {
        title: &series.title,
        year: series.year,
        series_type: series.series_type,
        imdb_id: series.imdb_id.as_deref(),
        tvdb_id: series.tvdb_id,
        tv_maze_id: series.tv_maze_id,
        tmdb_id: series.tmdb_id,
    }
}

/// Air dates as the naming tokens read them, owned so they outlive the
/// borrows `naming_episodes` takes.
pub fn air_dates(episodes: &[Episode]) -> Vec<Option<String>> {
    episodes
        .iter()
        .map(|e| e.air_date.map(|d: NaiveDate| d.format("%Y-%m-%d").to_string()))
        .collect()
}

pub fn naming_episodes<'a>(
    episodes: &'a [Episode],
    air_dates: &'a [Option<String>],
) -> Vec<NamingEpisode<'a>> {
    episodes
        .iter()
        .zip(air_dates)
        .map(|(e, air_date)| NamingEpisode {
            season_number: e.season_number,
            episode_number: e.episode_number,
            title: e.title.as_deref().unwrap_or(""),
            air_date: air_date.as_deref(),
            absolute_episode_number: e.absolute_episode_number,
        })
        .collect()
}

pub fn naming_formats(formats: &[CustomFormat]) -> Vec<NamingCustomFormat<'_>> {
    formats
        .iter()
        .map(|f| NamingCustomFormat {
            name: &f.name,
            include_when_renaming: f.include_when_renaming,
        })
        .collect()
}

/// `BuildSeasonPath(series, seasonNumber)`.
pub fn season_path(series: &Series, season_number: i64, config: &NamingConfig) -> String {
    if !series.season_folder {
        return series.path.clone();
    }
    let folder = naming::season_folder(&naming_series(series), season_number, config);
    let folder = naming::clean_file_name(&folder, config);
    super::join(&series.path, &folder)
}

/// `BuildFilePath(episodes, series, episodeFile, extension, namingConfig, customFormats)`.
/// `current_path` is the file as it is now (the drop folder path for a new
/// download, the library path for a rename), for the original-name tokens.
#[allow(clippy::too_many_arguments)]
pub fn build_file_path(
    env: &Environment,
    series: &Series,
    episodes: &[Episode],
    file: &EpisodeFile,
    current_path: &str,
    extension: &str,
    formats: &[CustomFormat],
    is_new: bool,
) -> Result<String, naming::NamingError> {
    let dates = air_dates(episodes);
    let naming_eps = naming_episodes(episodes, &dates);
    // `GetSceneOrFileName`: the scene name, else the file's own name.
    let scene_or_file_name = file
        .scene_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| super::file_name(current_path).to_string());
    let media_info: Option<NamingMediaInfo> = file
        .media_info
        .as_ref()
        .map(|m| super::format::naming_media_info(m, Some(&scene_or_file_name)));
    let definition = env.catalog.definition(file.quality.quality_id);
    let naming_file = NamingFile {
        quality: file.quality,
        quality_title: &definition.title,
        relative_path: current_path,
        scene_name: file.scene_name.as_deref(),
        release_group: file.release_group.as_deref(),
        release_hash: file.release_hash.as_deref(),
        media_info: media_info.as_ref(),
        is_new,
    };
    let season = episodes.first().map(|e| e.season_number).unwrap_or(0);
    let season_path = season_path(series, season, &env.naming);
    let file_name = naming::build_file_name(
        &naming_eps,
        &naming_series(series),
        &naming_file,
        extension,
        &env.naming,
        &naming_formats(formats),
    )?;
    Ok(super::join(&season_path, &file_name))
}

/// `RootFolderService.GetBestRootFolderPath`: the deepest root folder the
/// path is under, else its parent folder.
pub fn best_root_folder_path(root_folders: &[RootFolder], path: &str) -> String {
    root_folders
        .iter()
        .filter(|r| super::is_parent_path(&r.path, path))
        .max_by_key(|r| r.path.len())
        .map(|r| r.path.clone())
        .unwrap_or_else(|| super::parent_path(path).unwrap_or("").to_string())
}

/// `EnsureEpisodeFolder`: the series, season and episode folders created
/// as needed, once the root folder is known to exist.
fn ensure_episode_folder(
    env: &Environment,
    root_folders: &[RootFolder],
    series: &Series,
    season_number: i64,
    file_path: &str,
) -> Result<(), Failure> {
    let episode_folder = super::parent_path(file_path).unwrap_or("").to_string();
    let season_folder = season_path(series, season_number, &env.naming);
    let series_folder = series.path.as_str();
    let root_folder = best_root_folder_path(root_folders, series_folder);
    if root_folder.trim().is_empty() {
        return Err(Failure::RootFolderMissing(format!(
            "Root folder was not found, '{series_folder}' is not a subdirectory of a defined root folder."
        )));
    }
    if !Path::new(&root_folder).is_dir() {
        return Err(Failure::RootFolderMissing(format!(
            "Root folder '{root_folder}' was not found."
        )));
    }
    for folder in [series_folder, season_folder.as_str(), episode_folder.as_str()] {
        if !Path::new(folder).is_dir() {
            if let Err(e) = transfer::create_folder(&env.config, Path::new(folder)) {
                tracing::error!(error = %e, "Unable to create directory: {folder}");
            }
        }
    }
    Ok(())
}

/// `UpdateEpisodeFileService.ChangeFileDate`.
pub fn change_file_date(
    config: &MediaManagementConfig,
    path: &str,
    series: &Series,
    episodes: &[Episode],
) -> bool {
    let Some(first) = episodes.first() else {
        return false;
    };
    let current = transfer::last_write_time(Path::new(path));
    let target: Option<DateTime<Utc>> = match config.file_date {
        // The movie dates are Radarr's values of the setting.
        FileDateType::None | FileDateType::Cinemas | FileDateType::Release => None,
        // The original keeps the fraction of a second the file's date had
        // (recorded on Sonarr 4.0.20: a file dated `…:00.250` is dated
        // `22:00:00.250` after the scan), which the local date does not.
        FileDateType::UtcAirDate => first.air_date_utc.map(|utc| {
            let utc = utc.max(DateTime::<Utc>::UNIX_EPOCH);
            if current == Some(utc) {
                return utc;
            }
            let fraction = current.map_or(0, |c| c.timestamp_subsec_nanos());
            utc.with_nanosecond(fraction).unwrap_or(utc)
        }),
        FileDateType::LocalAirDate => {
            let (Some(date), Some(time)) = (
                first.air_date,
                series.air_time.as_deref().filter(|t| !t.trim().is_empty()),
            ) else {
                return false;
            };
            let parsed = chrono::NaiveTime::parse_from_str(time.trim(), "%H:%M")
                .or_else(|_| chrono::NaiveTime::parse_from_str(time.trim(), "%H:%M:%S"))
                .ok()
                .map(|t| date.and_time(t));
            let Some(naive) = parsed else {
                tracing::debug!("Could not create valid date to change file [{path}]");
                return false;
            };
            let local = naive
                .and_local_timezone(chrono::Local)
                .single()
                .map(|l| l.with_timezone(&Utc))
                .unwrap_or_else(|| naive.and_utc());
            Some(local.max(DateTime::<Utc>::UNIX_EPOCH))
        }
    };
    let Some(target) = target else {
        return false;
    };
    // `DateTime.Equals`: to the tick, not to the second.
    if current == Some(target) {
        return false;
    }
    if let Err(e) = transfer::try_set_last_write_time(Path::new(path), target) {
        tracing::warn!(error = %e, "Unable to set date of file [{path}]");
        return false;
    }
    tracing::debug!("Date of file [{path}] changed to '{target}'");
    true
}

/// `TransferFile` of `EpisodeFileMovingService`: the file to its place in
/// the library, the relative path set, the dates and permissions applied.
fn transfer_into_library(
    env: &Environment,
    series: &Series,
    episodes: &[Episode],
    file: &mut EpisodeFile,
    source: &str,
    destination: &str,
    mode: TransferMode,
) -> Result<(), Failure> {
    if !Path::new(source).is_file() {
        return Err(Failure::Other(format!(
            "Episode file path does not exist: {source}"
        )));
    }
    if source == destination {
        return Err(Failure::SameFile(source.to_string()));
    }
    file.relative_path = super::relative_path(&series.path, destination).to_string();
    transfer::transfer_file(Path::new(source), Path::new(destination), mode)?;
    change_file_date(&env.config, destination, series, episodes);
    transfer::set_last_write_time(Path::new(&series.path), file.date_added);
    if series.season_folder {
        if let Some(season_folder) = super::parent_path(destination) {
            transfer::set_last_write_time(Path::new(season_folder), file.date_added);
        }
    }
    transfer::set_permissions(&env.config, Path::new(destination));
    Ok(())
}

/// `MoveEpisodeFile(episodeFile, series)`: a library file moved to where
/// the naming settings put it now (the renames). `file.relative_path` is
/// updated; the row is the caller's to write.
pub fn move_episode_file(
    env: &Environment,
    root_folders: &[RootFolder],
    series: &Series,
    episodes: &[Episode],
    file: &mut EpisodeFile,
) -> Result<(), Failure> {
    let current = super::join(&series.path, &file.relative_path);
    let formats = crate::decision::formats::file_formats(&env.catalog, file, series);
    let destination = build_file_path(
        env,
        series,
        episodes,
        file,
        &current,
        super::extension(&current),
        &formats,
        false,
    )
    .map_err(|e| Failure::Other(e.to_string()))?;
    let season = episodes
        .first()
        .map(|e| e.season_number)
        .unwrap_or(file.season_number);
    ensure_episode_folder(env, root_folders, series, season, &destination)?;
    tracing::debug!("Renaming episode file: {} to {}", current, destination);
    transfer_into_library(
        env,
        series,
        episodes,
        file,
        &current,
        &destination,
        TransferMode::Move,
    )
}

/// `MoveEpisodeFile` / `CopyEpisodeFile(episodeFile, localEpisode)`.
fn move_or_copy(
    env: &Environment,
    root_folders: &[RootFolder],
    local: &mut LocalEpisode,
    file: &mut EpisodeFile,
    copy_only: bool,
) -> Result<(), Failure> {
    let series = local.series.clone().expect("a local episode carries its series");
    let destination = build_file_path(
        env,
        &series,
        &local.episodes,
        file,
        &local.path,
        super::extension(&local.path),
        &local.custom_formats,
        true,
    )
    .map_err(|e| Failure::Other(e.to_string()))?;
    let season = local.season_number().map_err(Failure::Other)?;
    ensure_episode_folder(env, root_folders, &series, season, &destination)?;
    let mode = if !copy_only {
        tracing::debug!("Moving episode file: {} to {}", local.path, destination);
        TransferMode::Move
    } else if env.config.copy_using_hardlinks {
        tracing::debug!(
            "Attempting to hardlink episode file: {} to {}",
            local.path,
            destination
        );
        TransferMode::HardLinkOrCopy
    } else {
        tracing::debug!("Copying episode file: {} to {}", local.path, destination);
        TransferMode::Copy
    };
    let source = local.path.clone();
    let episodes = local.episodes.clone();
    transfer_into_library(env, &series, &episodes, file, &source, &destination, mode)?;
    local.file_name_before_rename = Some(file.relative_path.clone());
    Ok(())
}

/// `UpgradeMediaFileService.UpgradeEpisodeFile`: the files the episodes
/// have now go to the recycling bin and out of the database, then the new
/// one is moved in.
#[allow(clippy::too_many_arguments)]
fn upgrade(
    db: &Db,
    env: &Environment,
    root_folders: &[RootFolder],
    local: &mut LocalEpisode,
    file: &mut EpisodeFile,
    copy_only: bool,
    now: DateTime<Utc>,
    events: &mut Events,
) -> Result<Vec<DeletedEpisodeFile>, Failure> {
    let series = local.series.clone().expect("a local episode carries its series");
    let mut ids: Vec<i64> = local
        .episodes
        .iter()
        .map(|e| e.episode_file_id)
        .filter(|id| *id > 0)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let existing = db.call_blocking(move |conn| store::episodes::list_files_by_ids(conn, &ids))?;
    let root_folder = super::parent_path(&series.path).unwrap_or("").to_string();
    if !existing.is_empty() && !Path::new(&root_folder).is_dir() {
        return Err(Failure::RootFolderMissing(format!(
            "Root folder '{root_folder}' was not found."
        )));
    }
    let mut old_files = Vec::new();
    for old in existing {
        let path = super::join(&series.path, &old.relative_path);
        let subfolder =
            super::relative_path(&root_folder, super::parent_path(&path).unwrap_or("")).to_string();
        let mut recycle_bin_path = None;
        if Path::new(&path).is_file() {
            tracing::debug!("Removing existing episode file: {}", path);
            recycle_bin_path = transfer::recycle_file(&env.config, Path::new(&path), &subfolder)?
                .map(|p| p.to_string_lossy().into_owned());
        }
        old_files.push(DeletedEpisodeFile {
            file: old.clone(),
            recycle_bin_path,
        });
        let (config, series2) = (env.config.clone(), series.clone());
        let deleted = db.call_blocking(move |conn| {
            deletion::delete_file(conn, &config, &series2, &old, DeleteReason::Upgrade, now)
        })?;
        deletion::clean_folders(&env.config, &deleted);
        events.push(ImportEvent::EpisodeFileDeleted(Box::new(deleted)));
    }
    local.old_files = old_files.clone();
    move_or_copy(env, root_folders, local, file, copy_only)?;
    Ok(old_files)
}

/// `GetOriginalFilePath`.
fn original_file_path(item: Option<&DownloadClientItem>, local: &LocalEpisode) -> String {
    let path = local.path.as_str();
    if let Some(output) = item
        .and_then(|i| i.output_path.as_deref())
        .filter(|o| !o.is_empty())
    {
        // `OutputPath.Directory`: the folder itself when the output is a
        // folder, else its parent.
        let output_dir = if Path::new(output).is_dir() {
            output.to_string()
        } else {
            super::parent_path(output).unwrap_or("").to_string()
        };
        if super::is_parent_path(&output_dir, path) {
            return super::relative_path(&output_dir, path).to_string();
        }
    }
    if let Some(folder_info) = &local.folder_episode_info {
        if let Some(folder) = ancestor_path(path, &folder_info.release_title) {
            if let Some(parent) = super::parent_path(folder) {
                return super::relative_path(parent, path).to_string();
            }
        }
    }
    if let Some(grandparent) = super::parent_path(path).and_then(super::parent_path) {
        return super::relative_path(grandparent, path).to_string();
    }
    super::file_name(path).to_string()
}

/// `GetAncestorPath(path, folderName)`: the ancestor folder with that name.
fn ancestor_path<'a>(path: &'a str, folder_name: &str) -> Option<&'a str> {
    let mut current = super::parent_path(path);
    while let Some(folder) = current {
        if super::file_name(folder).eq_ignore_ascii_case(folder_name) {
            return Some(folder);
        }
        current = super::parent_path(folder);
    }
    None
}

/// The grab history's `indexerFlags` and `releaseType` for a download.
fn grab_flags_and_type(
    conn: &Connection,
    download_id: &str,
) -> anyhow::Result<(Option<i64>, Option<ReleaseType>)> {
    let grabbed = store::history::find_by_download_id(conn, download_id)?
        .into_iter()
        .filter(|h| h.event_type == HistoryEvent::Grabbed)
        .max_by_key(|h| h.date);
    let Some(h) = grabbed else {
        return Ok((None, None));
    };
    let flags = h.data["indexerFlags"].as_str().and_then(grab::parse_flag_names);
    let release_type = h.data["releaseType"]
        .as_str()
        .and_then(super::aggregate::release_type_from_data);
    Ok((flags, release_type))
}

/// `MediaFileService.Add` and `EpisodeService.Handle(EpisodeFileAddedEvent)`.
fn add_file(conn: &Connection, file: &mut EpisodeFile, episodes: &mut [Episode]) -> anyhow::Result<()> {
    file.id = store::sequence::next_id(conn, "episode_files")?;
    store::episodes::insert_file(conn, file)?;
    for episode in episodes.iter_mut() {
        store::episodes::set_episode_file_id(conn, episode.id, file.id)?;
        episode.episode_file_id = file.id;
        tracing::debug!("Linking [{}] > [{}]", file.relative_path, episode.id);
    }
    Ok(())
}

fn parsed_release_type(local: &LocalEpisode) -> ReleaseType {
    local
        .download_client_episode_info
        .as_ref()
        .map(|i| i.release_type())
        .or_else(|| local.folder_episode_info.as_ref().map(|i| i.release_type()))
        .or_else(|| local.file_episode_info.as_ref().map(|i| i.release_type()))
        .unwrap_or(ReleaseType::Unknown)
}

/// One approved decision imported; `Err` is the exception `Import` logs.
#[allow(clippy::too_many_arguments)]
fn import_one(
    db: &Db,
    env: &Environment,
    root_folders: &[RootFolder],
    local: &mut LocalEpisode,
    new_download: bool,
    item: Option<&DownloadClientItem>,
    mode: ImportMode,
    now: DateTime<Utc>,
    events: &mut Events,
) -> Result<(EpisodeFile, Vec<DeletedEpisodeFile>), Failure> {
    let series = local.series.clone().expect("a local episode carries its series");
    let mut file = EpisodeFile {
        id: 0,
        series_id: series.id,
        season_number: local.season_number().map_err(Failure::Other)?,
        relative_path: String::new(),
        original_file_path: None,
        size: std::fs::metadata(&local.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0),
        date_added: now,
        scene_name: None,
        release_group: local.release_group.clone(),
        release_hash: local.release_hash.clone(),
        quality: local.quality,
        languages: local.languages.clone(),
        media_info: local.media_info.as_ref().map(|m| m.model.clone()),
        indexer_flags: 0,
        release_type: parsed_release_type(local),
    };
    match item.map(|i| i.download_id.trim()).filter(|d| !d.is_empty()) {
        Some(download_id) => {
            let id = download_id.to_string();
            let (flags, release_type) = db.call_blocking(move |conn| grab_flags_and_type(conn, &id))?;
            if let Some(flags) = flags {
                file.indexer_flags = flags;
            }
            if let Some(rt) = release_type {
                file.release_type = rt;
            }
        }
        None => {
            file.indexer_flags = local.indexer_flags;
            file.release_type = local.release_type;
        }
    }
    if file.release_type == ReleaseType::Unknown {
        file.release_type = parsed_release_type(local);
    }
    let copy_only = match mode {
        ImportMode::Auto => item.is_some_and(|i| !i.can_move_files),
        ImportMode::Move => false,
        ImportMode::Copy => true,
    };
    let mut old_files = Vec::new();
    if new_download {
        file.scene_name = local.scene_name.clone();
        file.original_file_path = Some(original_file_path(item, local));
        old_files = upgrade(db, env, root_folders, local, &mut file, copy_only, now, events)?;
    } else {
        file.relative_path = super::relative_path(&series.path, &local.path).to_string();
        // Rows already at this path are replaced.
        let (series_id, relative, config, series2) = (
            series.id,
            file.relative_path.clone(),
            env.config.clone(),
            series.clone(),
        );
        let deleted = db.call_blocking(move |conn| {
            let previous = store::episodes::files_with_relative_path(conn, series_id, &relative)?;
            let mut out = Vec::new();
            for p in previous {
                out.push(deletion::delete_file(
                    conn,
                    &config,
                    &series2,
                    &p,
                    DeleteReason::ManualOverride,
                    now,
                )?);
            }
            Ok(out)
        })?;
        for d in deleted {
            events.push(ImportEvent::EpisodeFileDeleted(Box::new(d)));
        }
    }
    let mut episodes = local.episodes.clone();
    let (mut file2, episodes2) = (file.clone(), episodes.clone());
    let (file2, episodes2) = db.call_blocking(move |conn| {
        let mut eps = episodes2;
        add_file(conn, &mut file2, &mut eps)?;
        Ok((file2, eps))
    })?;
    file = file2;
    episodes = episodes2;
    local.episodes = episodes;
    if new_download {
        // `ExtraService.ImportEpisode`: what came with the video.
        if let Err(e) = super::extras::import_episode(db, &env.config, local, &file, copy_only, now) {
            tracing::warn!(error = %e, "Failed to import the extra files of {}", local.path);
        }
    }
    Ok((file, old_files))
}

/// `Import(decisions, newDownload, downloadClientItem, importMode)`.
pub fn import(
    db: &Db,
    env: &Environment,
    decisions: Vec<ImportDecision>,
    new_download: bool,
    item: Option<&DownloadClientItem>,
    mode: ImportMode,
    now: DateTime<Utc>,
) -> Imported {
    let mut imported = Imported::default();
    let root_folders = db
        .call_blocking(|conn| store::catalog::list_root_folders(conn))
        .unwrap_or_default();
    // Approved decisions, grouped by series, best quality then largest
    // first within a group, then by first episode number then size.
    let mut approved: Vec<ImportDecision> = Vec::new();
    let mut rejected: Vec<ImportDecision> = Vec::new();
    for d in decisions {
        if d.approved() && d.local_episode.is_some() {
            approved.push(d);
        } else {
            rejected.push(d);
        }
    }
    let mut groups: Vec<(i64, Vec<ImportDecision>)> = Vec::new();
    for d in approved {
        let series_id = d
            .local_episode
            .as_ref()
            .and_then(|l| l.series.as_ref())
            .map(|s| s.id)
            .unwrap_or(0);
        match groups.iter_mut().find(|(id, _)| *id == series_id) {
            Some((_, list)) => list.push(d),
            None => groups.push((series_id, vec![d])),
        }
    }
    let mut qualified: Vec<ImportDecision> = Vec::new();
    for (_, mut list) in groups {
        let profile = list
            .first()
            .and_then(|d| d.local_episode.as_ref())
            .and_then(|l| l.series.as_ref())
            .and_then(|s| env.catalog.profile(s.quality_profile_id))
            .cloned();
        list.sort_by(|a, b| {
            let (la, lb) = (
                a.local_episode.as_ref().unwrap(),
                b.local_episode.as_ref().unwrap(),
            );
            let by_quality = match &profile {
                Some(p) => upgradable::compare_model(p, &lb.quality, &la.quality),
                None => std::cmp::Ordering::Equal,
            };
            by_quality.then(lb.size.cmp(&la.size))
        });
        qualified.extend(list);
    }
    qualified.sort_by(|a, b| {
        let (la, lb) = (
            a.local_episode.as_ref().unwrap(),
            b.local_episode.as_ref().unwrap(),
        );
        let min_a = la.episodes.iter().map(|e| e.episode_number).min().unwrap_or(0);
        let min_b = lb.episodes.iter().map(|e| e.episode_number).min().unwrap_or(0);
        min_a.cmp(&min_b).then(lb.size.cmp(&la.size))
    });

    let mut imported_episode_ids: Vec<i64> = Vec::new();
    for decision in qualified {
        let mut local = decision
            .local_episode
            .clone()
            .expect("approved decisions carry a file");
        if local
            .episodes
            .iter()
            .any(|e| imported_episode_ids.contains(&e.id))
        {
            imported.results.push(ImportResult::failed(
                decision,
                vec!["Episode has already been imported".to_string()],
            ));
            continue;
        }
        match import_one(
            db,
            env,
            &root_folders,
            &mut local,
            new_download,
            item,
            mode,
            now,
            &mut imported.events,
        ) {
            Ok((file, old_files)) => {
                imported_episode_ids.extend(local.episodes.iter().map(|e| e.id));
                let ev = EpisodeImported {
                    local: local.clone(),
                    file: file.clone(),
                    old_files,
                    new_download,
                    client: item.map(|i| i.client.clone()),
                    download_id: item.map(|i| i.download_id.clone()),
                };
                let ev2 = ev.clone();
                if let Err(e) = db.call_blocking(move |conn| events::record_imported(conn, &ev2, now)) {
                    tracing::error!(error = %e, "failed to record the import in the history");
                }
                let mut done = decision;
                done.local_episode = Some(local);
                imported.results.push(ImportResult::imported(done, file));
                imported.events.push(ImportEvent::EpisodeImported(Box::new(ev)));
            }
            Err(failure) => {
                tracing::warn!("Couldn't import episode {}: {}", local.path, failure.detail());
                match &failure {
                    Failure::RootFolderMissing(_) | Failure::RecycleBin => {
                        imported.events.push(ImportEvent::EpisodeImportFailed(Box::new(
                            EpisodeImportFailed {
                                local: local.clone(),
                                message: match &failure {
                                    Failure::RootFolderMissing(m) => m.clone(),
                                    other => other.message().to_string(),
                                },
                                new_download,
                                client: item.map(|i| i.client.clone()),
                                download_id: item.map(|i| i.download_id.clone()),
                            },
                        )));
                    }
                    Failure::DestinationExists(_) => {
                        if let Some(s) = &local.series {
                            imported.rescan_series.push(s.id);
                        }
                    }
                    Failure::Other(_) | Failure::SameFile(_) => {}
                }
                imported.results.push(ImportResult::failed(
                    decision,
                    vec![failure.message().to_string()],
                ));
            }
        }
    }
    for d in rejected {
        let errors: Vec<String> = d.rejections.iter().map(|r| r.message.clone()).collect();
        imported.results.push(ImportResult::failed(d, errors));
    }
    imported
}
