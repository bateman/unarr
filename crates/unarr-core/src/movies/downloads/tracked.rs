//! `TrackedDownload` and `TrackedDownloadService.TrackDownload`, Radarr's:
//! an item a client reports, matched back to the movie it is for, through
//! its title or the history of its grab.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::downloadclients::{DownloadClientDefinition, DownloadClientItem};
use crate::downloads::{StatusMessage, TrackedDownloadState, TrackedDownloadStatus};
use crate::indexers::rss::ReleaseInfo;
use crate::model::Protocol;
use crate::movies::decision::engine::{aggregate_languages, remote_formats};
use crate::movies::decision::{mapping, MovieCatalog, MovieLookup, RemoteMovie};
use crate::movies::model::MovieHistoryEvent;
use crate::movies::parser::{parse_movie_title, ParsedMovieInfo};
use crate::movies::quality::parse_indexer_flags;
use crate::movies::store;

/// `DownloadHistoryEventType`, as Radarr's table numbers it.
pub mod download_event {
    pub const GRABBED: i64 = 1;
    pub const IMPORTED: i64 = 2;
    pub const FAILED: i64 = 3;
    pub const IGNORED: i64 = 4;
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackedMovieDownload {
    pub download_client: i64,
    pub item: DownloadClientItem,
    /// The item with its import path resolved, once completed.
    pub import_item: Option<DownloadClientItem>,
    pub state: TrackedDownloadState,
    pub status: TrackedDownloadStatus,
    pub remote: Option<RemoteMovie>,
    pub status_messages: Vec<StatusMessage>,
    pub protocol: Protocol,
    pub indexer: Option<String>,
    pub added: Option<DateTime<Utc>>,
    pub is_trackable: bool,
    pub has_notified_manual_interaction_required: bool,
    /// The movie was read by its id (from the history of the grab) rather
    /// than found by the item's title. The original's movie then carries
    /// its alternative titles, which the one a title finds does not; the
    /// queue shows the difference.
    pub movie_by_id: bool,
}

impl TrackedMovieDownload {
    pub fn new(definition: &DownloadClientDefinition, item: DownloadClientItem) -> TrackedMovieDownload {
        TrackedMovieDownload {
            download_client: definition.id,
            protocol: definition.protocol(),
            item,
            import_item: None,
            state: TrackedDownloadState::Downloading,
            status: TrackedDownloadStatus::Ok,
            remote: None,
            status_messages: Vec::new(),
            indexer: None,
            added: None,
            is_trackable: true,
            has_notified_manual_interaction_required: false,
            movie_by_id: false,
        }
    }

    /// `Warn(message)`: a warning status with one message under the
    /// item's title.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.status = TrackedDownloadStatus::Warning;
        self.status_messages = vec![StatusMessage {
            title: self.item.title.clone(),
            messages: vec![message.into()],
        }];
    }

    pub fn download_id(&self) -> &str {
        &self.item.download_id
    }
}

/// `GetStateFromHistory`.
fn state_from_history(event: i64) -> TrackedDownloadState {
    match event {
        download_event::IMPORTED => TrackedDownloadState::Imported,
        download_event::FAILED => TrackedDownloadState::Failed,
        download_event::IGNORED => TrackedDownloadState::Ignored,
        _ => TrackedDownloadState::Downloading,
    }
}

/// `ParsingService.Map(parsedMovieInfo, "", 0, null)`.
fn map_by_title(
    lookup: &dyn MovieLookup,
    parsed: ParsedMovieInfo,
) -> Result<RemoteMovie, mapping::MultipleMovies> {
    let mut remote = RemoteMovie::new(ReleaseInfo::default());
    if let Some((movie, match_type)) = mapping::find_movie(lookup, &parsed, "", 0, None)? {
        remote.movie = Some(movie);
        remote.match_type = match_type;
    }
    remote.languages = parsed.languages.clone();
    remote.parsed = Some(parsed);
    Ok(remote)
}

