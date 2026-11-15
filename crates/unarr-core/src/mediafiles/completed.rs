//! `CompletedDownloadService.Import`/`VerifyImport`, `RejectedImportService`,
//! `DownloadProcessingService` and `DownloadEventHub`: the completed
//! downloads the monitor left pending are imported, told apart as fully
//! imported or blocked, marked imported at the client and removed from it
//! when its settings say so.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::decide::Environment;
use super::downloaded;
use super::events::{self, DownloadCompleted, Events, ImportEvent, ManualInteractionRequired};
use super::local::{ImportMode, ImportResult, ImportResultType, RejectionReason};
use crate::db::Db;
use crate::downloadclients::{Client, DownloadItemStatus};
use crate::downloads::{failed, Downloads, StatusMessage, TrackedDownload, TrackedDownloadState};
use crate::indexers::settings::NewznabSettings;
use crate::model::{HistoryEvent, ProviderKind};
use crate::store;

/// `FailDownloads.Executables`.
const FAIL_EXECUTABLES: i64 = 0;
/// `FailDownloads.PotentiallyDangerous`.
const FAIL_POTENTIALLY_DANGEROUS: i64 = 1;

/// `ValidatePath`.
fn validate_path(t: &mut TrackedDownload) -> bool {
    let output = t
        .import_item
        .as_ref()
        .and_then(|i| i.output_path.clone())
        .unwrap_or_default();
    if output.is_empty() {
        t.warn("Download doesn't contain intermediate path, Skipping.");
        return false;
    }
    if !crate::downloadclients::remote_path::is_rooted(&output)
        || output.contains('\\') && !output.starts_with('/')
    {
        t.warn(format!(
            "[{output}] is not a valid local path. You may need a Remote Path Mapping. Check the download troubleshooting entry on the wiki for details."
        ));
        return false;
    }
    true
}

/// `SetStateToImportBlocked`: blocked, and told once.
fn set_import_blocked(db: &Db, t: &mut TrackedDownload, events: &mut Events) {
    t.state = TrackedDownloadState::ImportBlocked;
    if t.has_notified_manual_interaction_required {
        return;
    }
    t.has_notified_manual_interaction_required = true;
    let download_id = t.download_id().to_string();
    let grabbed = db
        .call_blocking(move |conn| {
            Ok(store::history::find_by_download_id(conn, &download_id)?
                .into_iter()
                .filter(|h| h.event_type == HistoryEvent::Grabbed)
                .collect::<Vec<_>>())
        })
        .unwrap_or_default();
    let release = (!grabbed.is_empty()).then(|| super::aggregate::grabbed_release(&grabbed));
    events.push(ImportEvent::ManualInteractionRequired(Box::new(
        ManualInteractionRequired {
            tracked: t.clone(),
            release,
        },
    )));
}

/// `RejectedImportService.Process`: a single rejected file is a warning,
/// or a failure when the indexer asks for dangerous or executable files
/// to fail the download.
fn process_rejected(db: &Db, t: &mut TrackedDownload, result: &ImportResult) -> bool {
    let Some(release) = t.remote.as_ref().map(|r| &r.release) else {
        return false;
    };
    if result.result() != ImportResultType::Rejected {
        return false;
    }
    let indexer_id = release.indexer_id;
    let settings = db
        .call_blocking(move |conn| store::providers::get_provider(conn, ProviderKind::Indexer, indexer_id))
        .ok()
        .flatten()
        .map(|p| NewznabSettings::from_value(&p.settings));
    let reason = result.decision.rejections.first().map(|r| r.reason);
    let title = t.item.title.clone();
    let Some(settings) = settings else {
        t.warn_messages(vec![StatusMessage {
            title,
            messages: result.errors.clone(),
        }]);
        return true;
    };
    match reason {
        Some(RejectionReason::DangerousFile)
            if settings.fail_downloads.contains(&FAIL_POTENTIALLY_DANGEROUS) =>
        {
            tracing::trace!("Download '{title}' contains potentially dangerous file, marking as failed");
            t.fail();
        }
        Some(RejectionReason::ExecutableFile) if settings.fail_downloads.contains(&FAIL_EXECUTABLES) => {
            tracing::trace!("Download '{title}' contains executable file, marking as failed");
            t.fail();
        }
        _ => t.warn_messages(vec![StatusMessage {
            title,
            messages: result.errors.clone(),
        }]),
    }
    true
}

