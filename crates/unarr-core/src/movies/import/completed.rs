//! `CompletedDownloadService.Import` and `VerifyImport`, with
//! `DownloadProcessingService`: a download the refresh left `ImportPending`
//! is imported from the path its client reports, marked imported and, when
//! the client's settings say so, removed from the client.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::aggregate;
use super::decide::MovieEnvironment;
use super::downloaded;
use super::events::{
    self, MovieDownloadCompleted, MovieEvents, MovieImportEvent, MovieManualInteractionRequired,
};
use super::local::{MovieImportResult, MovieRejectionReason};
use crate::db::Db;
use crate::downloadclients::{Client, DownloadItemStatus};
use crate::downloads::{StatusMessage, TrackedDownloadState, TrackedDownloadStatus};
use crate::indexers::settings::NewznabSettings;
use crate::mediafiles::file_name;
use crate::mediafiles::local::{ImportMode, ImportResultType};
use crate::model::ProviderKind;
use crate::movies::downloads::{MovieDownloads, TrackedMovieDownload};
use crate::movies::model::{Movie, MovieHistoryEntry, MovieHistoryEvent};
use crate::movies::store;

/// `FailDownloads` values of an indexer's settings.
const FAIL_EXECUTABLES: i64 = 0;
const FAIL_POTENTIALLY_DANGEROUS: i64 = 1;

/// `Warn(params TrackedDownloadStatusMessage[])`.
fn warn_messages(t: &mut TrackedMovieDownload, messages: Vec<StatusMessage>) {
    t.status = TrackedDownloadStatus::Warning;
    t.status_messages = messages;
}

/// `ValidatePath`.
fn validate_path(t: &mut TrackedMovieDownload) -> bool {
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
            "[{output}] is not a valid local path. You may need a Remote Path Mapping."
        ));
        return false;
    }
    true
}

/// The grabs of a download.
pub(super) fn grabbed_history(db: &Db, download_id: &str) -> Vec<MovieHistoryEntry> {
    let download_id = download_id.to_string();
    db.call_blocking(move |conn| {
        Ok(store::history_by_download_id(conn, &download_id)?
            .into_iter()
            .filter(|h| h.event_type == MovieHistoryEvent::Grabbed)
            .collect::<Vec<_>>())
    })
    .unwrap_or_default()
}

/// `SetStateToImportBlocked`: blocked, and whoever listens told once.
fn set_import_blocked(db: &Db, t: &mut TrackedMovieDownload, events: &mut MovieEvents) {
    t.state = TrackedDownloadState::ImportBlocked;
    if t.has_notified_manual_interaction_required {
        return;
    }
    t.has_notified_manual_interaction_required = true;
    let grabbed = grabbed_history(db, t.download_id());
    let release = (!grabbed.is_empty()).then(|| aggregate::grabbed_release(&grabbed));
    events.push(MovieImportEvent::ManualInteractionRequired(Box::new(
        MovieManualInteractionRequired {
            tracked: t.clone(),
            release,
        },
    )));
}

/// `RejectedImportService.Process`: a single rejected result becomes the
/// download's warning, or its failure when the indexer says a dangerous or
/// executable file fails the download.
fn process_rejected(db: &Db, t: &mut TrackedMovieDownload, result: &MovieImportResult) -> bool {
    let Some(release) = t.remote.as_ref().map(|r| &r.release) else {
        return false;
    };
    if result.result() != ImportResultType::Rejected {
        return false;
    }
    let indexer_id = release.indexer_id;
    let settings = db
        .call_blocking(move |conn| {
            crate::store::providers::get_provider(conn, ProviderKind::Indexer, indexer_id)
        })
        .ok()
        .flatten()
        .map(|p| NewznabSettings::from_value(&p.settings));
    let reason = result.decision.rejections.first().map(|r| r.reason);
    let title = t.item.title.clone();
    let warning = vec![StatusMessage {
        title: title.clone(),
        messages: result.errors.clone(),
    }];
    let Some(settings) = settings else {
        warn_messages(t, warning);
        return true;
    };
    match reason {
        Some(MovieRejectionReason::DangerousFile)
            if settings.fail_downloads.contains(&FAIL_POTENTIALLY_DANGEROUS) =>
        {
            tracing::trace!("Download '{title}' contains potentially dangerous file, marking as failed");
            t.status = TrackedDownloadStatus::Error;
        }
        Some(MovieRejectionReason::ExecutableFile) if settings.fail_downloads.contains(&FAIL_EXECUTABLES) => {
            tracing::trace!("Download '{title}' contains executable file, marking as failed");
            t.status = TrackedDownloadStatus::Error;
        }
        _ => warn_messages(t, warning),
    }
    true
}

