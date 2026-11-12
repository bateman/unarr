//! The events an import publishes, as values: `MovieFileImportedEvent`,
//! `MovieFileDeletedEvent`, `DownloadCompletedEvent`,
//! `ManualInteractionRequiredEvent`, `MovieImportFailedEvent`. The history
//! rows `HistoryService` writes for them are written here; the SignalR
//! relay and the notifications read the values.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::local::{DeletedMovieFile, GrabbedMovieRelease, LocalMovie};
use crate::downloadclients::ItemClientInfo;
use crate::mediafiles::local::DeleteReason;
use crate::mediafiles::{file_stem, join};
use crate::movies::downloads::tracked::download_event;
use crate::movies::downloads::TrackedMovieDownload;
use crate::movies::model::{
    Movie, MovieDownloadHistoryEntry, MovieFile, MovieHistoryEntry, MovieHistoryEvent,
};
use crate::movies::quality::indexer_flags_text;
use crate::movies::store;

/// `MovieFileImportedEvent`.
#[derive(Debug, Clone)]
pub struct MovieImported {
    pub local: LocalMovie,
    pub file: MovieFile,
    pub old_files: Vec<DeletedMovieFile>,
    pub new_download: bool,
    pub client: Option<ItemClientInfo>,
    pub download_id: Option<String>,
}

/// `MovieFileDeletedEvent`.
#[derive(Debug, Clone)]
pub struct MovieFileDeleted {
    pub file: MovieFile,
    pub movie: Movie,
    pub reason: DeleteReason,
}

impl MovieFileDeleted {
    pub fn path(&self) -> String {
        join(&self.movie.path, &self.file.relative_path)
    }
}

/// `DownloadCompletedEvent`.
#[derive(Debug, Clone)]
pub struct MovieDownloadCompleted {
    pub tracked: TrackedMovieDownload,
    pub movie_id: i64,
    pub release: Option<GrabbedMovieRelease>,
}

/// `ManualInteractionRequiredEvent`.
#[derive(Debug, Clone)]
pub struct MovieManualInteractionRequired {
    pub tracked: TrackedMovieDownload,
    pub release: Option<GrabbedMovieRelease>,
}

/// `MovieImportFailedEvent`.
#[derive(Debug, Clone)]
pub struct MovieImportFailed {
    pub local: LocalMovie,
    pub message: String,
    pub new_download: bool,
    pub client: Option<ItemClientInfo>,
    pub download_id: Option<String>,
}

/// `MovieFileRenamedEvent`.
#[derive(Debug, Clone)]
pub struct MovieFileRenamed {
    pub movie: Movie,
    pub file: MovieFile,
    pub original_path: String,
}

/// What an import, a deletion or a scan tells the rest of the application.
#[derive(Debug, Clone)]
pub enum MovieImportEvent {
    Imported(Box<MovieImported>),
    ImportFailed(Box<MovieImportFailed>),
    FileDeleted(Box<MovieFileDeleted>),
    DownloadCompleted(Box<MovieDownloadCompleted>),
    ManualInteractionRequired(Box<MovieManualInteractionRequired>),
    /// A destination that already existed: `RescanMovieCommand` is queued.
    RescanRequested {
        movie_id: i64,
    },
    /// `MovieScannedEvent`, with the files of the movie's folder that are
    /// not videos (`PossibleExtraFiles`).
    Scanned {
        movie_id: i64,
        possible_extra_files: Vec<String>,
    },
    FileRenamed(Box<MovieFileRenamed>),
    /// `MovieRenamedEvent`: the files of one movie that were renamed.
    MovieRenamed {
        movie_id: i64,
        renamed: Vec<MovieFileRenamed>,
    },
    /// `MovieScanSkippedEvent`: the root folder is missing or empty.
    ScanSkipped {
        movie_id: i64,
    },
}

/// The events of one run, in the order they happened.
#[derive(Debug, Default)]
pub struct MovieEvents(pub Vec<MovieImportEvent>);

impl MovieEvents {
    pub fn push(&mut self, e: MovieImportEvent) {
        self.0.push(e);
    }

    pub fn extend(&mut self, other: MovieEvents) {
        self.0.extend(other.0);
    }

    pub fn into_vec(self) -> Vec<MovieImportEvent> {
        self.0
    }
}

fn text<T: ToString>(v: T) -> Value {
    Value::String(v.to_string())
}

