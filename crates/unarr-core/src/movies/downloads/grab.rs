//! `DownloadService.DownloadReport`, Radarr's: a release handed to a
//! client, the grab recorded in the history (`HistoryService`) and the
//! download history (`DownloadHistoryService`).

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use super::tracked::download_event;
use super::MovieDownloads;
use crate::decision::SeedConfiguration;
use crate::downloadclients::{
    download_release, Client, ClientError, DownloadClientDefinition, DownloadError, GrabRequest,
};
use crate::downloads::failed::pascal;
use crate::downloads::grab::{format_double, protocol_name, release_json};
use crate::downloads::{GrabError, Grabbed};
use crate::indexers::{IndexerDefinition, IndexerService};
use crate::model::{Protocol, ProviderKind};
use crate::movies::decision::RemoteMovie;
use crate::movies::model::{Movie, MovieDownloadHistoryEntry, MovieHistoryEntry, MovieHistoryEvent};
use crate::movies::quality::indexer_flags_text;
use crate::movies::store;

/// `MovieMetadata.IsRecentMovie`: out on disc or online in the last three
/// weeks, or in cinemas in the last four months.
pub fn is_recent_movie(movie: &Movie, now: DateTime<Utc>) -> bool {
    let m = &movie.metadata;
    let since =
        |date: Option<DateTime<Utc>>, days: i64| date.is_some_and(|d| d >= now - Duration::days(days));
    since(m.physical_release, 21) || since(m.digital_release, 21) || since(m.in_cinemas, 120)
}

pub async fn download_report(
    downloads: &MovieDownloads,
    remote: &RemoteMovie,
    download_client_id: Option<i64>,
    now: DateTime<Utc>,
) -> Result<Grabbed, GrabError> {
    let movie = remote
        .movie
        .as_ref()
        .ok_or_else(|| GrabError::Other(anyhow::anyhow!("remoteMovie.Movie must not be null")))?;
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
            let filter_blocked = release.pending_release_reason
                == Some(crate::downloads::pending::PendingReleaseReason::DownloadClientUnavailable);
            match clients
                .get_download_client(
                    release.protocol,
                    release.indexer_id,
                    filter_blocked,
                    &movie.tags,
                    now,
                )
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
            .call(move |conn| crate::store::providers::get_provider(conn, ProviderKind::Indexer, id))
            .await?
            .as_ref()
            .and_then(IndexerDefinition::from_provider)
    } else {
        None
    };
    // `SeedConfigProvider.GetSeedConfiguration`.
    let seed = match (&indexer, release.protocol) {
        (Some(i), Protocol::Torrent) => Some(SeedConfiguration {
            ratio: i.settings.seed_criteria.seed_ratio,
            seed_time_minutes: i.settings.seed_criteria.seed_time,
        }),
        _ => None,
    };
    let movie_id = movie.id;
    let blocklisted: Vec<String> = downloads
        .db()
        .call(move |conn| store::blocklist_of_movie(conn, movie_id))
        .await?
        .into_iter()
        .filter_map(|b| b.torrent_info_hash)
        .collect();
    let context = clients.context(now).await?;
    let client = Client::new(&definition, &context);
    let indexers = IndexerService::new(downloads.db().clone());
    let request = GrabRequest {
        release,
        recent: is_recent_movie(movie, now),
        seed: seed.as_ref(),
        source: remote.release_source,
        noun: "movie",
    };
    let download_id = match download_release(&client, &request, indexer.as_ref(), &blocklisted).await {
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
                    .record_failure(release.indexer_id, Duration::zero(), now)
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
    tracing::info!(
        movie = %movie.metadata.title,
        year = movie.metadata.year,
        client = %definition.name,
        indexer = %release.indexer,
        title = %release.title,
        "report sent to the download client"
    );
    let download_id = Some(download_id).filter(|id| !id.trim().is_empty());
    // The client has the release from here on: the grab happened whatever
    // becomes of its bookkeeping. The original publishes `MovieGrabbedEvent`
    // and its handlers' failures are logged, not thrown; were one to count
    // as a failed grab, the next release of the movie would be sent too.
    if let Err(e) = record_grab(downloads, remote, &definition, download_id.as_deref(), now).await {
        tracing::error!(title = %release.title, error = %format!("{e:#}"), "the grab was sent to the client but its history could not be written");
    }
    // `PendingReleaseService.Handle(MovieGrabbedEvent)`.
    let grabbed = remote.clone();
    if let Err(e) = downloads
        .db()
        .call(move |conn| super::pending::remove_grabbed(conn, &grabbed, now))
        .await
    {
        tracing::error!(title = %release.title, error = %format!("{e:#}"), "the pending releases of the grabbed movie could not be dropped");
    }
    downloads.mark_dirty();
    Ok(Grabbed {
        download_id,
        client: definition,
    })
}

