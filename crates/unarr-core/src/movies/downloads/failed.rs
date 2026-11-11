//! What happens to a movie download that failed or was waved off: Radarr's
//! `FailedDownloadService`, `IgnoredDownloadService` and the rows their
//! events leave in the history, the blocklist and the download history.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::json;

use super::tracked::{download_event, TrackedMovieDownload};
use super::MovieDownloads;
use crate::decision::ReleaseSource;
use crate::downloadclients::{Client, DownloadItemStatus};
use crate::downloads::failed::{parse_release_source, text_i64};
use crate::downloads::TrackedDownloadState;
use crate::model::{Protocol, QualityModel};
use crate::movies::model::{
    MovieBlocklistEntry, MovieDownloadHistoryEntry, MovieHistoryEntry, MovieHistoryEvent,
};
use crate::movies::quality::parse_indexer_flags;
use crate::movies::store;

/// `DownloadFailedEvent`, for whoever searches again.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieDownloadFailed {
    pub movie_id: i64,
    pub quality: QualityModel,
    pub source_title: String,
    pub download_id: Option<String>,
    pub message: String,
    pub languages: Vec<i64>,
    pub skip_redownload: bool,
    pub release_source: ReleaseSource,
}

/// The grabs of a download, newest first.
fn grabbed_history(conn: &Connection, download_id: &str) -> anyhow::Result<Vec<MovieHistoryEntry>> {
    Ok(store::history_by_download_id(conn, download_id)?
        .into_iter()
        .filter(|h| h.event_type == MovieHistoryEvent::Grabbed)
        .collect())
}

/// `FailedDownloadService.Check`: an encrypted or failed item of a grab
/// becomes `FailedPending`.
pub fn check(conn: &Connection, t: &mut TrackedMovieDownload) -> anyhow::Result<()> {
    if !matches!(
        t.state,
        TrackedDownloadState::Downloading | TrackedDownloadState::ImportBlocked
    ) {
        return Ok(());
    }
    if t.item.is_encrypted || t.item.status == DownloadItemStatus::Failed {
        if grabbed_history(conn, t.download_id())?.is_empty() {
            t.warn(if t.item.is_encrypted {
                "Download is encrypted and wasn't grabbed by Radarr, skipping automatic download handling"
            } else {
                "Download has failed wasn't grabbed by Radarr, skipping automatic download handling"
            });
            return Ok(());
        }
        t.state = TrackedDownloadState::FailedPending;
    }
    Ok(())
}

