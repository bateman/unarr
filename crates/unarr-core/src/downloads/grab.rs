//! `DownloadService.DownloadReport`: a release handed to a client, the
//! grab recorded in the history (`HistoryService`) and the download
//! history (`DownloadHistoryService`).

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::failed::pascal;
use super::Downloads;
use crate::decision::RemoteEpisode;
use crate::downloadclients::{download, Client, ClientError, DownloadClientDefinition, DownloadError};
use crate::indexers::{IndexerDefinition, IndexerService};
use crate::model::{HistoryEntry, HistoryEvent, Protocol, ProviderKind};
use crate::store;
use crate::store::download_history::{DownloadHistoryEntry, DownloadHistoryEvent};

#[derive(Debug, Clone)]
pub struct Grabbed {
    pub download_id: Option<String>,
    pub client: DownloadClientDefinition,
}

/// What a grab can fail with, by how the originals' API answers it.
#[derive(Debug)]
pub enum GrabError {
    /// `DownloadClientUnavailableException` or a client that does not
    /// exist: a 500 in the originals.
    NoClient(String),
    /// `ReleaseDownloadException`: a 409.
    ReleaseDownload(String),
    /// `ReleaseUnavailableException` / `ReleaseBlockedException`: a 409
    /// too, but a decision the engine drops rather than keeps pending.
    ReleaseUnavailable(String),
    /// `DownloadClientException`: a 500.
    Client(ClientError),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for GrabError {
    fn from(e: anyhow::Error) -> Self {
        GrabError::Other(e)
    }
}

impl std::fmt::Display for GrabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrabError::NoClient(m) | GrabError::ReleaseDownload(m) | GrabError::ReleaseUnavailable(m) => {
                f.write_str(m)
            }
            GrabError::Client(e) => f.write_str(e.message()),
            GrabError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// `DownloadProtocol.ToString()`.
pub(crate) fn protocol_name(p: Protocol) -> &'static str {
    match p {
        Protocol::Usenet => "Usenet",
        Protocol::Torrent => "Torrent",
        Protocol::Unknown => "Unknown",
    }
}

pub async fn download_report(
    downloads: &Downloads,
    remote: &RemoteEpisode,
    download_client_id: Option<i64>,
    now: DateTime<Utc>,
) -> Result<Grabbed, GrabError> {
    let series = remote
        .series
        .as_ref()
        .ok_or_else(|| GrabError::Other(anyhow::anyhow!("remoteEpisode.Series must not be null")))?;
    if remote.episodes.is_empty() {
        return Err(GrabError::Other(anyhow::anyhow!(
            "remoteEpisode.Episodes must have items"
        )));
    }
    let clients = downloads.clients();
    let release = &remote.release;
    let definition = match download_client_id {
        Some(id) => clients
            .find(id)
            .await?
            .filter(|d| d.enable)
            .ok_or_else(|| GrabError::NoClient("Sequence contains no matching element".into()))?,
        None => {
            // Only a release pending for want of a client avoids blocked ones.
            match clients
                .get_download_client(release.protocol, release.indexer_id, false, &series.tags, now)
                .await?
            {
                Ok(Some(d)) => d,
                Ok(None) => {
                    return Err(GrabError::NoClient(format!(
                        "{} Download client isn't configured yet",
                        protocol_name(release.protocol)
                    )))
                }
                Err(e) => return Err(GrabError::NoClient(e.0)),
            }
        }
    };
    let indexer = if release.indexer_id > 0 {
        let id = release.indexer_id;
        downloads
            .db()
            .call(move |conn| store::providers::get_provider(conn, ProviderKind::Indexer, id))
            .await?
            .as_ref()
            .and_then(IndexerDefinition::from_provider)
    } else {
        None
    };
    let series_id = series.id;
    let blocklisted: Vec<String> = downloads
        .db()
        .call(move |conn| store::history::blocklist_for_series(conn, series_id))
        .await?
        .into_iter()
        .filter_map(|b| b.torrent_info_hash)
        .collect();
    let context = clients.context(now).await?;
    let client = Client::new(&definition, &context);
    let indexers = IndexerService::new(downloads.db().clone());
    let download_id = match download(&client, remote, indexer.as_ref(), &blocklisted).await {
        Ok(id) => {
            clients.record_success(definition.id).await;
            if release.indexer_id > 0 {
                indexers.record_success(release.indexer_id, None).await;
            }
            id
        }
        Err(DownloadError::ReleaseDownload(m)) => {
            if release.indexer_id > 0 {
                indexers
                    .record_failure(release.indexer_id, chrono::Duration::zero(), now)
                    .await;
            }
            return Err(GrabError::ReleaseDownload(m));
        }
        Err(DownloadError::ReleaseUnavailable(m)) | Err(DownloadError::ReleaseBlocked(m)) => {
            return Err(GrabError::ReleaseUnavailable(m))
        }
        Err(DownloadError::Client(ClientError::Rejected(m))) => return Err(GrabError::ReleaseDownload(m)),
        Err(DownloadError::Client(e)) => return Err(GrabError::Client(e)),
        Err(DownloadError::InvalidNzb(m)) => return Err(GrabError::Client(ClientError::Other(m))),
    };
    tracing::info!(client = %definition.name, indexer = %release.indexer, title = %release.title, "report sent to the download client");
    let download_id = Some(download_id).filter(|id| !id.trim().is_empty());
    // The client has the release from here on: the grab happened whatever
    // becomes of its bookkeeping. The originals publish `EpisodeGrabbedEvent`
    // and its handlers' failures are logged, not thrown; were one to count
    // as a failed grab, the next release of the episode would be sent too.
    if let Err(e) = record_grab(downloads, remote, &definition, download_id.as_deref(), now).await {
        tracing::error!(title = %release.title, error = %format!("{e:#}"), "the grab was sent to the client but its history could not be written");
    }
    // `EpisodeControllerWithSignalR.Handle(EpisodeGrabbedEvent)`.
    downloads.grabbed(remote.episodes.iter().map(|e| e.id).collect());
    // `NotificationService.Handle(EpisodeGrabbedEvent)`.
    if let Some(notifier) = downloads.notifier() {
        notifier
            .on_grab(remote, &definition, download_id.as_deref(), now)
            .await;
    }
    // `PendingReleaseService.Handle(EpisodeGrabbedEvent)`.
    let grabbed_remote = remote.clone();
    if let Err(e) = downloads
        .db()
        .call(move |conn| super::pending::remove_grabbed(conn, &grabbed_remote, now))
        .await
    {
        tracing::error!(title = %release.title, error = %format!("{e:#}"), "the pending releases of the grabbed episodes could not be dropped");
    }
    downloads.mark_dirty();
    // `DownloadMonitoringService.Handle(EpisodeGrabbedEvent)`: the clients
    // are asked again, whoever sent the release (a command, the API). One
    // refresh waiting is enough for any number of grabs.
    if let Some(queue) = downloads.commands() {
        if let Err(e) = queue
            .push(
                crate::commands::CommandBody::RefreshMonitoredDownloads,
                crate::commands::CommandPriority::High,
                crate::commands::CommandTrigger::Unspecified,
                now,
            )
            .await
        {
            tracing::warn!(error = %e, "failed to queue the refresh of the downloads");
        }
    }
    Ok(Grabbed {
        download_id,
        client: definition,
    })
}

