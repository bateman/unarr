//! Movie downloads from grab to queue, after Radarr's `DownloadService`,
//! `TrackedDownloadService`, `DownloadMonitoringService`, `QueueService`,
//! `FailedDownloadService` and `IgnoredDownloadService`: what
//! `crate::downloads` is to Sonarr. The two sit side by side; the pieces
//! with no media notion in them (the states, the queue's ids and order,
//! what a removal can ask for, how a grab can fail) are the series side's.

pub mod failed;
pub mod grab;
pub mod monitor;
pub mod pending;
pub mod process;
pub mod queue;
pub mod redownload;
pub mod tracked;

use crate::sync::Recover;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};

use crate::commands::CommandQueue;
use crate::db::Db;
use crate::downloadclients::{Client, DownloadClientService};
use crate::downloads::{GrabError, Grabbed, RemoveError, RemoveOptions};
use crate::movies::decision::{QueuedMovie, RemoteMovie};
use crate::movies::search::MovieQueueSource;

pub use failed::MovieDownloadFailed;
pub use process::{process_decision, process_decisions, ProcessedMovies};
pub use queue::MovieQueueItem;
pub use tracked::TrackedMovieDownload;

pub struct MovieDownloads {
    db: Db,
    clients: DownloadClientService,
    /// `TrackedDownloadService`'s cache, by download id.
    tracked: Mutex<HashMap<String, TrackedMovieDownload>>,
    /// `QueueService._queue`.
    queue: Mutex<Vec<MovieQueueItem>>,
    /// Something changed since the queue was last built from the clients.
    dirty: AtomicBool,
    /// One refresh at a time: two would each record the same failure.
    refresh_lock: tokio::sync::Mutex<()>,
    /// The command queue, once the runtime is up, for the search a failed
    /// download triggers.
    commands: OnceLock<Arc<CommandQueue>>,
    /// What a refresh raised for whoever relays events
    /// (`ManualInteractionRequiredEvent`, published by the completed
    /// download check), until the runtime takes it.
    raised: Mutex<Vec<crate::movies::import::events::MovieImportEvent>>,
    /// The notifications, once the runtime is up: a grab tells them.
    notifier: OnceLock<Arc<crate::movies::notifications::MovieNotifier>>,
    /// Who hears of a grab besides the notifications: the runtime, which
    /// tells the UI (`MovieControllerWithSignalR.Handle(MovieGrabbedEvent)`).
    grab_listener: OnceLock<GrabListener>,
}

/// Called with the id of a movie whose release went to a client.
pub type GrabListener = Box<dyn Fn(i64) + Send + Sync>;

impl MovieDownloads {
    pub fn new(db: Db, clients: DownloadClientService) -> MovieDownloads {
        MovieDownloads {
            db,
            clients,
            tracked: Mutex::new(HashMap::new()),
            queue: Mutex::new(Vec::new()),
            dirty: AtomicBool::new(true),
            refresh_lock: tokio::sync::Mutex::new(()),
            commands: OnceLock::new(),
            raised: Mutex::new(Vec::new()),
            notifier: OnceLock::new(),
            grab_listener: OnceLock::new(),
        }
    }

    /// The events the refreshes raised since they were last taken.
    pub fn take_raised_events(&self) -> Vec<crate::movies::import::events::MovieImportEvent> {
        std::mem::take(&mut *self.raised.lock().recover())
    }

    /// Hand over the notifications; the first call wins.
    pub fn set_notifier(&self, notifier: Arc<crate::movies::notifications::MovieNotifier>) {
        let _ = self.notifier.set(notifier);
    }

    /// Hand over the grab listener; the first call wins.
    pub fn set_grab_listener(&self, listener: GrabListener) {
        let _ = self.grab_listener.set(listener);
    }

    pub(crate) fn grabbed(&self, movie_id: i64) {
        if let Some(listener) = self.grab_listener.get() {
            listener(movie_id);
        }
    }

    pub fn notifier(&self) -> Option<&Arc<crate::movies::notifications::MovieNotifier>> {
        self.notifier.get()
    }

