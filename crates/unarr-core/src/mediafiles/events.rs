//! What the import tells whoever listens, and what the history keeps of it.
//! The originals publish `EpisodeImportedEvent`, `EpisodeFileDeletedEvent`,
//! `DownloadCompletedEvent`, `ManualInteractionRequiredEvent`,
//! `SeriesScannedEvent` and `EpisodeFileRenamedEvent` on their bus; the
//! history service, the download history and the SignalR relays handle
//! them. Here the history rows are written by the pipeline itself (the
//! writers live in this module) and the events are returned to the runtime,
//! which relays them to the API and, later, to the notifications.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::local::{DeleteReason, DeletedEpisodeFile, GrabbedRelease, LocalEpisode};
use crate::downloadclients::ItemClientInfo;
use crate::downloads::TrackedDownload;
use crate::downloads::{failed, grab};
use crate::model::{Episode, EpisodeFile, HistoryEntry, HistoryEvent, Series};
use crate::store;
use crate::store::download_history::{DownloadHistoryEntry, DownloadHistoryEvent};

/// `EpisodeImportedEvent`.
#[derive(Debug, Clone)]
pub struct EpisodeImported {
    pub local: LocalEpisode,
    pub file: EpisodeFile,
    pub old_files: Vec<DeletedEpisodeFile>,
    pub new_download: bool,
    pub client: Option<ItemClientInfo>,
    pub download_id: Option<String>,
}

/// `EpisodeFileDeletedEvent`, with the series and episodes the originals
/// lazy-load on the file.
#[derive(Debug, Clone)]
pub struct EpisodeFileDeleted {
    pub file: EpisodeFile,
    pub series: Series,
    pub episodes: Vec<Episode>,
    pub reason: DeleteReason,
}

impl EpisodeFileDeleted {
    /// `EpisodeFile.Path`.
    pub fn path(&self) -> String {
        super::join(&self.series.path, &self.file.relative_path)
    }
}

/// `DownloadCompletedEvent`.
#[derive(Debug, Clone)]
pub struct DownloadCompleted {
    pub tracked: TrackedDownload,
    pub series_id: i64,
    pub files: Vec<EpisodeFile>,
    pub release: Option<GrabbedRelease>,
}

/// `ManualInteractionRequiredEvent`.
#[derive(Debug, Clone)]
pub struct ManualInteractionRequired {
    pub tracked: TrackedDownload,
    pub release: Option<GrabbedRelease>,
}

/// `EpisodeFileRenamedEvent`.
#[derive(Debug, Clone)]
pub struct EpisodeFileRenamed {
    pub series: Series,
    pub file: EpisodeFile,
    pub episodes: Vec<Episode>,
    pub original_path: String,
}

/// `EpisodeImportFailedEvent`: an import that raised, with the message the
/// originals log.
#[derive(Debug, Clone)]
pub struct EpisodeImportFailed {
    pub local: LocalEpisode,
    pub message: String,
    pub new_download: bool,
    pub client: Option<ItemClientInfo>,
    pub download_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ImportEvent {
    EpisodeImported(Box<EpisodeImported>),
    EpisodeImportFailed(Box<EpisodeImportFailed>),
    EpisodeFileDeleted(Box<EpisodeFileDeleted>),
    DownloadCompleted(Box<DownloadCompleted>),
    ManualInteractionRequired(Box<ManualInteractionRequired>),
    /// `SeriesScannedEvent`.
    SeriesScanned {
        series_id: i64,
    },
    EpisodeFileRenamed(Box<EpisodeFileRenamed>),
    /// `SeriesRenamedEvent`: the files of a series renamed by a command.
    SeriesRenamed {
        series_id: i64,
        renamed: Vec<EpisodeFileRenamed>,
    },
    /// `DeleteCompletedEvent`.
    DeleteCompleted,
    /// An import found its destination taken: the originals push a
    /// `RescanSeriesCommand`.
    RescanRequested {
        series_id: i64,
    },
}

/// What a pipeline step hands back besides its results.
#[derive(Debug, Default)]
pub struct Events(pub Vec<ImportEvent>);

impl Events {
    pub fn push(&mut self, e: ImportEvent) {
        self.0.push(e);
    }

    pub fn extend(&mut self, other: Events) {
        self.0.extend(other.0);
    }