/// `EpisodeGrabbedEvent` as the history and download history record it.
async fn record_grab(
    downloads: &Downloads,
    remote: &RemoteEpisode,
    definition: &DownloadClientDefinition,
    download_id: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let data = grab_history_data(remote, definition, download_id, now);
    let remote = remote.clone();
    let definition = definition.clone();
    let download_id = download_id.map(String::from);
    downloads
        .db()
        .call(move |conn| {
            // One transaction: a grab is a history row per episode plus its
            // download history, all or none.
            let tx = conn.transaction()?;
            let conn: &rusqlite::Connection = &tx;
            let parsed = remote.parsed.clone().unwrap_or_default();
            let series = remote.series.as_ref().expect("checked by the caller");
            let first_id = store::history::next_ids(conn, remote.episodes.len())?;
            for (id, episode) in (first_id..).zip(remote.episodes.iter()) {
                store::history::insert(
                    conn,
                    &HistoryEntry {
                        id,
                        series_id: episode.series_id,
                        episode_id: episode.id,
                        source_title: Some(remote.release.title.clone()),
                        date: now,
                        event_type: HistoryEvent::Grabbed,
                        quality: parsed.quality.quality,
                        languages: remote.languages.clone(),
                        data: data.clone(),
                        download_id: download_id.clone(),
                    },
                )?;
            }
            // Clients that report no id leave no download history.
            if let Some(download_id) = &download_id {
                store::download_history::insert(
                    conn,
                    &DownloadHistoryEntry {
                        id: 0,
                        event_type: DownloadHistoryEvent::DownloadGrabbed,
                        series_id: series.id,
                        download_id: download_id.clone(),
                        source_title: remote.release.title.clone(),
                        date: now,
                        protocol: remote.release.protocol,
                        indexer_id: remote.release.indexer_id,
                        download_client_id: definition.id,
                        release: Some(release_json(&remote.release)),
                        data: json!({
                            "indexer": remote.release.indexer,
                            "downloadClient": definition.kind.display_name(),
                            "downloadClientName": definition.name,
                            "customFormatScore": remote.custom_format_score.to_string(),
                        }),
                    },
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

/// The `data` of a `Grabbed` history row, every value a string as the
/// originals keep them.
pub fn grab_history_data(
    remote: &RemoteEpisode,
    definition: &DownloadClientDefinition,
    download_id: Option<&str>,
    now: DateTime<Utc>,
) -> Value {
    let release = &remote.release;
    let parsed = remote.parsed_or_default();
    let age = now - release.publish_date;
    let mut data = json!({
        "indexer": release.indexer,
        "nzbInfoUrl": release.info_url,
        "releaseGroup": parsed.release_group,
        "age": age.num_days().to_string(),
        "ageHours": format_double(age.num_milliseconds() as f64 / 3_600_000.0),
        "ageMinutes": format_double(age.num_milliseconds() as f64 / 60_000.0),
        "publishedDate": release.publish_date.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "downloadClient": definition.kind.display_name(),
        "downloadClientName": definition.name,
        "size": release.size.to_string(),
        "downloadUrl": release.download_url,
        "guid": release.guid,
        "tvdbId": release.tvdb_id.to_string(),
        "tvRageId": release.tv_rage_id.to_string(),
        "imdbId": release.imdb_id,
        "protocol": match release.protocol { Protocol::Usenet => "1", Protocol::Torrent => "2", Protocol::Unknown => "0" },
        "customFormatScore": remote.custom_format_score.to_string(),
        "seriesMatchType": pascal(remote.series_match_type.as_str()),
        "releaseSource": pascal(remote.release_source.as_str()),
        "indexerFlags": flag_names(release.indexer_flags),
        "releaseType": pascal(parsed.release_type().as_str()),
    });
    if !parsed.release_hash.is_empty() {
        data["releaseHash"] = json!(parsed.release_hash);
    }
    if release.protocol == Protocol::Torrent {
        // The hash is learned at grab time when the indexer did not say.
        let hash = release
            .info_hash
            .clone()
            .filter(|h| !h.trim().is_empty())
            .or_else(|| download_id.map(String::from));
        data["torrentInfoHash"] = json!(hash);
    }
    data
}

/// `IndexerFlags` members, bit and name.
fn flag_table() -> [(i64, &'static str); 8] {
    use crate::indexers::rss::flags::*;
    [
        (FREELEECH, "G_Freeleech"),
        (HALFLEECH, "G_Halfleech"),
        (DOUBLE_UPLOAD, "G_DoubleUpload"),
        (INTERNAL, "G_Internal"),
        (SCENE, "G_Scene"),
        (FREELEECH75, "G_Freeleech75"),
        (FREELEECH25, "G_Freeleech25"),
        (NUKED, "Nuked"),
    ]
}

/// `EpisodeFile.IndexerFlags.ToString()`: the file's flags are the
/// parser's `IndexerFlags`, whose names have no `G_` prefix.
pub fn file_flag_names(flags: i64) -> String {
    flag_names(flags).replace("G_", "")
}

/// `IndexerFlags.ToString()`: the flag names, or `0`.
pub fn flag_names(flags: i64) -> String {
    let names: Vec<&str> = flag_table()
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if names.is_empty() {
        flags.to_string()
    } else {
        names.join(", ")
    }
}

/// `Enum.TryParse<IndexerFlags>(text, ignoreCase)`: the names (or the
/// number) the history stored, back to bits; `None` for text that is not
/// a flag list.
pub fn parse_flag_names(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(n) = text.parse::<i64>() {
        return Some(n);
    }
    let table = flag_table();
    let mut flags = 0;
    for part in text.split(',') {
        let (bit, _) = table
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(part.trim()))?;
        flags |= bit;
    }
    Some(flags)
}

/// `double.ToString()`: the shortest text that reads back as the number.
pub(crate) fn format_double(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// `ReleaseInfo` as the download history keeps it.
pub(crate) fn release_json(r: &crate::indexers::rss::ReleaseInfo) -> Value {
    json!({
        "guid": r.guid,
        "title": r.title,
        "size": r.size,
        "downloadUrl": r.download_url,
        "infoUrl": r.info_url,
        "indexerId": r.indexer_id,
        "indexer": r.indexer,
        "downloadProtocol": r.protocol.as_str(),
        "publishDate": crate::time::format_utc(r.publish_date),
        "infoHash": r.info_hash,
        "magnetUrl": r.magnet_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_flags_and_formats_doubles() {
        assert_eq!(flag_names(0), "0");
        assert_eq!(flag_names(1 | 16), "G_Freeleech, G_Scene");
        assert_eq!(format_double(12.0), "12");
        assert_eq!(format_double(7234.519722222222), "7234.519722222222");
        assert_eq!(pascal("interactiveSearch"), "InteractiveSearch");
    }
}
