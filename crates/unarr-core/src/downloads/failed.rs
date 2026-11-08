//! What happens to a download that failed or was waved off: the originals'
//! `FailedDownloadService`, `IgnoredDownloadService` and the rows their
//! events leave in the history, the blocklist and the download history
//! (`HistoryService`, `BlocklistService`, `DownloadHistoryService`,
//! `DownloadEventHub`).

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::tracked::{TrackedDownload, TrackedDownloadState};
use super::Downloads;
use crate::decision::{ReleaseSource, RemoteEpisode};
use crate::downloadclients::{Client, DownloadItemStatus};
use crate::indexers::rss::flags;
use crate::model::{BlocklistEntry, HistoryEntry, HistoryEvent, Protocol, QualityModel, ReleaseType};
use crate::store;
use crate::store::download_history::{DownloadHistoryEntry, DownloadHistoryEvent};

/// `DownloadFailedEvent`, for whoever searches again (the scheduler).
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadFailed {
    pub series_id: i64,
    pub episode_ids: Vec<i64>,
    pub quality: QualityModel,
    pub source_title: String,
    pub download_id: Option<String>,
    pub message: String,
    pub languages: Vec<i64>,
    pub skip_redownload: bool,
    pub release_source: ReleaseSource,
}

/// `FailedDownloadService.Check`: an encrypted or failed item of a grab
/// becomes `FailedPending`.
pub fn check(conn: &Connection, t: &mut TrackedDownload) -> anyhow::Result<()> {
    if !matches!(
        t.state,
        TrackedDownloadState::Downloading | TrackedDownloadState::ImportBlocked
    ) {
        return Ok(());
    }
    if t.item.is_encrypted || t.item.status == DownloadItemStatus::Failed {
        if grabbed_history(conn, t.download_id())?.is_empty() {
            t.warn("Download wasn't grabbed by sonarr, skipping");
            return Ok(());
        }
        t.state = TrackedDownloadState::FailedPending;
    }
    Ok(())
}