/// `VerifyImport`: the download is done when a file of it was imported
/// now, or when the history says its movie was imported from it before.
pub fn verify_import(
    db: &Db,
    t: &mut TrackedMovieDownload,
    results: &[MovieImportResult],
    now: DateTime<Utc>,
    events: &mut MovieEvents,
) -> bool {
    let Some(movie_id) = t.remote.as_ref().and_then(|r| r.movie.as_ref()).map(|m| m.id) else {
        return false;
    };
    let complete = |t: &mut TrackedMovieDownload, events: &mut MovieEvents| {
        t.state = TrackedDownloadState::Imported;
        let grabbed = grabbed_history(db, t.download_id());
        let ev = MovieDownloadCompleted {
            tracked: t.clone(),
            movie_id,
            release: (!grabbed.is_empty()).then(|| aggregate::grabbed_release(&grabbed)),
        };
        let ev2 = ev.clone();
        if let Err(e) = db.call_blocking(move |conn| events::record_download_completed(conn, &ev2, now)) {
            tracing::error!(error = %e, "failed to record the completed download");
        }
        events.push(MovieImportEvent::DownloadCompleted(Box::new(ev)));
    };
    let at_least_one = results.iter().any(|r| {
        r.result() == ImportResultType::Imported
            && r.decision.local_movie.as_ref().is_some_and(|l| l.movie.is_some())
    });
    if at_least_one {
        tracing::debug!("All movies were imported for {}", t.item.title);
        complete(t, events);
        return true;
    }
    // `TrackedDownloadAlreadyImported.IsImported`: the newest history row
    // of the download's movie is an import.
    let download_id = t.download_id().to_string();
    let mut history = db
        .call_blocking(move |conn| store::history_by_download_id(conn, &download_id))
        .unwrap_or_default();
    history.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
    let imported_in_history = history
        .iter()
        .find(|h| h.movie_id == movie_id)
        .is_some_and(|h| h.event_type == MovieHistoryEvent::DownloadFolderImported);
    if imported_in_history {
        tracing::debug!(
            "No Movies were just imported, but all movies were previously imported, possible issue with download history."
        );
        complete(t, events);
        return true;
    }
    tracing::debug!("Not all movies have been imported for {}", t.item.title);
    false
}

/// The movie as the database has it now. A tracked download keeps the
/// movie it was matched to and, once past downloading, is handed back
/// unchanged at every refresh: by the time it is imported another download
/// of the same movie may have put a file in, and the copy still says the
/// movie has none, which skips the upgrade and already-imported rules and
/// lets a worse file replace a better one, left on disk without a row.
/// The original reads its copy the same way; this is a deliberate
/// departure, visible only in that race.
pub fn current_movie(db: &Db, tracked: Movie) -> Movie {
    let id = tracked.id;
    match db.call_blocking(move |conn| store::get_movie(conn, id)) {
        Ok(Some(movie)) => movie,
        Ok(None) => tracked,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "could not read the movie again before importing");
            tracked
        }
    }
}

