//! Removing an episode file: `MediaFileService.Delete` with the handlers
//! the originals run on `EpisodeFileDeletedEvent` (the episode service
//! detaching episodes, the history, the folder cleanup), and
//! `MediaFileDeletionService.DeleteEpisodeFile` for the API's deletes.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::events::{self, EpisodeFileDeleted, Events, ImportEvent};
use super::local::DeleteReason;
use super::transfer;
use crate::db::Db;
use crate::model::{EpisodeFile, Series};
use crate::store;
use crate::store::config::MediaManagementConfig;

/// `MediaFileService.Delete(episodeFile, reason)` and the database side of
/// its handlers: the row goes, the episodes are detached (and unmonitored
/// when the settings say so), the history is written. The event is
/// returned for the folder cleanup and the relays.
pub fn delete_file(
    conn: &Connection,
    config: &MediaManagementConfig,
    series: &Series,
    file: &EpisodeFile,
    reason: DeleteReason,
    now: DateTime<Utc>,
) -> anyhow::Result<EpisodeFileDeleted> {
    let episodes = store::episodes::list_episodes_by_file(conn, file.id)?;
    store::episodes::delete_file(conn, file.id)?;
    // `EpisodeService.Handle(EpisodeFileDeletedEvent)`.
    let unmonitor_for_reason = !matches!(
        reason,
        DeleteReason::Upgrade | DeleteReason::ManualOverride | DeleteReason::MissingFromDisk
    );
    let unmonitor = unmonitor_for_reason && config.auto_unmonitor_previously_downloaded_episodes;
    for episode in &episodes {
        tracing::debug!("Detaching episode {} from file.", episode.id);
    }
    store::episodes::clear_episode_file_id(conn, file.id)?;
    if unmonitor {
        let ids: Vec<i64> = episodes.iter().map(|e| e.id).collect();
        store::episodes::set_monitored(conn, &ids, false)?;
    }
    let ev = EpisodeFileDeleted {
        file: file.clone(),
        series: series.clone(),
        episodes,
        reason,
    };
    events::record_deleted(conn, &ev, now)?;
    Ok(ev)
}

/// `MediaFileDeletionService.Handle(EpisodeFileDeletedEvent)`: with
/// "delete empty folders" on, the folders the file left are pruned up to
/// and including the series folder.
pub fn clean_folders(config: &MediaManagementConfig, ev: &EpisodeFileDeleted) {
    if !config.delete_empty_folders || ev.reason == DeleteReason::MissingFromDisk {
        return;
    }
    let series_path = ev.series.path.as_str();
    let path = ev.path();
    let mut folder = super::parent_path(&path).map(str::to_string);
    while let Some(f) = folder.clone() {
        if !super::is_parent_path(series_path, &f) {
            break;
        }
        if Path::new(&f).is_dir() {
            transfer::remove_empty_subfolders(Path::new(&f));
        }
        folder = super::parent_path(&f).map(str::to_string);
    }
    transfer::remove_empty_subfolders(Path::new(series_path));
    if transfer::folder_empty(Path::new(series_path)) {
        if let Err(e) = std::fs::remove_dir_all(series_path) {
            tracing::debug!(error = %e, "unable to delete the empty series folder {series_path}");
        }
    }
}

/// Why `DeleteEpisodeFile` refused, with the HTTP status the originals
/// answer.
#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    #[error("Series' root folder ({0}) doesn't exist.")]
    RootFolderMissing(String),
    #[error("Series' root folder ({0}) is empty.")]
    RootFolderEmpty(String),
    #[error("Unable to delete episode file")]
    Unable,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl DeleteError {
    pub fn status(&self) -> u16 {
        match self {
            DeleteError::RootFolderMissing(_) | DeleteError::RootFolderEmpty(_) => 409,
            DeleteError::Unable | DeleteError::Other(_) => 500,
        }
    }
}

/// `MediaFileDeletionService.DeleteEpisodeFile(series, episodeFile)`: the
/// file to the recycling bin, the row removed, `DeleteCompletedEvent`.
pub fn delete_episode_file(
    db: &Db,
    config: &MediaManagementConfig,
    series: &Series,
    file: &EpisodeFile,
    now: DateTime<Utc>,
) -> Result<Events, DeleteError> {
    let full_path = super::join(&series.path, &file.relative_path);
    let root_folder = super::parent_path(&series.path).unwrap_or("").to_string();
    if !Path::new(&root_folder).is_dir() {
        tracing::warn!("Series' root folder ({root_folder}) doesn't exist.");
        return Err(DeleteError::RootFolderMissing(root_folder));
    }
    if transfer::directories_under(Path::new(&root_folder)).is_empty() {
        tracing::warn!("Series' root folder ({root_folder}) is empty.");
        return Err(DeleteError::RootFolderEmpty(root_folder));
    }
    if Path::new(&series.path).is_dir() && Path::new(&full_path).is_file() {
        tracing::info!("Deleting episode file: {full_path}");
        let subfolder =
            super::relative_path(&root_folder, super::parent_path(&full_path).unwrap_or("")).to_string();
        if let Err(e) = transfer::recycle_file(config, Path::new(&full_path), &subfolder) {
            tracing::error!(error = %e, "Unable to delete episode file");
            return Err(DeleteError::Unable);
        }
    }
    // The row goes even when the file was already gone.
    let (config2, series2, file2) = (config.clone(), series.clone(), file.clone());
    let ev = db.call_blocking(move |conn| {
        delete_file(conn, &config2, &series2, &file2, DeleteReason::Manual, now)
    })?;
    clean_folders(config, &ev);
    let mut events = Events::default();
    events.push(ImportEvent::EpisodeFileDeleted(Box::new(ev)));
    events.push(ImportEvent::DeleteCompleted);
    Ok(events)
}
