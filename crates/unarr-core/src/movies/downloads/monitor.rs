//! `DownloadMonitoringService.Refresh`, Radarr's, with the checks of
//! `FailedDownloadService` and `CompletedDownloadService` that run before
//! an import: every client asked, every item tracked, failures processed,
//! the queue rebuilt.

use chrono::{DateTime, Utc};

use super::failed;
use super::tracked::{self, TrackedMovieDownload};
use super::MovieDownloads;
use crate::decision::lookup::EngineConfig;
use crate::decision::ReleaseSource;
use crate::downloadclients::{Client, DownloadClientDefinition, DownloadClientItem, DownloadItemStatus};
use crate::downloads::TrackedDownloadState;
use crate::movies::decision::{mapping, MovieCatalog, MovieLookup, MovieMatchType, StoreMovieLookup};
use crate::movies::model::{Movie, MovieHistoryEvent};
use crate::movies::parser::parse_movie_title;
use crate::movies::store;

/// The queue rebuilt from what the clients hold; `Ok(true)` when it
/// changed.
pub async fn refresh(downloads: &MovieDownloads, now: DateTime<Utc>) -> anyhow::Result<bool> {
    let clients = downloads.clients();
    let definitions = clients.download_handling_enabled(true, now).await?;
    let context = clients.context(now).await?;
    // What each client holds, with the import path of completed items.
    let mut fetched: Vec<(
        DownloadClientDefinition,
        DownloadClientItem,
        Option<DownloadClientItem>,
    )> = Vec::new();
    for definition in definitions {
        let client = Client::new(&definition, &context);
        match client.items().await {
            Ok(items) => {
                clients.record_success(definition.id).await;
                for item in items {
                    let import_item = if item.status == DownloadItemStatus::Completed {
                        client.import_item(&item).await.ok()
                    } else {
                        None
                    };
                    fetched.push((definition.clone(), item, import_item));
                }
            }
            Err(e) => {
                clients.record_failure(definition.id, now).await;
                tracing::warn!(client = %definition.name, error = %e, "unable to retrieve queue and history items");
            }
        }
    }
    let existing = downloads.tracked_downloads();
    let config = downloads.db().call(|conn| EngineConfig::load(conn)).await?;
    let tracked_now: Vec<TrackedMovieDownload> = downloads
        .db()
        .call(move |conn| {
            let catalog = MovieCatalog::load(conn, now)?;
            let lookup = StoreMovieLookup { conn };
            let mut out = Vec::new();
            for (definition, item, import_item) in fetched {
                let previous = existing
                    .iter()
                    .find(|t| t.item.download_id == item.download_id)
                    .cloned();
                let title = item.title.clone();
                match tracked::track(conn, &catalog, &lookup, &definition, item, previous) {
                    Ok(mut t) => {
                        if import_item.is_some() {
                            t.import_item = import_item;
                        }
                        if matches!(
                            t.state,
                            TrackedDownloadState::Downloading | TrackedDownloadState::ImportBlocked
                        ) {
                            failed::check(conn, &mut t)?;
                            completed_check(conn, &lookup, &mut t)?;
                        }
                        out.push(t);
                    }
                    Err(e) => tracing::error!(title, error = %e, "couldn't process tracked download"),
                }
            }
            Ok(out)
        })
        .await?;
    // `DownloadProcessingService`: failures past their check are recorded.
    let mut processed = Vec::with_capacity(tracked_now.len());
    for mut t in tracked_now {
        if t.state == TrackedDownloadState::FailedPending {
            if let Err(e) = failed::process_failed(downloads, &mut t, now).await {
                tracing::error!(title = %t.item.title, error = %e, "failed to process the failed download");
            }
        }
        processed.push(t);
    }
    let trackable = |t: &TrackedMovieDownload| -> bool {
        // Imported, failed or ignored downloads are left alone; so are
        // completed ones when completed download handling is off.
        if matches!(
            t.state,
            TrackedDownloadState::Imported | TrackedDownloadState::Failed | TrackedDownloadState::Ignored
        ) {
            return false;
        }
        if !config.enable_completed_download_handling && t.item.status == DownloadItemStatus::Completed {
            return false;
        }
        true
    };
    let before = downloads.queue();
    {
        let mut cache = downloads.tracked.lock().unwrap();
        // Downloads no client reports any more are dropped rather than kept
        // as not trackable, so the cache does not grow for the life of the
        // process; a finished state comes back from the history.
        let seen: Vec<String> = processed.iter().map(|t| t.item.download_id.clone()).collect();
        cache.retain(|id, _| seen.contains(id));
        for mut t in processed {
            t.is_trackable = trackable(&t);
            cache.insert(t.item.download_id.clone(), t);
        }
    }
    downloads.rebuild_queue(now);
    Ok(downloads.queue() != before)
}