    /// The covers of the movie a tracked download holds, given their local
    /// addresses: what building a webhook payload for that movie left
    /// behind in the original (`WebhookBase.GetMovie` writes them into the
    /// movie object it is handed, the one the tracked download keeps). The
    /// queue and the manual import of the download show them from then
    /// on, until the download is matched to its movie afresh.
    pub fn convert_covers_of(&self, download_id: &str, url_base: &str) {
        let mut tracked = self.tracked.lock().recover();
        if let Some(movie) = tracked
            .get_mut(download_id)
            .and_then(|t| t.remote.as_mut())
            .and_then(|r| r.movie.as_mut())
        {
            crate::movies::notifications::convert_to_local_urls(
                url_base,
                movie.id,
                &mut movie.metadata.images,
            );
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

    /// The queue as last built.
    pub fn queue(&self) -> Vec<MovieQueueItem> {
        self.queue.lock().recover().clone()
    }

    /// The queue for the decision engine's `QueueSpecification`.
    pub fn queued_movies(&self) -> Vec<QueuedMovie> {
        queue::queued_movies(&self.queue.lock().recover())
    }

    /// A handle the searches read the queue through.
    pub fn queue_source(self: &Arc<Self>) -> MovieQueueSource {
        let downloads = self.clone();
        Arc::new(move || downloads.queued_movies())
    }

    pub fn find_queue_item(&self, id: i64) -> Option<MovieQueueItem> {
        self.queue.lock().recover().iter().find(|q| q.id == id).cloned()
    }

    pub fn tracked(&self, download_id: &str) -> Option<TrackedMovieDownload> {
        self.tracked.lock().recover().get(download_id).cloned()
    }

    pub fn tracked_downloads(&self) -> Vec<TrackedMovieDownload> {
        self.tracked.lock().recover().values().cloned().collect()
    }

    /// `StopTracking`: forget downloads and rebuild the queue from the rest.
    pub fn stop_tracking(&self, download_ids: &[String], now: DateTime<Utc>) {
        {
            let mut tracked = self.tracked.lock().recover();
            for id in download_ids {
                tracked.remove(id);
            }
        }
        self.rebuild_queue(now);
    }

    /// Put back a tracked download the importer changed, if it is still
    /// tracked. One past importing leaves the queue (`DownloadIsTrackable`).
    pub fn update_tracked(&self, mut t: TrackedMovieDownload) {
        if matches!(
            t.state,
            crate::downloads::TrackedDownloadState::Imported
                | crate::downloads::TrackedDownloadState::Failed
                | crate::downloads::TrackedDownloadState::Ignored
        ) {
            t.is_trackable = false;
        }
        let mut tracked = self.tracked.lock().recover();
        if tracked.contains_key(&t.item.download_id) {
            tracked.insert(t.item.download_id.clone(), t);
        }
    }

    /// The queue built again from the tracked downloads as they stand.
    pub fn rebuild_queue_now(&self, now: DateTime<Utc>) {
        self.rebuild_queue(now);
    }

    /// `QueueService.Handle(TrackedDownloadRefreshedEvent)`.
    fn rebuild_queue(&self, now: DateTime<Utc>) {
        let items = {
            let tracked = self.tracked.lock().recover();
            let mut all: Vec<&TrackedMovieDownload> = tracked.values().collect();
            all.sort_by_key(|t| t.item.download_id.clone());
            queue::build(&all, now)
        };
        *self.queue.lock().recover() = items;
    }

    /// `DownloadService.DownloadReport`: hand a release to a client and
    /// record the grab.
    pub async fn download_report(
        &self,
        remote: &RemoteMovie,
        download_client_id: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<Grabbed, GrabError> {
        grab::download_report(self, remote, download_client_id, now).await
    }

    /// `QueueController.RemoveAction` for a tracked download.
    pub async fn remove_queue_item(
        &self,
        id: i64,
        options: RemoveOptions,
        now: DateTime<Utc>,
    ) -> Result<Option<MovieDownloadFailed>, RemoveError> {
        let item = self.find_queue_item(id).ok_or(RemoveError::NotFound)?;
        let download_id = item.download_id.as_deref().ok_or(RemoveError::NotFound)?;
        let tracked = self.tracked(download_id).ok_or(RemoveError::NotFound)?;
        let failed = self.remove_tracked(&tracked, options, now).await?;
        self.stop_tracking(std::slice::from_ref(&tracked.item.download_id), now);
        Ok(failed)
    }

    /// `QueueController.RemoveMany`: ids that name nothing are skipped.
    pub async fn remove_queue_items(
        &self,
        ids: &[i64],
        options: RemoveOptions,
        now: DateTime<Utc>,
    ) -> Result<Vec<MovieDownloadFailed>, RemoveError> {
        let mut to_remove: Vec<TrackedMovieDownload> = Vec::new();
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
        let mut failures = Vec::new();
        let mut outcome = Ok(());
        for tracked in &to_remove {
            match self.remove_tracked(tracked, options, now).await {
                Ok(failed) => failures.extend(failed),
                Err(e) => {
                    outcome = Err(e);
                    break;
                }
            }
            removed.push(tracked.item.download_id.clone());
        }
        // What was removed stops being tracked even when a later one failed,
        // else a retry would record its failure and blocklist rows again.
        self.stop_tracking(&removed, now);
        outcome.map(|()| failures)
    }

    /// `QueueController.Remove(trackedDownload, ...)`.
    async fn remove_tracked(
        &self,
        tracked: &TrackedMovieDownload,
        options: RemoveOptions,
        now: DateTime<Utc>,
    ) -> Result<Option<MovieDownloadFailed>, RemoveError> {
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
                    .map_err(|e| RemoveError::Other(e.into_anyhow()))?;
            } else if let Err(e) = client.mark_imported(&tracked.item).await {
                tracing::warn!(error = %e.exception(), "failed to change the category of the download");
            }
        }
        let mut failed = None;
        if options.blocklist {
            failed = failed::mark_as_failed(self, tracked, options.skip_redownload, now).await?;
        }
        if !options.remove_from_client && !options.blocklist && !options.change_category {
            failed::ignore_download(self, tracked, now).await?;
        }
        Ok(failed)
    }
}
