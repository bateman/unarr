//! `DownloadMonitoringService.Refresh` with the checks of
//! `FailedDownloadService` and `CompletedDownloadService` that run before
//! an import: every client asked, every item tracked, failures processed,
//! the queue rebuilt.

use crate::sync::Recover;
use chrono::{DateTime, Utc};

use super::failed;
use super::tracked::{self, TrackedDownload, TrackedDownloadState};
use super::Downloads;
use crate::decision::lookup::{Catalog, Lookup, StoreLookup};
use crate::decision::mapping;
use crate::decision::{ReleaseSource, SeriesMatchType};
use crate::downloadclients::{Client, DownloadClientDefinition, DownloadClientItem, DownloadItemStatus};
use crate::model::HistoryEvent;
use crate::store;

/// The queue rebuilt from what the clients hold; `Ok(true)` when it
/// changed.
pub async fn refresh(downloads: &Downloads, now: DateTime<Utc>) -> anyhow::Result<bool> {
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
                tracing::warn!(client = %definition.name, error = %e.exception(), "unable to retrieve queue and history items");
            }
        }
    }
    let existing = downloads.tracked_downloads();
    let config = downloads
        .db()
        .call(|conn| crate::decision::lookup::EngineConfig::load(conn))
        .await?;
    let tracked_now: Vec<TrackedDownload> = downloads
        .db()
        .call(move |conn| {
            let catalog = Catalog::load(conn, now)?;
            let lookup = StoreLookup::new(conn);
            let mut out = Vec::new();
            for (definition, item, import_item) in fetched {
                let previous = existing
                    .iter()
                    .find(|t| t.item.download_id == item.download_id)
                    .cloned();
                let title = item.title.clone();
                match tracked::track(conn, &catalog, &lookup, &definition, item, previous, now) {
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
    let trackable = |t: &TrackedDownload| -> bool {
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
        let mut cache = downloads.tracked.lock().recover();
        // `UpdateTrackable` marks downloads no client reports any more as
        // not trackable; dropping them instead keeps the cache from growing
        // for the life of the process. A finished state comes back from the
        // history when the client reports the download again.
        let seen: Vec<String> = processed.iter().map(|t| t.item.download_id.clone()).collect();
        cache.retain(|id, _| seen.contains(id));
        for mut t in processed {
            t.is_trackable = trackable(&t);
            cache.insert(t.item.download_id.clone(), t);
        }
    }
    downloads.rebuild_queue(now);
    *downloads.last_refresh.lock().recover() = Some(now);
    Ok(downloads.queue() != before)
}

/// `CompletedDownloadService.Check` up to the point an import would start:
/// the download is left `ImportPending` for the importer of a later phase.
fn completed_check(
    conn: &rusqlite::Connection,
    lookup: &StoreLookup<'_>,
    t: &mut TrackedDownload,
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
    let grabbed: Vec<_> = store::history::find_by_download_id(conn, t.download_id())?
        .into_iter()
        .filter(|h| h.event_type == HistoryEvent::Grabbed)
        .collect();
    let history_item = grabbed.iter().max_by_key(|h| h.date);
    if history_item.is_none() && t.item.category.as_deref().unwrap_or("").trim().is_empty() {
        t.warn("Download wasn't grabbed by Sonarr and not in a category, Skipping.");
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
            "[{output}] is not a valid local path. You may need a Remote Path Mapping. Check the download troubleshooting entry on the wiki for details."
        ));
        return Ok(());
    }
    let mut series = mapping::get_series(lookup, &t.item.title).ok().flatten();
    if series.is_none() {
        if let Some(h) = history_item {
            series = lookup.series_by_id(h.series_id);
        }
        let Some(h) = history_item else {
            t.warn("Series title mismatch; automatic import is not possible. Check the download troubleshooting entry on the wiki for common causes.");
            t.state = TrackedDownloadState::ImportBlocked;
            return Ok(());
        };
        if series.is_none() {
            t.warn("Series title mismatch; automatic import is not possible. Check the download troubleshooting entry on the wiki for common causes.");
            t.state = TrackedDownloadState::ImportBlocked;
            return Ok(());
        }
        let match_type = h.data["seriesMatchType"].as_str().unwrap_or("");
        let source = h.data["releaseSource"].as_str().unwrap_or("");
        if match_type.eq_ignore_ascii_case(SeriesMatchType::Id.as_str())
            && !source.eq_ignore_ascii_case(ReleaseSource::InteractiveSearch.as_str())
        {
            t.warn("Found matching series via grab history, but release was matched to series by ID. Automatic import is not possible. See the FAQ for details.");
            t.state = TrackedDownloadState::ImportBlocked;
            return Ok(());
        }
    }
    t.state = TrackedDownloadState::ImportPending;
    Ok(())
}
