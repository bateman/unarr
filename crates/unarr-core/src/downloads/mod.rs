//! Downloads from grab to queue, after the originals' `DownloadService`,
//! `TrackedDownloadService`, `DownloadMonitoringService`, `QueueService`,
//! `FailedDownloadService` and `IgnoredDownloadService`.
//!
//! One [`Downloads`] lives for the process: it hands releases to clients,
//! asks every client what it holds, matches each item back to a series and
//! episodes, and keeps the queue the API serves. The originals refresh on
//! a debounced event after a grab and once a minute from the scheduler;
//! here a grab or a removal marks the queue dirty and the next reader
//! refreshes it, and the scheduler (next step) refreshes it on its own.

pub mod failed;
pub mod grab;
pub mod monitor;
pub mod pending;
pub mod process;
pub mod queue;
pub mod redownload;
pub mod tracked;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::commands::CommandQueue;
use crate::db::Db;
use crate::decision::{QueueSource, QueuedRelease};
use crate::downloadclients::{Client, DownloadClientService};

pub use failed::DownloadFailed;
pub use grab::{GrabError, Grabbed};
pub use pending::PendingReleaseReason;
pub use process::{process_decisions, Processed, ProcessedResult};
pub use queue::{QueueItem, QueueStatus};
pub use tracked::{StatusMessage, TrackedDownload, TrackedDownloadState, TrackedDownloadStatus};

pub struct Downloads {
    db: Db,
    clients: DownloadClientService,
    /// `TrackedDownloadService`'s cache, by download id.
    tracked: Mutex<HashMap<String, TrackedDownload>>,
    /// `QueueService._queue`.
    queue: Mutex<Vec<QueueItem>>,
    /// The queue as the decision engine reads it, rebuilt with it.
    published: Mutex<Vec<QueuedRelease>>,
    /// Something changed since the queue was last built from the clients.
    dirty: AtomicBool,
    last_refresh: Mutex<Option<DateTime<Utc>>>,
    /// One refresh at a time: two would each record the same failure.
    refresh_lock: tokio::sync::Mutex<()>,
    /// The command queue, once the runtime is up, for the searches a
    /// failed download triggers.
    commands: OnceLock<Arc<CommandQueue>>,
}

/// What `DELETE /queue/{id}` was asked to do.
#[derive(Debug, Clone, Copy, Default)]
pub struct RemoveOptions {
    pub remove_from_client: bool,
    pub blocklist: bool,
    pub skip_redownload: bool,
    pub change_category: bool,
}

#[derive(Debug)]
pub enum RemoveError {
    /// No queue item or tracked download by that id.
    NotFound,
    /// The download client is gone (`BadRequestException`).
    ClientGone,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RemoveError {
    fn from(e: anyhow::Error) -> Self {
        RemoveError::Other(e)
    }
}

impl Downloads {
    pub fn new(db: Db, clients: DownloadClientService) -> Downloads {
        Downloads {
            db,
            clients,
            tracked: Mutex::new(HashMap::new()),
            queue: Mutex::new(Vec::new()),
            published: Mutex::new(Vec::new()),
            dirty: AtomicBool::new(true),
            last_refresh: Mutex::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
            commands: OnceLock::new(),
        }
    }

    /// Hand over the command queue; the first call wins.
    pub fn set_commands(&self, queue: Arc<CommandQueue>) {
        let _ = self.commands.set(queue);
    }

    pub fn commands(&self) -> Option<&Arc<CommandQueue>> {
        self.commands.get()
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn clients(&self) -> &DownloadClientService {
        &self.clients
    }

    /// Ask for a refresh before the queue is next read.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    pub fn last_refresh(&self) -> Option<DateTime<Utc>> {
        *self.last_refresh.lock().unwrap()
    }

    /// `RefreshMonitoredDownloads`: every enabled client asked, every item
    /// tracked, the queue rebuilt. Returns whether the queue changed.
    pub async fn refresh(&self, now: DateTime<Utc>) -> anyhow::Result<bool> {
        let _one_at_a_time = self.refresh_lock.lock().await;
        self.run_refresh(now).await
    }

    /// Refresh when something changed since the last time.
    pub async fn refresh_if_dirty(&self, now: DateTime<Utc>) -> anyhow::Result<bool> {
        if !self.is_dirty() {
            return Ok(false);
        }
        let _one_at_a_time = self.refresh_lock.lock().await;
        // The refresh that held the lock may have been the one asked for.
        if !self.is_dirty() {
            return Ok(false);
        }
        self.run_refresh(now).await
    }

    /// The refresh itself, under the lock. The flag is cleared before the
    /// clients are asked, so a grab during the refresh marks the queue
    /// dirty again instead of being lost; a failed refresh leaves it dirty.
    async fn run_refresh(&self, now: DateTime<Utc>) -> anyhow::Result<bool> {
        self.dirty.store(false, Ordering::SeqCst);
        let result = monitor::refresh(self, now).await;
        if result.is_err() {
            self.dirty.store(true, Ordering::SeqCst);
        }
        result
    }

    /// The queue for the decision engine's `QueueSpecification`.
    pub fn queued_releases(&self) -> Vec<QueuedRelease> {
        self.published.lock().unwrap().clone()
    }