/// `ParsingService.Map(parsedMovieInfo, movieId)`.
fn map_by_id(conn: &Connection, parsed: ParsedMovieInfo, movie_id: i64) -> anyhow::Result<RemoteMovie> {
    let mut remote = RemoteMovie::new(ReleaseInfo::default());
    remote.movie = store::get_movie(conn, movie_id)?;
    remote.parsed = Some(parsed);
    Ok(remote)
}

/// `RemoteMovieAggregationService.Augment` and the custom formats of the
/// item's size.
fn augment(catalog: &MovieCatalog, remote: &mut RemoteMovie, size: i64) {
    if let Some(movie) = remote.movie.clone() {
        remote.languages = aggregate_languages(catalog, remote, &movie);
    }
    remote.custom_formats = remote_formats(catalog, remote, size);
}

/// `TrackDownload`: the tracked download for an item, reusing the existing
/// one when it is past downloading.
pub fn track(
    conn: &Connection,
    catalog: &MovieCatalog,
    lookup: &dyn MovieLookup,
    definition: &DownloadClientDefinition,
    item: DownloadClientItem,
    existing: Option<TrackedMovieDownload>,
) -> anyhow::Result<TrackedMovieDownload> {
    if let Some(mut existing) = existing.clone() {
        if existing.state != TrackedDownloadState::Downloading {
            existing.item = item;
            existing.is_trackable = true;
            return Ok(existing);
        }
    }
    let mut tracked = TrackedMovieDownload::new(definition, item);
    tracked.has_notified_manual_interaction_required = existing
        .as_ref()
        .map(|e| e.has_notified_manual_interaction_required)
        .unwrap_or(false);
    let download_history = store::latest_download_history(conn, tracked.download_id())?;
    if let Some(dh) = &download_history {
        tracked.state = state_from_history(dh.event_type);
    }
    let mut parsed = parse_movie_title(&tracked.item.title, false);
    if let Some(p) = parsed.clone() {
        let mapped = match &download_history {
            Some(dh) if dh.event_type == download_event::IMPORTED => {
                tracked.movie_by_id = true;
                Ok(map_by_id(conn, p, dh.movie_id)?)
            }
            _ => map_by_title(lookup, p),
        };
        match mapped {
            Ok(remote) => tracked.remote = Some(remote),
            Err(e) => {
                tracing::debug!(title = %tracked.item.title, error = %e, "found multiple movies for the download");
                // The original names the movies; their count is what the
                // lookup keeps.
                tracked.warn(format!(
                    "Unable to import automatically, found multiple movies: {e}"
                ));
                return Ok(tracked);
            }
        }
    }
    let history = store::history_by_download_id(conn, tracked.download_id())?;
    if let Some(first) = history.first() {
        let grabbed = history
            .iter()
            .find(|h| h.event_type == MovieHistoryEvent::Grabbed);
        tracked.indexer = grabbed.and_then(|g| g.data["indexer"].as_str()).map(String::from);
        tracked.added = grabbed.map(|g| g.date);
        let unmatched = parsed.is_none() || tracked.remote.as_ref().is_none_or(|r| r.movie.is_none());
        if unmatched {
            // The title the item carries did not say; the grab's does.
            parsed = parse_movie_title(first.source_title.as_deref().unwrap_or(""), false);
            if let Some(p) = parsed.clone() {
                tracked.remote = Some(map_by_id(conn, p, first.movie_id)?);
                tracked.movie_by_id = true;
            }
        }
        if let Some(remote) = &mut tracked.remote {
            remote.release.indexer = tracked.indexer.clone().unwrap_or_default();
            if let Some(p) = &remote.parsed {
                remote.release.title = p.release_title.clone();
            }
            if let Some(flags) = grabbed
                .and_then(|g| g.data["indexerFlags"].as_str())
                .and_then(parse_indexer_flags)
            {
                remote.release.indexer_flags = flags;
            }
            if let Some(dh) = &download_history {
                remote.release.indexer_id = dh.indexer_id;
            }
        }
    }
    let size = tracked.item.total_size;
    match &mut tracked.remote {
        Some(remote) => augment(catalog, remote, size),
        None => {
            tracing::trace!(title = %tracked.item.title, "no movie found for the download, tracking it anyway")
        }
    }
    Ok(tracked)
}