/// `HistoryService.Handle(MovieFileImportedEvent)`: a new download leaves
/// a `DownloadFolderImported` row; a file found by a scan leaves none.
pub fn record_imported(conn: &Connection, ev: &MovieImported, now: DateTime<Utc>) -> anyhow::Result<()> {
    if !ev.new_download {
        return Ok(());
    }
    let movie = ev.local.movie_ref();
    let download_id = match ev.download_id.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(d) => Some(d.to_string()),
        None => find_download_id(conn, ev)?,
    };
    let source_title = ev
        .file
        .scene_name
        .clone()
        .unwrap_or_else(|| file_stem(&ev.local.path).to_string());
    let entry = MovieHistoryEntry {
        id: store::next_history_id(conn)?,
        movie_id: movie.id,
        source_title: Some(source_title),
        date: now,
        event_type: MovieHistoryEvent::DownloadFolderImported,
        quality: ev.local.quality,
        languages: ev.local.languages.clone(),
        data: json!({
            "fileId": text(ev.file.id),
            "droppedPath": ev.local.path,
            "importedPath": join(&movie.path, &ev.file.relative_path),
            "downloadClient": ev.client.as_ref().map(|c| c.kind.display_name()),
            "downloadClientName": ev.client.as_ref().map(|c| c.name.clone()),
            "releaseGroup": ev.local.release_group,
            "customFormatScore": text(ev.local.custom_format_score),
            "size": text(ev.local.size),
            "indexerFlags": indexer_flags_text(ev.file.indexer_flags),
        }),
        download_id,
    };
    store::insert_history(conn, &entry)
}

/// `FindDownloadId`: the one download of this movie in this quality that
/// was grabbed and not dealt with since, when there is exactly one.
fn find_download_id(conn: &Connection, ev: &MovieImported) -> anyhow::Result<Option<String>> {
    let movie_id = ev.local.movie_ref().id;
    let history: Vec<MovieHistoryEntry> = store::history_of_movie(conn, movie_id, None)?
        .into_iter()
        .filter(|h| h.quality == ev.file.quality)
        .filter(|h| {
            matches!(
                h.event_type,
                MovieHistoryEvent::Grabbed
                    | MovieHistoryEvent::DownloadFailed
                    | MovieHistoryEvent::DownloadFolderImported
            )
        })
        .collect();
    let processed: Vec<&str> = history
        .iter()
        .filter(|h| h.event_type != MovieHistoryEvent::Grabbed)
        .filter_map(|h| h.download_id.as_deref())
        .collect();
    let still_downloading: Vec<&MovieHistoryEntry> = history
        .iter()
        .filter(|h| {
            h.event_type == MovieHistoryEvent::Grabbed
                && !h.download_id.as_deref().is_some_and(|d| processed.contains(&d))
        })
        .collect();
    match still_downloading.as_slice() {
        [one] => Ok(one.download_id.clone()),
        _ => Ok(None),
    }
}

/// `HistoryService.Handle(MovieFileDeletedEvent)`.
pub fn record_deleted(conn: &Connection, ev: &MovieFileDeleted, now: DateTime<Utc>) -> anyhow::Result<()> {
    if ev.reason == DeleteReason::NoLinkedEpisodes {
        tracing::debug!(
            "Removing movie file from DB as part of cleanup routine, not creating history event."
        );
        return Ok(());
    }
    let entry = MovieHistoryEntry {
        id: store::next_history_id(conn)?,
        movie_id: ev.file.movie_id,
        source_title: Some(ev.path()),
        date: now,
        event_type: MovieHistoryEvent::MovieFileDeleted,
        quality: ev.file.quality,
        languages: ev.file.languages.clone(),
        data: json!({
            "reason": ev.reason.as_str(),
            "releaseGroup": ev.file.release_group,
            "size": text(ev.file.size),
            "indexerFlags": indexer_flags_text(ev.file.indexer_flags),
        }),
        download_id: None,
    };
    store::insert_history(conn, &entry)
}

/// `DownloadHistoryService.Handle(DownloadCompletedEvent)`.
pub fn record_download_completed(
    conn: &Connection,
    ev: &MovieDownloadCompleted,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let item = &ev.tracked.item;
    store::add_download_history(
        conn,
        &MovieDownloadHistoryEntry {
            id: 0,
            event_type: download_event::IMPORTED,
            movie_id: ev.movie_id,
            download_id: item.download_id.clone(),
            source_title: item.title.clone(),
            date: now,
            protocol: ev.tracked.protocol,
            indexer_id: 0,
            download_client_id: ev.tracked.download_client,
            release: None,
            data: json!({
                "downloadClient": item.client.kind.display_name(),
                "downloadClientName": item.client.name,
            }),
        },
    )
}

/// `HistoryService.Handle(MovieFileRenamedEvent)`.
pub fn record_renamed(conn: &Connection, ev: &MovieFileRenamed, now: DateTime<Utc>) -> anyhow::Result<()> {
    let entry = MovieHistoryEntry {
        id: store::next_history_id(conn)?,
        movie_id: ev.file.movie_id,
        source_title: Some(ev.original_path.clone()),
        date: now,
        event_type: MovieHistoryEvent::MovieFileRenamed,
        quality: ev.file.quality,
        languages: ev.file.languages.clone(),
        data: json!({
            "sourcePath": ev.original_path,
            "sourceRelativePath": crate::mediafiles::relative_path(&ev.movie.path, &ev.original_path),
            "path": join(&ev.movie.path, &ev.file.relative_path),
            "relativePath": ev.file.relative_path,
            "releaseGroup": ev.file.release_group,
            "size": text(ev.file.size),
            "indexerFlags": indexer_flags_text(ev.file.indexer_flags),
        }),
        download_id: None,
    };
    store::insert_history(conn, &entry)
}