/// `ParsingService.GetMovie(title)`.
fn movie_for_title(lookup: &dyn MovieLookup, title: &str) -> Option<Movie> {
    match parse_movie_title(title, false) {
        Some(parsed) => mapping::find_movie(lookup, &parsed, "", 0, None)
            .ok()
            .flatten()
            .map(|(movie, _)| movie),
        None => mapping::find_by_title(lookup, &[title.to_string()], None)
            .ok()
            .flatten(),
    }
}

/// `CompletedDownloadService.Check` up to the point an import would start:
/// the download is left `ImportPending` for the importer of milestone 4c.
fn completed_check(
    conn: &rusqlite::Connection,
    lookup: &dyn MovieLookup,
    t: &mut TrackedMovieDownload,
) -> anyhow::Result<()> {
    if t.item.status != DownloadItemStatus::Completed {
        return Ok(());
    }
    if t.import_item.is_none() {
        t.import_item = Some(t.item.clone());
    }
    if !matches!(
        t.state,
        TrackedDownloadState::Downloading | TrackedDownloadState::ImportBlocked
    ) {
        return Ok(());
    }
    let grabbed: Vec<_> = store::history_by_download_id(conn, t.download_id())?
        .into_iter()
        .filter(|h| h.event_type == MovieHistoryEvent::Grabbed)
        .collect();
    let history_item = grabbed.iter().max_by_key(|h| h.date);
    if history_item.is_none() && t.item.category.as_deref().unwrap_or("").trim().is_empty() {
        t.warn("Download wasn't grabbed by Radarr and not in a category, Skipping.");
        return Ok(());
    }
    let output = t
        .import_item
        .as_ref()
        .and_then(|i| i.output_path.clone())
        .unwrap_or_default();
    if output.is_empty() {
        t.warn("Download doesn't contain intermediate path, Skipping.");
        return Ok(());
    }
    if !crate::downloadclients::remote_path::is_rooted(&output)
        || output.contains('\\') && !output.starts_with('/')
    {
        t.warn(format!(
            "[{output}] is not a valid local path. You may need a Remote Path Mapping."
        ));
        return Ok(());
    }
    if movie_for_title(lookup, &t.item.title).is_none() {
        let movie = match history_item {
            Some(h) => store::get_movie(conn, h.movie_id)?,
            None => None,
        };
        let (Some(_), Some(h)) = (movie, history_item) else {
            t.warn("Movie title mismatch, automatic import is not possible. Manual Import required.");
            t.state = TrackedDownloadState::ImportBlocked;
            return Ok(());
        };
        let match_type = h.data["movieMatchType"].as_str().unwrap_or("");
        let source = h.data["releaseSource"].as_str().unwrap_or("");
        if match_type.eq_ignore_ascii_case(MovieMatchType::Id.as_str())
            && !source.eq_ignore_ascii_case(ReleaseSource::InteractiveSearch.as_str())
        {
            t.warn(
                "Found matching movie via grab history, but release was matched to movie by ID. Manual Import required.",
            );
            t.state = TrackedDownloadState::ImportBlocked;
            return Ok(());
        }
    }
    t.state = TrackedDownloadState::ImportPending;
    Ok(())
}