/// `ProcessFailed`: the failure recorded and the item removed from the
/// client when its definition says so.
pub async fn process_failed(
    downloads: &MovieDownloads,
    t: &mut TrackedMovieDownload,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<MovieDownloadFailed>> {
    if t.state != TrackedDownloadState::FailedPending {
        return Ok(None);
    }
    let download_id = t.download_id().to_string();
    let grabbed = downloads
        .db()
        .call(move |conn| grabbed_history(conn, &download_id))
        .await?;
    let Some(first) = grabbed.into_iter().next() else {
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
    let event = publish_failed(downloads, first, &failure, Some(t), false, now).await?;
    // Only once the rows exist: a failed write leaves the item pending, to
    // be tried again on the next refresh rather than forgotten.
    t.state = TrackedDownloadState::Failed;
    Ok(Some(event))
}

/// `MarkAsFailed(trackedDownload, skipRedownload)`: from the queue.
pub async fn mark_as_failed(
    downloads: &MovieDownloads,
    t: &TrackedMovieDownload,
    skip_redownload: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<MovieDownloadFailed>> {
    let download_id = t.download_id().to_string();
    let grabbed = downloads
        .db()
        .call(move |conn| grabbed_history(conn, &download_id))
        .await?;
    let Some(first) = grabbed.into_iter().next() else {
        return Ok(None);
    };
    let event = publish_failed(
        downloads,
        first,
        "Manually marked as failed",
        Some(t),
        skip_redownload,
        now,
    )
    .await?;
    Ok(Some(event))
}

/// `IgnoredDownloadService.IgnoreDownload`: `false` when the movie is
/// unknown, as the original refuses then.
pub async fn ignore_download(
    downloads: &MovieDownloads,
    t: &TrackedMovieDownload,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let Some(remote) = t.remote.clone() else {
        tracing::warn!("unable to ignore download for unknown movie");
        return Ok(false);
    };
    let Some(movie) = remote.movie.clone() else {
        tracing::warn!("unable to ignore download for unknown movie");
        return Ok(false);
    };
    let item = t.item.clone();
    let protocol = item.client.protocol;
    let download_client = item.client.id;
    downloads
        .db()
        .call(move |conn| {
            let tx = conn.transaction()?;
            let conn: &Connection = &tx;
            let parsed = remote.parsed.as_ref();
            store::insert_history(
                conn,
                &MovieHistoryEntry {
                    id: store::next_history_id(conn)?,
                    movie_id: movie.id,
                    source_title: Some(item.title.clone()),
                    date: now,
                    event_type: MovieHistoryEvent::DownloadIgnored,
                    quality: parsed.map(|p| p.quality.quality).unwrap_or_default(),
                    languages: remote.languages.clone(),
                    data: json!({
                        "downloadClient": item.client.kind.display_name(),
                        "downloadClientName": item.client.name,
                        "message": "Manually ignored",
                        "releaseGroup": parsed.and_then(|p| p.release_group.clone()),
                        "size": item.total_size.to_string(),
                        "indexer": remote.release.indexer,
                    }),
                    download_id: Some(item.download_id.clone()),
                },
            )?;
            store::add_download_history(
                conn,
                &MovieDownloadHistoryEntry {
                    id: 0,
                    event_type: download_event::IGNORED,
                    movie_id: movie.id,
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

/// `PublishDownloadFailedEvent` and its handlers: the history row, the
/// blocklist row, the download history row, the client told to drop the
/// item.
async fn publish_failed(
    downloads: &MovieDownloads,
    history_item: MovieHistoryEntry,
    message: &str,
    t: Option<&TrackedMovieDownload>,
    skip_redownload: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<MovieDownloadFailed> {
    let data = history_item.data.clone();
    let event = MovieDownloadFailed {
        movie_id: history_item.movie_id,
        quality: history_item.quality,
        source_title: history_item.source_title.clone().unwrap_or_default(),
        download_id: history_item.download_id.clone(),
        message: message.to_string(),
        languages: history_item.languages.clone(),
        skip_redownload,
        release_source: data["releaseSource"]
            .as_str()
            .and_then(parse_release_source)
            .unwrap_or_default(),
    };
    let tracked = t.cloned();
    let rows = event.clone();
    downloads
        .db()
        .call(move |conn| {
            // History, blocklist and download history rows, all or none: a
            // crash between them would repeat the failure after a restart.
            let tx = conn.transaction()?;
            let conn: &Connection = &tx;
            let e = &rows;
            let remote = tracked.as_ref().and_then(|t| t.remote.as_ref());
            // What the tracked download knows wins over what the grab kept.
            let or_grab = |known: Option<String>, key: &str| match known {
                Some(v) => json!(v),
                None => data[key].clone(),
            };
            store::insert_history(
                conn,
                &MovieHistoryEntry {
                    id: store::next_history_id(conn)?,
                    movie_id: e.movie_id,
                    source_title: Some(e.source_title.clone()),
                    date: now,
                    event_type: MovieHistoryEvent::DownloadFailed,
                    quality: e.quality,
                    languages: e.languages.clone(),
                    data: json!({
                        "downloadClient": data["downloadClient"],
                        "downloadClientName": tracked.as_ref().map(|t| t.item.client.name.clone()),
                        "message": e.message,
                        "releaseGroup": or_grab(
                            remote
                                .and_then(|r| r.parsed.as_ref())
                                .and_then(|p| p.release_group.clone()),
                            "releaseGroup",
                        ),
                        "size": or_grab(tracked.as_ref().map(|t| t.item.total_size.to_string()), "size"),
                        "indexer": or_grab(
                            remote.map(|r| r.release.indexer.clone()).filter(|i| !i.is_empty()),
                            "indexer",
                        ),
                    }),
                    download_id: e.download_id.clone(),
                },
            )?;
            // `BlocklistService.Handle(DownloadFailedEvent)`.
            let torrent_hash = match &tracked {
                Some(t) if t.protocol == Protocol::Torrent => Some(t.item.download_id.clone()),
                _ => data["torrentInfoHash"].as_str().map(String::from),
            };
            store::insert_blocklist(
                conn,
                &MovieBlocklistEntry {
                    id: store::next_blocklist_id(conn)?,
                    movie_id: e.movie_id,
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
                    indexer_flags: data["indexerFlags"]
                        .as_str()
                        .and_then(parse_indexer_flags)
                        .unwrap_or(0),
                },
            )?;
            // `DownloadHistoryService.Handle(DownloadFailedEvent)`.
            if let Some(t) = &tracked {
                store::add_download_history(
                    conn,
                    &MovieDownloadHistoryEntry {
                        id: 0,
                        event_type: download_event::FAILED,
                        movie_id: e.movie_id,
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