/// `ProcessFailed`: the failure recorded and the item removed from the
/// client when its definition says so.
pub async fn process_failed(
    downloads: &Downloads,
    t: &mut TrackedDownload,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<DownloadFailed>> {
    if t.state != TrackedDownloadState::FailedPending {
        return Ok(None);
    }
    let download_id = t.download_id().to_string();
    let grabbed = downloads
        .db()
        .call(move |conn| grabbed_history(conn, &download_id))
        .await?;
    let Some(first) = grabbed.first().cloned() else {
        return Ok(None);
    };
    let mut failure = "Failed download detected".to_string();
    if t.item.is_encrypted {
        failure = "Encrypted download detected".into();
    } else if t.item.status == DownloadItemStatus::Failed {
        if let Some(m) = t.item.message.as_deref().filter(|m| !m.trim().is_empty()) {
            failure = m.to_string();
        }
    }
    let episode_ids = episode_ids(&grabbed);
    let event = publish_failed(downloads, first, episode_ids, &failure, Some(t), false, now).await?;
    // Only once the rows exist: a failed write leaves the item pending, to
    // be tried again on the next refresh rather than forgotten.
    t.state = TrackedDownloadState::Failed;
    Ok(Some(event))
}

/// `MarkAsFailed(trackedDownload, skipRedownload)`: from the queue.
pub async fn mark_as_failed(
    downloads: &Downloads,
    t: &TrackedDownload,
    skip_redownload: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<DownloadFailed>> {
    let download_id = t.download_id().to_string();
    let grabbed = downloads
        .db()
        .call(move |conn| grabbed_history(conn, &download_id))
        .await?;
    let Some(first) = grabbed.first().cloned() else {
        return Ok(None);
    };
    let episode_ids = episode_ids(&grabbed);
    let event = publish_failed(
        downloads,
        first,
        episode_ids,
        "Manually marked as failed",
        Some(t),
        skip_redownload,
        now,
    )
    .await?;
    Ok(Some(event))
}

/// `IgnoredDownloadService.IgnoreDownload`: `false` when the series is
/// unknown, as the originals refuse then.
pub async fn ignore_download(
    downloads: &Downloads,
    t: &TrackedDownload,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let Some(remote) = t.remote.clone() else {
        tracing::warn!("unable to ignore download for unknown series");
        return Ok(false);
    };
    let Some(series) = remote.series.clone() else {
        tracing::warn!("unable to ignore download for unknown series");
        return Ok(false);
    };
    let parsed = remote.parsed.clone().unwrap_or_default();
    let item = t.item.clone();
    let protocol = t.protocol;
    let download_client = t.download_client;
    downloads
        .db()
        .call(move |conn| {
            let tx = conn.transaction()?;
            let conn: &rusqlite::Connection = &tx;
            let data = json!({
                "downloadClient": item.client.kind.display_name(),
                "downloadClientName": item.client.name,
                "message": "Manually ignored",
                "releaseGroup": parsed.release_group,
                "size": item.total_size.to_string(),
                "releaseType": pascal(parsed.release_type().as_str()),
            });
            let first_id = store::history::next_id(conn)?;
            for (id, episode) in (first_id..).zip(remote.episodes.iter()) {
                store::history::insert(
                    conn,
                    &HistoryEntry {
                        id,
                        series_id: series.id,
                        episode_id: episode.id,
                        source_title: Some(item.title.clone()),
                        date: now,
                        event_type: HistoryEvent::DownloadIgnored,
                        quality: parsed.quality.quality,
                        languages: remote.languages.clone(),
                        data: data.clone(),
                        download_id: Some(item.download_id.clone()),
                    },
                )?;
            }
            store::download_history::insert(
                conn,
                &DownloadHistoryEntry {
                    id: 0,
                    event_type: DownloadHistoryEvent::DownloadIgnored,
                    series_id: series.id,
                    download_id: item.download_id.clone(),
                    source_title: item.title.clone(),
                    date: now,
                    protocol,
                    indexer_id: 0,
                    download_client_id: download_client,
                    release: None,
                    data: json!({
                        "downloadClient": item.client.kind.display_name(),
                        "downloadClientName": item.client.name,
                    }),
                },
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(true)
}

/// The grabs of a download, newest first.
fn grabbed_history(conn: &Connection, download_id: &str) -> anyhow::Result<Vec<HistoryEntry>> {
    Ok(store::history::find_by_download_id(conn, download_id)?
        .into_iter()
        .filter(|h| h.event_type == HistoryEvent::Grabbed)
        .collect())
}

fn episode_ids(history: &[HistoryEntry]) -> Vec<i64> {
    let mut ids = Vec::new();
    for h in history {
        if !ids.contains(&h.episode_id) {
            ids.push(h.episode_id);
        }
    }
    ids
}

/// `PublishDownloadFailedEvent` and its handlers: history rows, the
/// blocklist row, the download history row, the client told to drop the
/// item.
async fn publish_failed(
    downloads: &Downloads,
    history_item: HistoryEntry,
    episode_ids: Vec<i64>,
    message: &str,
    t: Option<&TrackedDownload>,
    skip_redownload: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<DownloadFailed> {
    let data = history_item.data.clone();
    let release_source = data["releaseSource"]
        .as_str()
        .and_then(parse_release_source)
        .unwrap_or_default();
    let event = DownloadFailed {
        series_id: history_item.series_id,
        episode_ids: episode_ids.clone(),
        quality: history_item.quality,
        source_title: history_item.source_title.clone().unwrap_or_default(),
        download_id: history_item.download_id.clone(),
        message: message.to_string(),
        languages: history_item.languages.clone(),
        skip_redownload,
        release_source,
    };
    let tracked = t.cloned();
    let message = message.to_string();
    let event_for_rows = event.clone();
    downloads
        .db()
        .call(move |conn| {
            // History, blocklist and download history rows, all or none: a
            // crash between them would repeat the failure after a restart.
            let tx = conn.transaction()?;
            let conn: &rusqlite::Connection = &tx;
            let e = &event_for_rows;
            let release_group = tracked
                .as_ref()
                .and_then(|t| t.remote.as_ref())
                .and_then(|r| r.parsed.as_ref())
                .and_then(|p| p.release_group.clone());
            let row_data = json!({
                "downloadClient": data["downloadClient"],
                "downloadClientName": tracked.as_ref().map(|t| t.item.client.name.clone()),
                "message": message,
                "releaseGroup": release_group,
                "size": tracked.as_ref().map(|t| t.item.total_size.to_string()),
            });
            let first_id = store::history::next_id(conn)?;
            for (id, episode_id) in (first_id..).zip(e.episode_ids.iter()) {
                store::history::insert(
                    conn,
                    &HistoryEntry {
                        id,
                        series_id: e.series_id,
                        episode_id: *episode_id,
                        source_title: Some(e.source_title.clone()),
                        date: now,
                        event_type: HistoryEvent::DownloadFailed,
                        quality: e.quality,
                        languages: e.languages.clone(),
                        data: row_data.clone(),
                        download_id: e.download_id.clone(),
                    },
                )?;
            }
            // `BlocklistService.Handle(DownloadFailedEvent)`.
            let torrent_hash = match &tracked {
                Some(t) if t.protocol == Protocol::Torrent => Some(t.item.download_id.clone()),
                _ => data["torrentInfoHash"].as_str().map(String::from),
            };
            store::history::insert_blocklist(
                conn,
                &BlocklistEntry {
                    id: store::history::next_blocklist_id(conn)?,
                    series_id: e.series_id,
                    episode_ids: e.episode_ids.clone(),
                    source_title: Some(e.source_title.clone()),
                    quality: e.quality,
                    date: now,
                    published_date: data["publishedDate"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.with_timezone(&Utc)),
                    size: Some(text_i64(&data["size"])),
                    protocol: match text_i64(&data["protocol"]) {
                        1 => Protocol::Usenet,
                        2 => Protocol::Torrent,
                        _ => Protocol::Unknown,
                    },
                    indexer: data["indexer"].as_str().map(String::from),
                    message: Some(e.message.clone()),
                    torrent_info_hash: torrent_hash,
                    languages: e.languages.clone(),
                    indexer_flags: data["indexerFlags"].as_str().map(flags::from_names).unwrap_or(0),
                    release_type: data["releaseType"]
                        .as_str()
                        .and_then(parse_release_type)
                        .unwrap_or(ReleaseType::Unknown),
                },
            )?;
            // `DownloadHistoryService.Handle(DownloadFailedEvent)`.
            if let Some(t) = &tracked {
                store::download_history::insert(
                    conn,
                    &DownloadHistoryEntry {
                        id: 0,
                        event_type: DownloadHistoryEvent::DownloadFailed,
                        series_id: e.series_id,
                        download_id: e.download_id.clone().unwrap_or_default(),
                        source_title: e.source_title.clone(),
                        date: now,
                        protocol: t.protocol,
                        indexer_id: 0,
                        download_client_id: t.download_client,
                        release: None,
                        data: json!({
                            "downloadClient": t.item.client.kind.display_name(),
                            "downloadClientName": t.item.client.name,
                        }),
                    },
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    // `RedownloadFailedDownloadService.Handle(DownloadFailedEvent)`.
    if let Err(e) = super::redownload::redownload(downloads, &event, now).await {
        tracing::warn!(error = %e, "unable to queue the search for the failed download");
    }
    // `DownloadEventHub.Handle(DownloadFailedEvent)`.
    if let Some(t) = t {
        if t.item.can_be_removed {
            if let Some(definition) = downloads.clients().find(t.download_client).await? {
                if definition.remove_failed_downloads {
                    let context = downloads.clients().context(now).await?;
                    let client = Client::new(&definition, &context);
                    if let Err(e) = client.remove(&t.item, true).await {
                        tracing::warn!(title = %t.item.title, error = %e, "unable to remove failed download from the client");
                    }
                }
            }
        }
    }
    Ok(event)
}

/// `BlocklistService.Block(remoteEpisode, message)`: a release blocklisted
/// before any download (a pending release removed from the queue).
pub async fn block_release(
    downloads: &Downloads,
    remote: &RemoteEpisode,
    message: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(series) = remote.series.clone() else {
        return Ok(());
    };
    let parsed = remote.parsed_or_default().into_owned();
    let entry = BlocklistEntry {
        id: 0,
        series_id: series.id,
        episode_ids: remote.episodes.iter().map(|e| e.id).collect(),
        source_title: Some(remote.release.title.clone()),
        quality: parsed.quality.quality,
        date: now,
        published_date: Some(remote.release.publish_date),
        size: Some(remote.release.size),
        protocol: remote.release.protocol,
        indexer: Some(remote.release.indexer.clone()),
        message: Some(message.to_string()),
        torrent_info_hash: remote.release.info_hash.clone(),
        languages: remote.languages.clone(),
        indexer_flags: remote.release.indexer_flags,
        release_type: parsed.release_type(),
    };
    downloads
        .db()
        .call(move |conn| {
            let entry = BlocklistEntry {
                id: store::history::next_blocklist_id(conn)?,
                ..entry
            };
            store::history::insert_blocklist(conn, &entry)
        })
        .await
}

fn text_i64(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// `Enum.ToString()`: the camel-case name with its first letter raised.
pub fn pascal(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn parse_release_source(s: &str) -> Option<ReleaseSource> {
    [
        ReleaseSource::Unknown,
        ReleaseSource::Rss,
        ReleaseSource::Search,
        ReleaseSource::UserInvokedSearch,
        ReleaseSource::InteractiveSearch,
        ReleaseSource::ReleasePush,
    ]
    .into_iter()
    .find(|r| r.as_str().eq_ignore_ascii_case(s))
}

fn parse_release_type(s: &str) -> Option<ReleaseType> {
    ReleaseType::ALL
        .iter()
        .copied()
        .find(|r| r.as_str().eq_ignore_ascii_case(s))
}