/// `Import(trackedDownload)`. File work: call it off the async runtime.
pub fn import(
    db: &Db,
    env: &MovieEnvironment,
    t: &mut TrackedMovieDownload,
    now: DateTime<Utc>,
) -> MovieEvents {
    let mut events = MovieEvents::default();
    if t.import_item.is_none() {
        t.import_item = Some(t.item.clone());
    }
    if !validate_path(t) {
        return events;
    }
    let Some(movie) = t.remote.as_ref().and_then(|r| r.movie.clone()) else {
        t.warn("Unable to parse download, automatic import is not possible.");
        set_import_blocked(db, t, &mut events);
        return events;
    };
    let movie = current_movie(db, movie);
    t.state = TrackedDownloadState::Importing;
    let import_item = t.import_item.clone().expect("set above");
    let output_path = import_item.output_path.clone().unwrap_or_default();
    // The flags of the release the download is tracked with, as the
    // decision maker reads them from the tracked download.
    let tracked_flags = t.remote.as_ref().map(|r| r.release.indexer_flags);
    let imported = downloaded::process_path(
        db,
        env,
        &output_path,
        ImportMode::Auto,
        Some(&movie),
        Some(&import_item),
        tracked_flags,
        now,
    );
    events.extend(imported.events);
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
        title: "One or more movies expected in this release were not imported or missing".into(),
        messages: Vec::new(),
    }];
    let mut not_imported: Vec<&MovieImportResult> = results
        .iter()
        .filter(|r| r.result() != ImportResultType::Imported && r.decision.local_movie.is_some())
        .collect();
    not_imported.sort_by(|a, b| {
        let pa = &a.decision.local_movie.as_ref().unwrap().path;
        let pb = &b.decision.local_movie.as_ref().unwrap().path;
        pa.cmp(pb)
    });
    for r in not_imported {
        let path = &r.decision.local_movie.as_ref().unwrap().path;
        messages.push(StatusMessage {
            title: file_name(path).to_string(),
            messages: r.errors.clone(),
        });
    }
    warn_messages(t, messages);
    set_import_blocked(db, t, &mut events);
    events
}

/// `DownloadEventHub.Handle(DownloadCompletedEvent)`: the client told the
/// item was imported, and the item removed when its client is set to.
pub async fn download_completed(
    downloads: &MovieDownloads,
    t: &mut TrackedMovieDownload,
    now: DateTime<Utc>,
) {
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
async fn download_can_be_removed(
    downloads: &MovieDownloads,
    t: &mut TrackedMovieDownload,
    now: DateTime<Utc>,
) {
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

async fn remove_from_client(client: &Client<'_>, t: &mut TrackedMovieDownload) {
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
/// the pending imports, then the imported downloads the clients may drop.
/// The failed ones are dealt with as the refresh finds them. Returns the
/// events for the relays.
pub async fn process_monitored_downloads(
    downloads: &Arc<MovieDownloads>,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieImportEvent>> {
    let db = downloads.db().clone();
    let env = db.call(move |conn| MovieEnvironment::load(conn, now)).await?;
    let enabled = env.config.enable_completed_download_handling;
    let mut events: Vec<MovieImportEvent> = Vec::new();
    let mut tracked: Vec<TrackedMovieDownload> = downloads
        .tracked_downloads()
        .into_iter()
        .filter(|t| t.is_trackable)
        .collect();
    tracked.sort_by(|a, b| a.item.download_id.cmp(&b.item.download_id));
    for t in tracked {
        if !(enabled && t.state == TrackedDownloadState::ImportPending) {
            continue;
        }
        let (db2, env2) = (db.clone(), env.clone());
        let (mut t, ev) = tokio::task::spawn_blocking(move || {
            let mut t = t;
            let ev = import(&db2, &env2, &mut t, now);
            (t, ev)
        })
        .await?;
        if ev
            .0
            .iter()
            .any(|e| matches!(e, MovieImportEvent::DownloadCompleted(_)))
        {
            download_completed(downloads, &mut t, now).await;
        }
        events.extend(ev.0);
        downloads.update_tracked(t);
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