/// `MovieGrabbedEvent` as the history and download history record it.
async fn record_grab(
    downloads: &MovieDownloads,
    remote: &RemoteMovie,
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
            // One transaction: the history row and its download history,
            // both or neither.
            let tx = conn.transaction()?;
            let conn: &rusqlite::Connection = &tx;
            let movie = remote.movie.as_ref().expect("checked by the caller");
            let quality = remote
                .parsed
                .as_ref()
                .map(|p| p.quality.quality)
                .unwrap_or_default();
            store::insert_history(
                conn,
                &MovieHistoryEntry {
                    id: store::next_history_id(conn)?,
                    movie_id: movie.id,
                    source_title: Some(remote.release.title.clone()),
                    date: now,
                    event_type: MovieHistoryEvent::Grabbed,
                    quality,
                    languages: remote.languages.clone(),
                    data,
                    download_id: download_id.clone(),
                },
            )?;
            // Clients that report no id leave no download history.
            if let Some(download_id) = download_id {
                store::add_download_history(
                    conn,
                    &MovieDownloadHistoryEntry {
                        id: 0,
                        event_type: download_event::GRABBED,
                        movie_id: movie.id,
                        download_id,
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
/// original keeps them.
pub fn grab_history_data(
    remote: &RemoteMovie,
    definition: &DownloadClientDefinition,
    download_id: Option<&str>,
    now: DateTime<Utc>,
) -> Value {
    let release = &remote.release;
    let age = now - release.publish_date;
    // The number Radarr's feed parsers keep of an IMDb id.
    let imdb_id = release
        .imdb_id
        .as_deref()
        .and_then(|id| id.trim_start_matches("tt").parse::<i64>().ok())
        .unwrap_or(0);
    let mut data = json!({
        "indexer": release.indexer,
        "nzbInfoUrl": release.info_url,
        "releaseGroup": remote.parsed.as_ref().and_then(|p| p.release_group.clone()),
        "age": age.num_days().to_string(),
        "ageHours": format_double(age.num_milliseconds() as f64 / 3_600_000.0),
        "ageMinutes": format_double(age.num_milliseconds() as f64 / 60_000.0),
        "publishedDate": release.publish_date.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "downloadClient": definition.kind.display_name(),
        "downloadClientName": definition.name,
        "size": release.size.to_string(),
        "downloadUrl": release.download_url,
        "guid": release.guid,
        "tmdbId": release.tmdb_id.to_string(),
        "imdbId": imdb_id.to_string(),
        "protocol": match release.protocol { Protocol::Usenet => "1", Protocol::Torrent => "2", Protocol::Unknown => "0" },
        "customFormatScore": remote.custom_format_score.to_string(),
        "movieMatchType": pascal(remote.match_type.as_str()),
        "releaseSource": pascal(remote.release_source.as_str()),
        "indexerFlags": indexer_flags_text(release.indexer_flags),
        "indexerId": release.indexer_id.to_string(),
    });
    if let Some(hash) = remote
        .parsed
        .as_ref()
        .map(|p| p.release_hash.as_str())
        .filter(|h| !h.trim().is_empty())
    {
        data["releaseHash"] = json!(hash);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_movies_are_the_originals() {
        let now: DateTime<Utc> = "2026-11-11T12:00:00Z".parse().unwrap();
        let mut movie = crate::movies::decision::testing::movie(1, "Lamb", 2021);
        movie.metadata.in_cinemas = None;
        movie.metadata.physical_release = None;
        movie.metadata.digital_release = None;
        assert!(!is_recent_movie(&movie, now));
        movie.metadata.in_cinemas = Some(now - Duration::days(119));
        assert!(is_recent_movie(&movie, now));
        movie.metadata.in_cinemas = Some(now - Duration::days(121));
        assert!(!is_recent_movie(&movie, now));
        movie.metadata.digital_release = Some(now - Duration::days(20));
        assert!(is_recent_movie(&movie, now));
        movie.metadata.digital_release = Some(now - Duration::days(22));
        movie.metadata.physical_release = Some(now - Duration::days(22));
        assert!(!is_recent_movie(&movie, now));
    }
}