/// `VerifyImport(trackedDownload, importResults)`: `true` when every
/// episode of the release is in the library, by this import or by an
/// earlier one the history remembers.
pub fn verify_import(
    db: &Db,
    t: &mut TrackedDownload,
    results: &[ImportResult],
    now: DateTime<Utc>,
    events: &mut Events,
) -> bool {
    let Some(remote) = t.remote.clone() else {
        return false;
    };
    let Some(series) = remote.series.as_ref() else {
        return false;
    };
    let imported_count: usize = results
        .iter()
        .filter(|r| r.result() == ImportResultType::Imported)
        .map(|r| {
            r.decision
                .local_episode
                .as_ref()
                .map(|l| l.episodes.len())
                .unwrap_or(0)
        })
        .sum();
    let all_imported = imported_count >= remote.episodes.len().max(1);
    let download_id = t.download_id().to_string();
    let mut history = db
        .call_blocking(move |conn| store::history::find_by_download_id(conn, &download_id))
        .unwrap_or_default();
    history.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
    let grabbed: Vec<_> = history
        .iter()
        .filter(|h| h.event_type == HistoryEvent::Grabbed)
        .cloned()
        .collect();
    let release = (!grabbed.is_empty()).then(|| super::aggregate::grabbed_release(&grabbed));
    let complete = |t: &mut TrackedDownload, files, events: &mut Events| {
        t.state = TrackedDownloadState::Imported;
        let ev = DownloadCompleted {
            tracked: t.clone(),
            series_id: series.id,
            files,
            release: release.clone(),
        };
        let ev2 = ev.clone();
        if let Err(e) = db.call_blocking(move |conn| events::record_download_completed(conn, &ev2, now)) {
            tracing::error!(error = %e, "failed to record the completed download");
        }
        events.push(ImportEvent::DownloadCompleted(Box::new(ev)));
    };
    if all_imported {
        tracing::debug!("All episodes were imported for {}", t.item.title);
        let files = results
            .iter()
            .filter(|r| r.result() == ImportResultType::Imported)
            .filter_map(|r| r.episode_file.clone())
            .collect();
        complete(t, files, events);
        return true;
    }
    // `TrackedDownloadAlreadyImported.IsImported`: the newest history row of
    // every episode is an import.
    let at_least_one = results.iter().any(|r| r.result() == ImportResultType::Imported);
    let all_in_history = !history.is_empty()
        && remote.episodes.iter().all(|e| {
            history
                .iter()
                .find(|h| h.episode_id == e.id)
                .is_some_and(|h| h.event_type == HistoryEvent::DownloadFolderImported)
        });
    if all_in_history {
        if at_least_one {
            tracing::debug!("All episodes were imported in history for {}", t.item.title);
        } else {
            tracing::debug!(
                "No Episodes were just imported, but all episodes were previously imported, possible issue with download history."
            );
        }
        let ids: Vec<i64> = remote.episodes.iter().map(|e| e.id).collect();
        let files = db
            .call_blocking(move |conn| {
                let episodes = store::episodes::list_episodes_by_ids(conn, &ids)?;
                let mut file_ids: Vec<i64> = episodes
                    .iter()
                    .map(|e| e.episode_file_id)
                    .filter(|i| *i > 0)
                    .collect();
                file_ids.sort_unstable();
                file_ids.dedup();
                store::episodes::list_files_by_ids(conn, &file_ids)
            })
            .unwrap_or_default();
        complete(t, files, events);
        return true;
    }
    tracing::debug!(
        "Not all episodes have been imported for the release '{}'",
        t.item.title
    );
    false
}

/// `Import(trackedDownload)`: the download's folder processed, the state
/// moved on. Runs on a blocking thread.
pub fn import(db: &Db, env: &Environment, t: &mut TrackedDownload, now: DateTime<Utc>) -> Events {
    let mut events = Events::default();
    if t.import_item.is_none() {
        t.import_item = Some(t.item.clone());
    }
    if !validate_path(t) {
        return events;
    }
    let Some(series) = t.remote.as_ref().and_then(|r| r.series.clone()) else {
        t.warn("Unable to parse download, automatic import is not possible.");
        set_import_blocked(db, t, &mut events);
        return events;
    };
    t.state = TrackedDownloadState::Importing;
    let import_item = t.import_item.clone().expect("set above");
    let output_path = import_item.output_path.clone().unwrap_or_default();
    let imported = downloaded::process_path(
        db,
        env,
        &output_path,
        ImportMode::Auto,
        Some(&series),
        Some(&import_item),
        now,
    );
    events.extend(imported.events);
    for series_id in imported.rescan_series {
        events.push(ImportEvent::RescanRequested { series_id });
    }
    let results = imported.results;
    if verify_import(db, t, &results, now, &mut events) {
        return events;
    }
    t.state = TrackedDownloadState::ImportPending;
    if results.is_empty() {
        t.warn(format!("No files found are eligible for import in {output_path}"));
        return events;
    }
    if results.len() == 1 && process_rejected(db, t, &results[0]) {
        return events;
    }
    let mut messages = vec![StatusMessage {
        title: "One or more episodes expected in this release were not imported or missing from the release"
            .into(),
        messages: Vec::new(),
    }];
    let mut not_imported: Vec<&ImportResult> = results
        .iter()
        .filter(|r| r.result() != ImportResultType::Imported && r.decision.local_episode.is_some())
        .collect();
    not_imported.sort_by(|a, b| {
        let pa = &a.decision.local_episode.as_ref().unwrap().path;
        let pb = &b.decision.local_episode.as_ref().unwrap().path;
        pa.cmp(pb)
    });
    for r in not_imported {
        let path = &r.decision.local_episode.as_ref().unwrap().path;
        messages.push(StatusMessage {
            title: super::file_name(path).to_string(),
            messages: r.errors.clone(),
        });
    }
    t.warn_messages(messages);
    set_import_blocked(db, t, &mut events);
    events
}