    pub fn into_vec(self) -> Vec<ImportEvent> {
        self.0
    }
}

fn text<T: ToString>(v: T) -> Value {
    Value::String(v.to_string())
}

fn opt(v: Option<&str>) -> Value {
    match v {
        Some(s) => Value::String(s.to_string()),
        None => Value::Null,
    }
}

/// `HistoryService.Handle(EpisodeImportedEvent)` and
/// `DownloadHistoryService.Handle(EpisodeImportedEvent)`: one history row
/// per episode and one download history row, for a new download.
pub fn record_imported(conn: &Connection, ev: &EpisodeImported, now: DateTime<Utc>) -> anyhow::Result<()> {
    if !ev.new_download {
        return Ok(());
    }
    let series = ev.local.series_ref();
    let download_id = match ev.download_id.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(d) => Some(d.to_string()),
        None => find_download_id(conn, ev)?,
    };
    let source_title = ev
        .file
        .scene_name
        .clone()
        .unwrap_or_else(|| super::file_stem(&ev.local.path).to_string());
    let imported_path = super::join(&series.path, &ev.file.relative_path);
    let first = store::history::next_ids(conn, ev.local.episodes.len())?;
    for (i, episode) in ev.local.episodes.iter().enumerate() {
        let entry = HistoryEntry {
            id: first + i as i64,
            series_id: ev.file.series_id,
            episode_id: episode.id,
            source_title: Some(source_title.clone()),
            date: now,
            event_type: HistoryEvent::DownloadFolderImported,
            quality: ev.local.quality,
            languages: ev.local.languages.clone(),
            data: json!({
                "fileId": text(ev.file.id),
                "droppedPath": ev.local.path,
                "importedPath": imported_path,
                "downloadClient": opt(ev.client.as_ref().map(|c| c.kind.display_name())),
                "downloadClientName": opt(ev.client.as_ref().map(|c| c.name.as_str())),
                "releaseGroup": ev.local.release_group,
                "customFormatScore": text(ev.local.custom_format_score),
                "size": text(ev.local.size),
                "indexerFlags": grab::flag_names(ev.file.indexer_flags),
                "releaseType": failed::pascal(ev.file.release_type.as_str()),
            }),
            download_id: download_id.clone(),
        };
        store::history::insert(conn, &entry)?;
    }
    let (Some(download_id), Some(client)) = (download_id, ev.client.as_ref()) else {
        return Ok(());
    };
    let entry = DownloadHistoryEntry {
        id: 0,
        event_type: DownloadHistoryEvent::FileImported,
        series_id: ev.file.series_id,
        download_id,
        source_title: ev.local.path.clone(),
        date: now,
        protocol: client.protocol,
        indexer_id: 0,
        download_client_id: client.id,
        release: None,
        data: json!({
            "DownloadClient": client.kind.display_name(),
            "DownloadClientName": client.name,
            "SourcePath": ev.local.path,
            "DestinationPath": imported_path,
        }),
    };
    store::download_history::insert(conn, &entry)?;
    Ok(())
}

/// `HistoryService.FindDownloadId`: the download an import without a
/// download id belongs to, when the grab history of its episodes names
/// exactly one still-downloading release at that quality.
pub fn find_download_id(conn: &Connection, ev: &EpisodeImported) -> anyhow::Result<Option<String>> {
    tracing::debug!("Trying to find downloadId for {} from history", ev.local.path);
    let episode_ids: Vec<i64> = ev.local.episodes.iter().map(|e| e.id).collect();
    let all = store::history::download_history_for(conn, ev.file.series_id, &ev.file.quality)?;
    let episodes_history: Vec<&HistoryEntry> = all
        .iter()
        .filter(|h| episode_ids.contains(&h.episode_id))
        .collect();
    let processed: Vec<&str> = episodes_history
        .iter()
        .filter(|h| h.event_type != HistoryEvent::Grabbed)
        .filter_map(|h| h.download_id.as_deref())
        .collect();
    let still_downloading: Vec<&HistoryEntry> = episodes_history
        .iter()
        .copied()
        .filter(|h| {
            h.event_type == HistoryEvent::Grabbed
                && !h.download_id.as_deref().is_some_and(|d| processed.contains(&d))
        })
        .collect();
    if still_downloading.is_empty() {
        return Ok(None);
    }
    let mut download_id: Option<String> = None;
    for episode in &ev.local.episodes {
        let matching: Vec<&&HistoryEntry> = still_downloading
            .iter()
            .filter(|h| h.episode_id == episode.id)
            .collect();
        if matching.len() != 1 {
            return Ok(None);
        }
        let new_id = matching[0].download_id.clone();
        match (&download_id, new_id) {
            (None, id) => download_id = id,
            (Some(current), Some(id)) if *current == id => {}
            _ => return Ok(None),
        }
    }
    Ok(download_id)
}

