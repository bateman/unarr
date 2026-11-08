//! `TrackedDownload` and `TrackedDownloadService.TrackDownload`: an item a
//! client reports, matched back to the series and episodes it is for,
//! through its title or the history of its grab.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::decision::lookup::{Catalog, Lookup};
use crate::decision::mapping::MappingError;
use crate::decision::{engine, mapping, RemoteEpisode};
use crate::downloadclients::{DownloadClientDefinition, DownloadClientItem};
use crate::indexers::rss::{flags, ReleaseInfo};
use crate::model::{HistoryEvent, Protocol};
use crate::parser;
use crate::store;
use crate::store::download_history::DownloadHistoryEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackedDownloadState {
    #[default]
    Downloading,
    ImportBlocked,
    ImportPending,
    Importing,
    Imported,
    FailedPending,
    Failed,
    Ignored,
}

impl TrackedDownloadState {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackedDownloadState::Downloading => "downloading",
            TrackedDownloadState::ImportBlocked => "importBlocked",
            TrackedDownloadState::ImportPending => "importPending",
            TrackedDownloadState::Importing => "importing",
            TrackedDownloadState::Imported => "imported",
            TrackedDownloadState::FailedPending => "failedPending",
            TrackedDownloadState::Failed => "failed",
            TrackedDownloadState::Ignored => "ignored",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackedDownloadStatus {
    #[default]
    Ok,
    Warning,
    Error,
}

impl TrackedDownloadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackedDownloadStatus::Ok => "ok",
            TrackedDownloadStatus::Warning => "warning",
            TrackedDownloadStatus::Error => "error",
        }
    }
}

/// `TrackedDownloadStatusMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessage {
    pub title: String,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackedDownload {
    pub download_client: i64,
    pub item: DownloadClientItem,
    /// The item with its import path resolved, once completed.
    pub import_item: Option<DownloadClientItem>,
    pub state: TrackedDownloadState,
    pub status: TrackedDownloadStatus,
    pub remote: Option<RemoteEpisode>,
    pub status_messages: Vec<StatusMessage>,
    pub protocol: Protocol,
    pub indexer: Option<String>,
    pub added: Option<DateTime<Utc>>,
    pub is_trackable: bool,
    pub has_notified_manual_interaction_required: bool,
}

impl TrackedDownload {
    pub fn new(definition: &DownloadClientDefinition, item: DownloadClientItem) -> TrackedDownload {
        TrackedDownload {
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
        }
    }

    /// `Warn(message)`: a warning status with one message under the
    /// item's title.
    pub fn warn(&mut self, message: impl Into<String>) {
        let title = self.item.title.clone();
        self.warn_messages(vec![StatusMessage {
            title,
            messages: vec![message.into()],
        }]);
    }

    pub fn warn_messages(&mut self, messages: Vec<StatusMessage>) {
        self.status = TrackedDownloadStatus::Warning;
        self.status_messages = messages;
    }

    pub fn fail(&mut self) {
        self.status = TrackedDownloadStatus::Error;
    }

    pub fn download_id(&self) -> &str {
        &self.item.download_id
    }
}

/// `GetStateFromHistory`.
fn state_from_history(event: DownloadHistoryEvent) -> TrackedDownloadState {
    match event {
        DownloadHistoryEvent::DownloadImported => TrackedDownloadState::Imported,
        DownloadHistoryEvent::DownloadFailed => TrackedDownloadState::Failed,
        DownloadHistoryEvent::DownloadIgnored => TrackedDownloadState::Ignored,
        _ => TrackedDownloadState::Downloading,
    }
}

/// `TrackDownload`: the tracked download for an item, reusing the existing
/// one when it is past downloading.
pub fn track(
    conn: &Connection,
    catalog: &Catalog,
    lookup: &dyn Lookup,
    definition: &DownloadClientDefinition,
    item: DownloadClientItem,
    existing: Option<TrackedDownload>,
    now: DateTime<Utc>,
) -> anyhow::Result<TrackedDownload> {
    if let Some(mut existing) = existing.clone() {
        if existing.state != TrackedDownloadState::Downloading {
            existing.item = item;
            existing.is_trackable = true;
            return Ok(existing);
        }
    }
    let mut tracked = TrackedDownload::new(definition, item);
    tracked.has_notified_manual_interaction_required = existing
        .as_ref()
        .map(|e| e.has_notified_manual_interaction_required)
        .unwrap_or(false);
    let history = store::history::find_by_download_id(conn, tracked.download_id())?;
    let mut parsed = parser::parse_title(&tracked.item.title);
    if let Some(p) = &parsed {
        match mapping::map(lookup, release_for(&tracked.item.title), p.clone(), None) {
            Ok(remote) => tracked.remote = Some(remote),
            Err(MappingError::MultipleSeries(titles)) => {
                tracked.warn(format!(
                    "Unable to import automatically, found multiple series: {titles}"
                ));
            }
            Err(_) => tracked.warn("Unable to parse episodes from title"),
        }
    }
    let download_history = store::download_history::latest_for_download_id(conn, tracked.download_id())?;
    if let Some(dh) = &download_history {
        tracked.state = state_from_history(dh.event_type);
    }
    if let Some(first) = history.first() {
        let grabbed = history.iter().find(|h| h.event_type == HistoryEvent::Grabbed);
        tracked.indexer = grabbed.and_then(|g| g.data["indexer"].as_str()).map(String::from);
        tracked.added = grabbed.map(|g| g.date);
        let unmatched = parsed.is_none()
            || tracked
                .remote
                .as_ref()
                .is_none_or(|r| r.series.is_none() || r.episodes.is_empty());
        if unmatched {
            // The title the item carries did not say; the grab's did.
            let source_title = first.source_title.clone().unwrap_or_default();
            let from_source = parser::parse_title(&source_title).or_else(|| {
                mapping::parse_special_episode_title(lookup, parsed.as_ref(), &source_title, 0, 0, None, None)
                    .ok()
                    .flatten()
            });
            if let Some(p) = from_source {
                let episode_ids: Vec<i64> = {
                    let mut ids: Vec<i64> = history
                        .iter()
                        .filter(|h| h.event_type == HistoryEvent::Grabbed)
                        .map(|h| h.episode_id)
                        .collect();
                    ids.dedup();
                    ids
                };
                let mut remote = RemoteEpisode::new(release_for(&tracked.item.title));
                remote.series = lookup.series_by_id(first.series_id);
                remote.episodes = store::episodes::list_episodes_by_ids(conn, &episode_ids)?;
                remote.parsed = Some(p.clone());
                parsed = Some(p);
                tracked.remote = Some(remote);
            }
        }
        if let Some(remote) = &mut tracked.remote {
            remote.release.indexer = tracked.indexer.clone().unwrap_or_default();
            if let Some(p) = &parsed {
                remote.release.title = p.release_title.clone();
            }
            if let Some(names) = grabbed.and_then(|g| g.data["indexerFlags"].as_str()) {
                remote.release.indexer_flags = flags::from_names(names);
            }
            if let Some(dh) = &download_history {
                remote.release.indexer_id = dh.indexer_id;
            }
        }
    }
    if let Some(remote) = &mut tracked.remote {
        let size = tracked.item.total_size;
        engine::augment_tracked(catalog, lookup, now, remote, size);
    }
    if tracked.remote.is_none() {
        tracing::trace!(title = %tracked.item.title, "no series and episodes found for the download, tracking it anyway");
    }
    Ok(tracked)
}

fn release_for(title: &str) -> ReleaseInfo {
    ReleaseInfo {
        title: title.to_string(),
        ..Default::default()
    }
}