/// `DownloadEventHub.Handle(DownloadCompletedEvent)`: the item marked
/// imported at the client, and removed when the client's settings say so.
pub async fn download_completed(downloads: &Downloads, t: &mut TrackedDownload, now: DateTime<Utc>) {
    let Ok(Some(definition)) = downloads.clients().find(t.download_client).await else {
        return;
    };
    let Ok(context) = downloads.clients().context(now).await else {
        return;
    };
    let client = Client::new(&definition, &context);
    tracing::debug!(
        "[{}] Marking download as imported from {}",
        t.item.title,
        t.item.client.name
    );
    if let Err(e) = client.mark_imported(&t.item).await {
        tracing::debug!(error = %e.exception(), "couldn't mark the item as imported");
    }
    if t.item.removed || !t.item.can_be_removed || t.item.status == DownloadItemStatus::Downloading {
        return;
    }
    if !definition.remove_completed_downloads {
        return;
    }
    remove_from_client(&client, t).await;
}

/// `DownloadEventHub.Handle(DownloadCanBeRemovedEvent)`.
async fn download_can_be_removed(downloads: &Downloads, t: &mut TrackedDownload, now: DateTime<Utc>) {
    if t.item.removed || !t.item.can_be_removed {
        return;
    }
    let Ok(Some(definition)) = downloads.clients().find(t.download_client).await else {
        return;
    };
    if !definition.remove_completed_downloads {
        return;
    }
    let Ok(context) = downloads.clients().context(now).await else {
        return;
    };
    let client = Client::new(&definition, &context);
    remove_from_client(&client, t).await;
}

async fn remove_from_client(client: &Client<'_>, t: &mut TrackedDownload) {
    tracing::debug!(
        "[{}] Removing download from {} history",
        t.item.title,
        t.item.client.name
    );
    match client.remove(&t.item, true).await {
        Ok(()) => t.item.removed = true,
        Err(e) => {
            tracing::error!(error = %e.exception(), "Couldn't remove item {} from client {}", t.item.title, t.item.client.name)
        }
    }
}

/// `DownloadProcessingService.Execute(ProcessMonitoredDownloadsCommand)`:
/// the pending imports, then the failures, then the imported downloads the
/// clients may drop. Returns the events for the relays.
pub async fn process_monitored_downloads(
    downloads: &Arc<Downloads>,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<ImportEvent>> {
    let db = downloads.db().clone();
    let env = db.call(move |conn| Environment::load(conn, now)).await?;
    let enabled = env.config.enable_completed_download_handling;
    let mut events: Vec<ImportEvent> = Vec::new();
    let tracked: Vec<TrackedDownload> = downloads
        .tracked_downloads()
        .into_iter()
        .filter(|t| t.is_trackable)
        .collect();
    for mut t in tracked {
        if enabled && t.state == TrackedDownloadState::ImportPending {
            let (db2, env2) = (db.clone(), env.clone());
            let (updated, ev) = tokio::task::spawn_blocking(move || {
                let mut t = t;
                let ev = import(&db2, &env2, &mut t, now);
                (t, ev)
            })
            .await?;
            t = updated;
            for e in &ev.0 {
                if matches!(e, ImportEvent::DownloadCompleted(_)) {
                    download_completed(downloads, &mut t, now).await;
                }
            }
            events.extend(ev.0);
            downloads.update_tracked(t.clone());
        }
        if t.state == TrackedDownloadState::FailedPending {
            if let Err(e) = failed::process_failed(downloads, &mut t, now).await {
                tracing::debug!(error = %e, "Failed to process download: {}", t.item.title);
            }
            downloads.update_tracked(t.clone());
        }
    }
    // `RemoveCompletedDownloads`: imported downloads are no longer
    // trackable, so they are processed after the trackable ones.
    for mut t in downloads.tracked_downloads() {
        if !t.item.removed && t.item.can_be_removed && t.state == TrackedDownloadState::Imported {
            download_can_be_removed(downloads, &mut t, now).await;
            downloads.update_tracked(t);
        }
    }
    downloads.rebuild_queue_now(now);
    Ok(events)
}