/// `HistoryService.Handle(EpisodeFileDeletedEvent)`: a row per episode,
/// except for the cleanup and manual-override deletions.
pub fn record_deleted(conn: &Connection, ev: &EpisodeFileDeleted, now: DateTime<Utc>) -> anyhow::Result<()> {
    match ev.reason {
        DeleteReason::NoLinkedEpisodes => {
            tracing::debug!(
                "Removing episode file from DB as part of cleanup routine, not creating history event."
            );
            return Ok(());
        }
        DeleteReason::ManualOverride => {
            tracing::debug!(
                "Removing episode file from DB as part of manual override of existing file, not creating history event."
            );
            return Ok(());
        }
        _ => {}
    }
    let path = ev.path();
    let first = store::history::next_ids(conn, ev.episodes.len())?;
    for (i, episode) in ev.episodes.iter().enumerate() {
        let entry = HistoryEntry {
            id: first + i as i64,
            series_id: ev.file.series_id,
            episode_id: episode.id,
            source_title: Some(path.clone()),
            date: now,
            event_type: HistoryEvent::EpisodeFileDeleted,
            quality: ev.file.quality,
            languages: ev.file.languages.clone(),
            data: json!({
                "reason": ev.reason.as_str(),
                "releaseGroup": ev.file.release_group,
                "size": text(ev.file.size),
                "indexerFlags": grab::flag_names(ev.file.indexer_flags),
                "releaseType": failed::pascal(ev.file.release_type.as_str()),
            }),
            download_id: None,
        };
        store::history::insert(conn, &entry)?;
    }
    Ok(())
}

/// `HistoryService.Handle(EpisodeFileRenamedEvent)`.
pub fn record_renamed(conn: &Connection, ev: &EpisodeFileRenamed, now: DateTime<Utc>) -> anyhow::Result<()> {
    let source_relative = super::relative_path(&ev.series.path, &ev.original_path).to_string();
    let path = super::join(&ev.series.path, &ev.file.relative_path);
    let first = store::history::next_ids(conn, ev.episodes.len())?;
    for (i, episode) in ev.episodes.iter().enumerate() {
        let entry = HistoryEntry {
            id: first + i as i64,
            series_id: ev.file.series_id,
            episode_id: episode.id,
            source_title: Some(ev.original_path.clone()),
            date: now,
            event_type: HistoryEvent::EpisodeFileRenamed,
            quality: ev.file.quality,
            languages: ev.file.languages.clone(),
            data: json!({
                "sourcePath": ev.original_path,
                "sourceRelativePath": source_relative,
                "path": path,
                "relativePath": ev.file.relative_path,
                "releaseGroup": ev.file.release_group,
                "size": text(ev.file.size),
                "indexerFlags": grab::flag_names(ev.file.indexer_flags),
                "releaseType": failed::pascal(ev.file.release_type.as_str()),
            }),
            download_id: None,
        };
        store::history::insert(conn, &entry)?;
    }
    Ok(())
}

/// `DownloadHistoryService.Handle(DownloadCompletedEvent)`.
pub fn record_download_completed(
    conn: &Connection,
    ev: &DownloadCompleted,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let item = &ev.tracked.item;
    let entry = DownloadHistoryEntry {
        id: 0,
        event_type: DownloadHistoryEvent::DownloadImported,
        series_id: ev.series_id,
        download_id: item.download_id.clone(),
        source_title: item.title.clone(),
        date: now,
        protocol: ev.tracked.protocol,
        indexer_id: 0,
        download_client_id: ev.tracked.download_client,
        release: None,
        data: json!({
            "DownloadClient": item.client.kind.display_name(),
            "DownloadClientName": item.client.name,
        }),
    };
    store::download_history::insert(conn, &entry)?;
    Ok(())
}