    /// A handle the searches read the queue through.
    pub fn queue_source(self: &Arc<Self>) -> QueueSource {
        let downloads = self.clone();
        Arc::new(move || downloads.queued_releases())
    }

    /// The queue as last built.
    pub fn queue(&self) -> Vec<QueueItem> {
        self.queue.lock().unwrap().clone()
    }

    pub fn find_queue_item(&self, id: i64) -> Option<QueueItem> {
        self.queue.lock().unwrap().iter().find(|q| q.id == id).cloned()
    }

    pub fn tracked(&self, download_id: &str) -> Option<TrackedDownload> {
        self.tracked.lock().unwrap().get(download_id).cloned()
    }

    pub fn tracked_downloads(&self) -> Vec<TrackedDownload> {
        self.tracked.lock().unwrap().values().cloned().collect()
    }

    /// `StopTracking`: forget downloads and rebuild the queue from the rest.
    pub fn stop_tracking(&self, download_ids: &[String], now: DateTime<Utc>) {
        {
            let mut tracked = self.tracked.lock().unwrap();
            for id in download_ids {
                tracked.remove(id);
            }
        }
        self.rebuild_queue(now);
    }

    /// `QueueService.Handle(TrackedDownloadRefreshedEvent)`.
    fn rebuild_queue(&self, now: DateTime<Utc>) {
        let items = {
            let tracked = self.tracked.lock().unwrap();
            let mut all: Vec<&TrackedDownload> = tracked.values().collect();
            all.sort_by_key(|t| t.item.download_id.clone());
            queue::build(&all, now)
        };
        *self.published.lock().unwrap() = queue::queued_releases(&items);
        *self.queue.lock().unwrap() = items;
    }

    /// `DownloadService.DownloadReport`: hand a release to a client and
    /// record the grab.
    pub async fn download_report(
        &self,
        remote: &crate::decision::RemoteEpisode,
        download_client_id: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<Grabbed, GrabError> {
        grab::download_report(self, remote, download_client_id, now).await
    }

    /// `QueueController.RemoveAction` for a tracked download: `Ok(true)`
    /// when the item is gone from the queue.
    pub async fn remove_queue_item(
        &self,
        id: i64,
        options: RemoveOptions,
        now: DateTime<Utc>,
    ) -> Result<bool, RemoveError> {
        let item = self.find_queue_item(id).ok_or(RemoveError::NotFound)?;
        let download_id = item.download_id.as_deref().ok_or(RemoveError::NotFound)?;
        let tracked = self.tracked(download_id).ok_or(RemoveError::NotFound)?;
        self.remove_tracked(&tracked, options, now).await?;
        self.stop_tracking(std::slice::from_ref(&tracked.item.download_id), now);
        Ok(true)
    }

    /// `QueueController.RemoveMany`: ids that name nothing are skipped.
    pub async fn remove_queue_items(
        &self,
        ids: &[i64],
        options: RemoveOptions,
        now: DateTime<Utc>,
    ) -> Result<(), RemoveError> {
        let mut to_remove: Vec<TrackedDownload> = Vec::new();
        for id in ids {
            let Some(item) = self.find_queue_item(*id) else {
                continue;
            };
            if let Some(tracked) = item.download_id.as_deref().and_then(|d| self.tracked(d)) {
                if !to_remove
                    .iter()
                    .any(|t| t.item.download_id == tracked.item.download_id)
                {
                    to_remove.push(tracked);
                }
            }
        }
        let mut removed = Vec::new();
        let mut outcome = Ok(());
        for tracked in &to_remove {
            if let Err(e) = self.remove_tracked(tracked, options, now).await {
                outcome = Err(e);
                break;
            }
            removed.push(tracked.item.download_id.clone());
        }
        // What was removed stops being tracked even when a later one failed,
        // else a retry would record its failure and blocklist rows again.
        self.stop_tracking(&removed, now);
        outcome
    }

    /// `QueueController.Remove(trackedDownload, ...)`.
    async fn remove_tracked(
        &self,
        tracked: &TrackedDownload,
        options: RemoveOptions,
        now: DateTime<Utc>,
    ) -> Result<(), RemoveError> {
        if options.remove_from_client || options.change_category {
            let definition = self
                .clients
                .find(tracked.download_client)
                .await?
                .filter(|d| d.enable)
                .ok_or(RemoveError::ClientGone)?;
            let context = self.clients.context(now).await?;
            let client = Client::new(&definition, &context);
            if options.remove_from_client {
                client
                    .remove(&tracked.item, true)
                    .await
                    .map_err(|e| RemoveError::Other(anyhow::anyhow!("{e}")))?;
            } else if let Err(e) = client.mark_imported(&tracked.item).await {
                tracing::warn!(error = %e, "failed to change the category of the download");
            }
        }
        if options.blocklist {
            failed::mark_as_failed(self, tracked, options.skip_redownload, now).await?;
        }
        if !options.remove_from_client && !options.blocklist && !options.change_category {
            failed::ignore_download(self, tracked, now).await?;
        }
        Ok(())
    }
}
