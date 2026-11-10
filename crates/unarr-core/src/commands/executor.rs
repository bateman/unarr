//! `CommandExecutor` and the handlers of the commands Unarr runs, plus the
//! scheduler's tick. A [`Runtime`] owns the queue and the services the
//! handlers need; `start` spawns the workers and the scheduler on the
//! current Tokio runtime.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

use super::scheduler;
use super::{CommandBody, CommandModel, CommandPriority, CommandQueue, CommandStatus, CommandTrigger};
use crate::db::Db;
use crate::decision::{self, Decision, DecisionInput};
use crate::downloads::pending;
use crate::downloads::process::{self, Processed};
use crate::downloads::Downloads;
use crate::health::{self, CheckSet, HealthCheck, ImportFailure};
use crate::indexers::IndexerService;
use crate::mediafiles::decide::Environment;
use crate::mediafiles::manual::{self, ManualImportFile};
use crate::mediafiles::{completed, downloaded, rename, scan, ImportEvent, ImportMode};
use crate::model::Episode;
use crate::notifications::{Identity, Notifier};
use crate::profile;
use crate::search::{SearchService, Trigger};
use crate::store;

/// The source of the current time: the daemon's clock, or a pinned one
/// in tests.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// `THREAD_LIMIT`: commands running at once.
const WORKERS: usize = 3;

/// What the runtime tells whoever listens (the API relays it to the UI).
#[derive(Debug, Clone)]
pub enum Event {
    /// `CommandUpdatedEvent`, for commands that send updates to clients.
    CommandUpdated(Box<CommandModel>),
    /// `QueueUpdatedEvent` / `PendingReleasesUpdatedEvent`.
    QueueUpdated,
    /// `HealthCheckCompleteEvent`, when a verdict changed.
    HealthChanged,
    /// What the importer raised: `EpisodeImportedEvent`,
    /// `EpisodeFileDeletedEvent`, `DownloadCompletedEvent` and the rest.
    Import(Box<ImportEvent>),
    /// `SeriesUpdatedEvent` and `MediaCoversUpdatedEvent` with a change:
    /// the series resource the UI holds is stale.
    SeriesUpdated { series_id: i64 },
}

/// Health runs the originals' debouncer holds for five seconds after a
/// change (and after the start).
const HEALTH_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

pub struct Runtime {
    db: Db,
    queue: Arc<CommandQueue>,
    downloads: Arc<Downloads>,
    search: SearchService,
    indexers: IndexerService,
    clock: Clock,
    events: broadcast::Sender<Event>,
    /// `HealthCheckService`'s results: the verdicts that stand.
    health: Mutex<Vec<HealthCheck>>,
    /// Health runs in flight, so a test can wait for them.
    health_runs: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// `_pendingHealthChecks`: the sets a debounced run will cover.
    health_pending: Mutex<Vec<(CheckSet, Option<ImportFailure>)>>,
    /// A handle on this runtime, for the background runs it spawns.
    me: Mutex<std::sync::Weak<Runtime>>,
    /// The provider statuses the last status-driven run saw, so a refresh
    /// that changed nothing does not ask the clients again.
    health_statuses: Mutex<Option<String>>,
    is_docker: bool,
    /// Set at shutdown: the workers take no further command.
    stopping: std::sync::atomic::AtomicBool,
    notifier: Arc<Notifier>,
    /// When the runtime came up, for the health checks' startup grace.
    started_at: DateTime<Utc>,
    /// `_hasRunHealthChecksAfterGracePeriod`: health verdicts reach the
    /// notifications only once the checks ran after the grace period.
    health_armed: std::sync::atomic::AtomicBool,
    /// Where series metadata come from: Skyhook unless replaced.
    metadata: std::sync::RwLock<crate::metadata::SharedProvider>,
    /// Scene mappings, XEM and the cover downloads, through the same
    /// upstreams as the provider.
    augment: std::sync::RwLock<crate::metadata::augment::Augment>,
    scene: crate::tv::scene::SceneState,
    /// `MediaCover/` under the application folder, once the host says
    /// where that is; no covers are fetched before.
    media_cover_dir: std::sync::RwLock<Option<std::path::PathBuf>>,
    /// What the Backup command needs; without it the command does nothing.
    backup: std::sync::RwLock<Option<crate::backup::BackupContext>>,
    /// `EpisodeRefreshedService._searchCache`: recently aired episodes a
    /// refresh added, searched for once the series is scanned.
    refreshed_search: Mutex<std::collections::HashMap<i64, Vec<i64>>>,
}

/// `HealthCheckService`'s startup grace: fifteen minutes.
const HEALTH_GRACE: chrono::Duration = chrono::Duration::minutes(15);

impl Runtime {
    /// Open the queue over `db`, bring the scheduled tasks in line with
    /// the defaults, and hand the queue to the downloads for the searches
    /// a failed download triggers.
    pub async fn new(
        db: Db,
        downloads: Arc<Downloads>,
        clock: Clock,
        identity: Identity,
    ) -> anyhow::Result<Arc<Runtime>> {
        crate::time::process_start();
        let queue = Arc::new(CommandQueue::open(db.clone()).await?);
        let now = clock();
        db.call(move |conn| scheduler::ensure_defaults(conn, now)).await?;
        downloads.set_commands(queue.clone());
        let notifier = Arc::new(Notifier::new(db.clone(), identity, now));
        downloads.set_notifier(notifier.clone());
        let (events, _) = broadcast::channel(256);
        let runtime = Arc::new(Runtime {
            search: SearchService::new(db.clone()).with_queue(downloads.queue_source()),
            indexers: IndexerService::new(db.clone()),
            db,
            queue,
            downloads,
            clock,
            events,
            health: Mutex::new(Vec::new()),
            health_runs: Mutex::new(Vec::new()),
            health_pending: Mutex::new(Vec::new()),
            me: Mutex::new(std::sync::Weak::new()),
            health_statuses: Mutex::new(None),
            is_docker: std::path::Path::new("/.dockerenv").exists(),
            stopping: std::sync::atomic::AtomicBool::new(false),
            notifier,
            started_at: now,
            health_armed: std::sync::atomic::AtomicBool::new(false),
            metadata: std::sync::RwLock::new(Arc::new(crate::metadata::Skyhook::new(
                crate::metadata::Upstreams::real(),
            ))),
            augment: std::sync::RwLock::new(crate::metadata::augment::Augment::new(
                crate::http::shared().clone(),
                crate::metadata::Upstreams::real(),
            )),
            scene: crate::tv::scene::SceneState::default(),
            media_cover_dir: std::sync::RwLock::new(None),
            backup: std::sync::RwLock::new(None),
            refreshed_search: Mutex::new(std::collections::HashMap::new()),
        });
        *runtime.me.lock().unwrap() = Arc::downgrade(&runtime);
        Ok(runtime)
    }

    /// The metadata provider series are looked up, added and refreshed
    /// from.
    pub fn metadata(&self) -> crate::metadata::SharedProvider {
        self.metadata.read().unwrap().clone()
    }

    /// Replace the metadata provider.
    pub fn set_metadata(&self, provider: crate::metadata::SharedProvider) {
        *self.metadata.write().unwrap() = provider;
    }

    /// Ask every metadata host through `upstreams`: Skyhook as the
    /// provider, the services, XEM and the artwork. Tests and the replay
    /// point them at a mock.
    pub fn set_upstreams(&self, upstreams: crate::metadata::Upstreams) {
        self.set_metadata(Arc::new(crate::metadata::Skyhook::new(upstreams.clone())));
        *self.augment.write().unwrap() =
            crate::metadata::augment::Augment::new(crate::http::shared().clone(), upstreams);
    }

    /// Where the series covers live (`<app data>/MediaCover`).
    pub fn set_media_cover_dir(&self, dir: std::path::PathBuf) {
        *self.media_cover_dir.write().unwrap() = Some(dir);
    }

    /// Where the Backup command finds the app data, and Unarr's version.
    pub fn set_backup(&self, context: crate::backup::BackupContext) {
        *self.backup.write().unwrap() = Some(context);
    }

    /// The client the covers and the cover proxy fetch through.
    pub fn augment_client(&self) -> crate::metadata::augment::Augment {
        self.augment()
    }

    /// The import lists, asked through the same upstreams as the metadata.
    pub fn import_lists(&self) -> crate::importlists::ImportLists {
        crate::importlists::ImportLists::new(
            self.db.clone(),
            crate::http::shared().clone(),
            self.augment().upstreams().clone(),
        )
    }

    /// `ProviderDeletedEvent<IImportList>`: the library is checked against
    /// the lists that remain.
    pub async fn import_list_deleted(&self) {
        match self.import_lists().try_clean_library().await {
            Ok(updated) => {
                for series_id in updated {
                    self.emit(Event::SeriesUpdated { series_id });
                }
            }
            Err(e) => tracing::warn!("Cleaning the library after a deleted import list failed: {e:#}"),
        }
    }

    fn augment(&self) -> crate::metadata::augment::Augment {
        self.augment.read().unwrap().clone()
    }

    pub fn notifier(&self) -> &Arc<Notifier> {
        &self.notifier
    }

    /// The health verdicts that stand.
    pub fn health(&self) -> Vec<HealthCheck> {
        self.health.lock().unwrap().clone()
    }

    /// Run a set of health checks now and keep what they found; `true`
    /// when a verdict changed.
    pub async fn check_health(&self, set: CheckSet) -> bool {
        self.check_health_sets(&[(set, None)]).await
    }

    /// `ProcessHealthChecks`: the pending sets in one run, then
    /// `HealthCheckCompleteEvent`.
    async fn check_health_sets(&self, sets: &[(CheckSet, Option<ImportFailure>)]) -> bool {
        let now = self.now();
        let clients = self.downloads.clients();
        let failure = sets.iter().find_map(|(_, f)| f.clone());
        let ctx = health::Context {
            db: &self.db,
            indexers: &self.indexers,
            clients,
            now,
            is_docker: self.is_docker,
            import_failure: failure.as_ref(),
        };
        let standing = self.health();
        let names: Vec<CheckSet> = sets.iter().map(|(s, _)| *s).collect();
        let run = health::run(&ctx, &names, &standing).await;
        let (changed, failed, restored) = {
            let mut results = self.health.lock().unwrap();
            let before = results.clone();
            let changed = health::merge(&mut results, run.clone());
            // `HealthCheckFailedEvent` for a source that had no verdict,
            // `HealthCheckRestoredEvent` for one whose verdict cleared.
            let failed: Vec<HealthCheck> = run
                .iter()
                .filter(|r| !r.is_ok() && !before.iter().any(|b| b.source == r.source))
                .cloned()
                .collect();
            let restored: Vec<HealthCheck> = run
                .iter()
                .filter(|r| r.is_ok())
                .filter_map(|r| before.iter().find(|b| b.source == r.source).cloned())
                .collect();
            (changed, failed, restored)
        };
        if changed {
            self.emit(Event::HealthChanged);
        }
        if self.health_armed.load(std::sync::atomic::Ordering::SeqCst) {
            for check in &failed {
                self.notifier.on_health_issue(check, now).await;
            }
            for check in &restored {
                self.notifier.on_health_restored(check, now).await;
            }
        }
        // `HealthCheckCompleteEvent`.
        self.notifier.process_queue(now).await;
        changed
    }

    /// `HealthCheckService.HandleAsync(IEvent)`: once the startup grace
    /// period is over, the checks run again and every failure that stands
    /// reaches the notifications; from then on new verdicts do too.
    async fn arm_health_notifications(&self) {
        use std::sync::atomic::Ordering;
        if self.health_armed.load(Ordering::SeqCst) || self.now() < self.started_at + HEALTH_GRACE {
            return;
        }
        self.check_health(CheckSet::All).await;
        self.health_armed.store(true, Ordering::SeqCst);
        let now = self.now();
        for check in self.health() {
            self.notifier.on_health_issue(&check, now).await;
        }
    }

    /// Run a set of health checks in the background after the originals'
    /// debounce, as a provider change or the start asks for.
    pub fn check_health_later(self: &Arc<Self>, set: CheckSet) {
        self.check_health_later_with(set, None);
    }

    /// `HealthCheckService.HandleAsync(IEvent)` through the debouncer: the
    /// set joins what is pending; one run, five seconds after the first
    /// request, covers them all.
    fn check_health_later_with(&self, set: CheckSet, failure: Option<ImportFailure>) {
        let first = {
            let mut pending = self.health_pending.lock().unwrap();
            pending.push((set, failure));
            pending.len() == 1
        };
        if !first {
            return;
        }
        let Some(runtime) = self.me.lock().unwrap().upgrade() else {
            return;
        };
        let handle = tokio::spawn(async move {
            tokio::time::sleep(HEALTH_DELAY).await;
            let sets: Vec<(CheckSet, Option<ImportFailure>)> =
                std::mem::take(&mut *runtime.health_pending.lock().unwrap());
            runtime.check_health_sets(&sets).await;
        });
        let mut runs = self.health_runs.lock().unwrap();
        runs.retain(|h| !h.is_finished());
        runs.push(handle);
    }

    /// The event-driven checks an import's outcome asks for (`CheckOn` on
    /// `EpisodeImportedEvent` and `EpisodeImportFailedEvent`).
    fn check_health_after_import(&self, events: &[ImportEvent]) {
        let failure = events.iter().find_map(|e| match e {
            ImportEvent::EpisodeImportFailed(f) => Some(ImportFailure {
                path: Some(f.local.path.clone()).filter(|p| !p.is_empty()),
                download_client: f.client.as_ref().map(|c| c.name.clone()),
                download_id: f.download_id.clone(),
                output_path: f
                    .download_id
                    .as_deref()
                    .and_then(|id| self.downloads.tracked(id))
                    .and_then(|t| {
                        t.import_item
                            .as_ref()
                            .or(Some(&t.item))
                            .and_then(|i| i.output_path.clone())
                    }),
            }),
            _ => None,
        });
        if let Some(f) = failure {
            self.check_health_later_with(CheckSet::ImportFailed, Some(f));
        }
        if events
            .iter()
            .any(|e| matches!(e, ImportEvent::EpisodeImported(_)))
        {
            self.check_health_later_with(CheckSet::ImportSucceeded, None);
        }
    }

    /// The status-driven checks, when a provider's status changed since the
    /// last look (`ProviderStatusChangedEvent`).
    async fn check_health_on_status_change(&self) {
        let snapshot = self
            .db
            .call(|conn| {
                let mut text = String::new();
                for kind in [
                    crate::model::ProviderKind::Indexer,
                    crate::model::ProviderKind::DownloadClient,
                ] {
                    for (id, s) in store::provider_status::provider_statuses(conn, kind)? {
                        text.push_str(&format!(
                            "{}:{id}:{}:{};",
                            kind.as_str(),
                            s.escalation_level,
                            s.disabled_till.map(crate::time::format_utc).unwrap_or_default()
                        ));
                    }
                }
                Ok(text)
            })
            .await
            .unwrap_or_default();
        let changed = {
            let mut last = self.health_statuses.lock().unwrap();
            let changed = last.as_deref() != Some(snapshot.as_str());
            *last = Some(snapshot);
            changed
        };
        if changed {
            self.check_health(CheckSet::Statuses).await;
        }
    }

    /// Wait until no command is queued or started and no health run is in
    /// flight. For tests and the contract replay.
    pub async fn wait_idle(&self) {
        loop {
            self.queue.wait_idle().await;
            let runs: Vec<tokio::task::JoinHandle<()>> =
                std::mem::take(&mut *self.health_runs.lock().unwrap());
            if runs.is_empty() {
                return;
            }
            for run in runs {
                let _ = run.await;
            }
        }
    }

    pub fn queue(&self) -> &Arc<CommandQueue> {
        &self.queue
    }

    pub fn downloads(&self) -> &Arc<Downloads> {
        &self.downloads
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Spawn the workers and the scheduler. The tasks end with the Tokio
    /// runtime.
    pub fn start(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::with_capacity(WORKERS + 1);
        for _ in 0..WORKERS {
            let runtime = self.clone();
            handles.push(tokio::spawn(async move { runtime.worker().await }));
        }
        let runtime = self.clone();
        handles.push(tokio::spawn(async move { runtime.scheduler_loop().await }));
        // `Handle(ApplicationStartedEvent)`: the startup checks.
        self.check_health_later(CheckSet::All);
        handles
    }

    /// `ExecuteCommands`: take the next command whenever the queue changes.
    async fn worker(self: Arc<Self>) {
        loop {
            while !self.is_stopping() {
                let Some(model) = self.queue.try_start(self.now()) else {
                    break;
                };
                self.execute(model).await;
            }
            if self.is_stopping() {
                return;
            }
            self.queue.changed().await;
        }
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Stop taking commands and wait up to `grace` for the running ones:
    /// a grab interrupted between the client and its history row would
    /// leave a download the queue cannot account for.
    pub async fn shutdown(&self, grace: std::time::Duration) {
        self.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        self.queue.notify();
        let deadline = tokio::time::Instant::now() + grace;
        while self.queue.has_running() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if self.queue.has_running() {
            tracing::warn!("commands still running at shutdown; their results are lost");
        }
    }

    async fn scheduler_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(scheduler::TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires at once; the originals wait a full period.
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = self.tick_scheduler().await {
                tracing::error!(error = %e, "scheduler tick failed");
            }
            crate::fs::release_memory();
        }
    }

    /// `Scheduler.ExecuteCommands`: push every due task. Returns how many
    /// were pushed.
    pub async fn tick_scheduler(&self) -> anyhow::Result<usize> {
        let now = self.now();
        let due = self
            .db
            .call(move |conn| {
                // `Handle(ConfigSavedEvent)`: a changed interval setting
                // takes effect at the next tick.
                scheduler::refresh_intervals(conn)?;
                scheduler::pending_tasks(conn, now)
            })
            .await?;
        tracing::trace!(count = due.len(), "pending tasks");
        let mut pushed = 0;
        for task in due {
            let Some(body) = CommandBody::from_type_name(&task.type_name) else {
                tracing::warn!(task = task.type_name, "no command for the scheduled task");
                continue;
            };
            let priority = scheduler::priority_of(&task.type_name);
            self.queue
                .push_with(body, priority, CommandTrigger::Scheduled, now, |m| {
                    m.last_execution_time = task.last_execution;
                    m.last_start_time = task.last_start_time;
                })
                .await?;
            pushed += 1;
        }
        Ok(pushed)
    }

    /// `ExecuteCommand`: a started command run to its end, the result
    /// recorded, the scheduled task updated.
    pub async fn execute(&self, model: CommandModel) {
        let id = model.id;
        let name = model.name();
        tracing::trace!(name, id, "command started");
        self.arm_health_notifications().await;
        if let Err(e) = self.queue.record_started(&model).await {
            tracing::warn!(error = %e, "failed to record the command's start");
        }
        self.broadcast(&model);
        let outcome = self.run(&model).await;
        // `ProviderStatusChangedEvent` is published while the command runs:
        // the checks it asks for are done by the time the command ends.
        if matches!(
            model.body,
            CommandBody::RssSync
                | CommandBody::RefreshMonitoredDownloads
                | CommandBody::EpisodeSearch { .. }
                | CommandBody::SeasonSearch { .. }
                | CommandBody::SeriesSearch { .. }
                | CommandBody::MissingEpisodeSearch { .. }
                | CommandBody::CutoffUnmetEpisodeSearch { .. }
        ) {
            self.check_health_on_status_change().await;
        }
        let now = self.now();
        let ended = match outcome {
            Ok(()) => {
                let message = model
                    .body
                    .completion_message()
                    .map(String::from)
                    .or_else(|| self.queue.find(id).and_then(|c| c.message));
                self.queue
                    .end(id, CommandStatus::Completed, message, None, now)
                    .await
            }
            Err(e) => {
                tracing::error!(name, error = %e, "error occurred while executing task");
                self.queue
                    .end(
                        id,
                        CommandStatus::Failed,
                        Some("Failed".into()),
                        Some(format!("{e:#}")),
                        now,
                    )
                    .await
            }
        };
        let ended = match ended {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "failed to record the command's end");
                return;
            }
        };
        self.broadcast(&ended);
        // `CommandExecutedEvent` -> `TaskManager`.
        let body = ended.body.clone();
        let started = ended.started_at.unwrap_or(now);
        if let Err(e) = self
            .db
            .call(move |conn| scheduler::record_execution(conn, &body, now, started))
            .await
        {
            tracing::warn!(error = %e, "failed to record the scheduled task's run");
        }
        tracing::trace!(name, id, duration = ?ended.duration, "command ended");
    }

    fn broadcast(&self, model: &CommandModel) {
        if model.send_updates_to_client {
            self.emit(Event::CommandUpdated(Box::new(model.clone())));
        }
    }

    /// `ProgressInfo`: the message the command shows while it runs, and
    /// its final one.
    fn progress(&self, id: i64, message: String) {
        tracing::info!("{message}");
        self.queue.set_message(id, &message);
    }

    async fn run(&self, model: &CommandModel) -> anyhow::Result<()> {
        let id = model.id;
        let trigger = Trigger {
            user_invoked: model.trigger == CommandTrigger::Manual,
            interactive: false,
        };
        match &model.body {
            CommandBody::RssSync => self.rss_sync(id).await,
            CommandBody::RefreshMonitoredDownloads => self.refresh_monitored_downloads().await,
            CommandBody::ProcessMonitoredDownloads => self.process_monitored_downloads().await,
            CommandBody::RescanSeries { series_id } => self.rescan_series(id, *series_id).await,
            CommandBody::ManualImport { files, import_mode } => {
                self.manual_import(id, files, *import_mode).await
            }
            CommandBody::RenameFiles { series_id, files } => self.rename_files(id, *series_id, files).await,
            CommandBody::RenameSeries { series_ids } => self.rename_series(id, series_ids).await,
            CommandBody::DownloadedEpisodesScan {
                path,
                download_client_id,
                import_mode,
            } => {
                self.downloaded_episodes_scan(
                    id,
                    path.as_deref(),
                    download_client_id.as_deref(),
                    *import_mode,
                )
                .await
            }
            CommandBody::EpisodeSearch { episode_ids } => {
                for episode_id in episode_ids {
                    let decisions = self
                        .search
                        .episode_search(*episode_id, trigger, self.now())
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    let processed = self.process(decisions).await?;
                    self.progress(
                        id,
                        format!(
                            "Episode search completed. {} reports downloaded.",
                            processed.grabbed.len()
                        ),
                    );
                }
                Ok(())
            }
            CommandBody::SeasonSearch {
                series_id,
                season_number,
            } => {
                let decisions = self
                    .search
                    .season_search(*series_id, *season_number, false, true, trigger, self.now())
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                let processed = self.process(decisions).await?;
                self.progress(
                    id,
                    format!(
                        "Season search completed. {} reports downloaded.",
                        processed.grabbed.len()
                    ),
                );
                Ok(())
            }
            CommandBody::SeriesSearch { series_id } => self.series_search(id, *series_id, trigger).await,
            CommandBody::MissingEpisodeSearch { series_id, monitored } => {
                self.missing_search(id, *series_id, *monitored, trigger).await
            }
            CommandBody::CutoffUnmetEpisodeSearch { series_id, monitored } => {
                self.cutoff_unmet_search(id, *series_id, *monitored, trigger)
                    .await
            }
            CommandBody::ClearBlocklist => {
                self.db.call(|conn| store::history::clear_blocklist(conn)).await?;
                Ok(())
            }
            CommandBody::MessagingCleanup => self.queue.clean(self.now()).await,
            CommandBody::RefreshSeries {
                series_ids,
                is_new_series,
            } => {
                self.refresh_series(id, series_ids, *is_new_series, model.trigger)
                    .await
            }
            CommandBody::MoveSeries {
                series_id,
                source_path,
                destination_path,
            } => {
                self.move_series(id, *series_id, source_path, destination_path, None)
                    .await
            }
            CommandBody::BulkMoveSeries {
                destination_root_folder,
                series,
            } => self.bulk_move_series(id, destination_root_folder, series).await,
            CommandBody::UpdateSceneMapping => {
                crate::tv::scene::update_mappings(&self.db, &self.augment(), &self.scene).await;
                Ok(())
            }
            CommandBody::CheckHealth => {
                self.check_health(CheckSet::All).await;
                Ok(())
            }
            CommandBody::ImportListSync { definition_id } => {
                let progress = |message: String| self.progress(id, message);
                let outcome = self
                    .import_lists()
                    .sync(self.metadata().as_ref(), *definition_id, self.now(), &progress)
                    .await?;
                for series_id in outcome.added {
                    self.series_added(series_id).await?;
                }
                for series_id in outcome.updated {
                    self.emit(Event::SeriesUpdated { series_id });
                }
                Ok(())
            }
            CommandBody::Backup { kind } => {
                let context = self.backup.read().unwrap().clone();
                let Some(context) = context else {
                    tracing::debug!("no app data directory set, nothing to back up");
                    return Ok(());
                };
                self.progress(id, "Starting Backup".into());
                let path = crate::backup::create(&self.db, &context, *kind, self.now()).await?;
                tracing::debug!(path = %path.display(), "backup written");
                self.progress(id, "Backup zip created".into());
                Ok(())
            }
            CommandBody::Housekeeping
            | CommandBody::ApplicationUpdateCheck { .. }
            | CommandBody::CleanUpRecycleBin => {
                tracing::debug!(name = model.name(), "nothing to do for this command yet");
                Ok(())
            }
        }
    }

    /// `ProcessDecisions`, with the queue refreshed after a grab as the
    /// originals' debounced refresh does.
    async fn process(&self, decisions: Vec<Decision>) -> anyhow::Result<Processed> {
        let processed = process::process_decisions(&self.downloads, decisions, self.now()).await?;
        if !processed.grabbed.is_empty() {
            self.queue
                .push(
                    CommandBody::RefreshMonitoredDownloads,
                    CommandPriority::High,
                    CommandTrigger::Unspecified,
                    self.now(),
                )
                .await?;
        }
        if !processed.pending.is_empty() {
            self.emit(Event::QueueUpdated);
        }
        Ok(processed)
    }

    /// `RssSyncService.Sync`.
    async fn rss_sync(&self, id: i64) -> anyhow::Result<()> {
        self.progress(id, "Starting RSS Sync".into());
        let now = self.now();
        let mut reports = self.indexers.fetch_recent(now).await?;
        let pending = self.db.call(move |conn| pending::get_pending(conn, now)).await?;
        reports.extend(pending);
        let count = reports.len();
        let queue = self.downloads.queued_releases();
        let decisions = self
            .db
            .call(move |conn| {
                let catalog = decision::Catalog::load(conn, now)?.with_queue(queue);
                let lookup = decision::StoreLookup::new(conn);
                Ok(decision::decide(
                    &catalog,
                    &lookup,
                    now,
                    reports,
                    DecisionInput::Rss,
                ))
            })
            .await?;
        let processed = self.process(decisions).await?;
        let mut message = format!(
            "RSS Sync Completed. Reports found: {}, Reports grabbed: {}",
            count,
            processed.grabbed.len()
        );
        if !processed.pending.is_empty() {
            message.push_str(&format!(", Reports pending: {}", processed.pending.len()));
        }
        self.progress(id, message);
        // `RssSyncCompleteEvent` -> the pending releases drop what was rejected.
        let rejected = processed.rejected;
        let removed = self
            .db
            .call(move |conn| pending::remove_rejected(conn, &rejected))
            .await?;
        if removed {
            self.emit(Event::QueueUpdated);
        }
        Ok(())
    }

    /// `DownloadMonitoringService.Refresh`: the clients asked, then the
    /// processing of what completed or failed queued as its own command.
    async fn refresh_monitored_downloads(&self) -> anyhow::Result<()> {
        if self.downloads.refresh(self.now()).await? {
            self.emit(Event::QueueUpdated);
        }
        self.queue
            .push(
                CommandBody::ProcessMonitoredDownloads,
                CommandPriority::High,
                CommandTrigger::Unspecified,
                self.now(),
            )
            .await?;
        Ok(())
    }

    /// `DownloadProcessingService.Execute(ProcessMonitoredDownloadsCommand)`.
    async fn process_monitored_downloads(&self) -> anyhow::Result<()> {
        let events = completed::process_monitored_downloads(&self.downloads, self.now()).await?;
        self.emit_import_events(events).await;
        // `DownloadsProcessedEvent` -> `TrackedDownloadRefreshedEvent` -> the
        // queue, and the notifications flush what they queued.
        self.emit(Event::QueueUpdated);
        self.notifier.process_queue(self.now()).await;
        Ok(())
    }

    /// Events the API raised outside a command (a file deleted through it):
    /// relayed like the commands' own.
    pub async fn publish_import_events(&self, events: Vec<ImportEvent>) {
        self.emit_import_events(events).await;
    }

    /// `Execute(ManualImportCommand)`.
    async fn manual_import(
        &self,
        id: i64,
        files: &[ManualImportFile],
        mode: ImportMode,
    ) -> anyhow::Result<()> {
        let now = self.now();
        let env = self.db.call(move |conn| Environment::load(conn, now)).await?;
        let (db, downloads, files) = (self.db.clone(), self.downloads.clone(), files.to_vec());
        let (messages, outcome) = tokio::task::spawn_blocking(move || {
            let mut messages = Vec::new();
            let outcome = manual::execute(&db, &env, &downloads, &files, mode, now, &mut |m| {
                messages.push(m)
            });
            (messages, outcome)
        })
        .await?;
        for m in messages {
            self.progress(id, m);
        }
        let outcome = outcome.map_err(|e| anyhow::anyhow!("{e}"))?;
        for mut t in outcome.completed {
            completed::download_completed(&self.downloads, &mut t, now).await;
            self.downloads.update_tracked(t);
        }
        self.downloads.rebuild_queue_now(now);
        self.emit_import_events(outcome.events.into_vec()).await;
        self.emit(Event::QueueUpdated);
        Ok(())
    }

    /// `Execute(RenameFilesCommand)`.
    async fn rename_files(&self, id: i64, series_id: i64, files: &[i64]) -> anyhow::Result<()> {
        let now = self.now();
        let ids = files.to_vec();
        let (env, series, files) = self
            .db
            .call(move |conn| {
                let series = store::series::get(conn, series_id)?
                    .ok_or_else(|| anyhow::anyhow!("Series with ID {series_id} does not exist"))?;
                Ok((
                    Environment::load(conn, now)?,
                    series,
                    store::episodes::list_files_by_ids(conn, &ids)?,
                ))
            })
            .await?;
        self.progress(id, format!("Renaming {} files for {}", files.len(), series.title));
        let db = self.db.clone();
        let title = series.title.clone();
        let (renamed, events) =
            tokio::task::spawn_blocking(move || rename::rename_files(&db, &env, &series, files, now))
                .await??;
        self.progress(
            id,
            format!("{} selected episode files renamed for {}", renamed.len(), title),
        );
        self.emit_import_events(events.into_vec()).await;
        // `RenameCompletedEvent`.
        self.notifier.process_queue(self.now()).await;
        Ok(())
    }

    /// `Execute(RenameSeriesCommand)`.
    async fn rename_series(&self, id: i64, series_ids: &[i64]) -> anyhow::Result<()> {
        tracing::debug!("Renaming all files for selected series");
        let now = self.now();
        let ids = series_ids.to_vec();
        let (env, series) = self
            .db
            .call(move |conn| {
                Ok((
                    Environment::load(conn, now)?,
                    store::series::get_many(conn, &ids)?,
                ))
            })
            .await?;
        for s in series {
            let sid = s.id;
            let files = self
                .db
                .call(move |conn| store::episodes::list_files(conn, sid))
                .await?;
            self.progress(id, format!("Renaming all files in series: {}", s.title));
            let (db, env2, title) = (self.db.clone(), env.clone(), s.title.clone());
            let (renamed, events) =
                tokio::task::spawn_blocking(move || rename::rename_files(&db, &env2, &s, files, now))
                    .await??;
            self.progress(
                id,
                format!("{} episode files renamed for {}", renamed.len(), title),
            );
            self.emit_import_events(events.into_vec()).await;
        }
        // `RenameCompletedEvent`.
        self.notifier.process_queue(self.now()).await;
        Ok(())
    }

    /// The importer's events relayed, with the rescans a destination clash
    /// asks for queued.
    async fn emit_import_events(&self, events: Vec<ImportEvent>) {
        // `NotificationService`'s synchronous handlers, in event order.
        self.notifier.on_import_events(&events, self.now()).await;
        // The health checks an import's outcome triggers, debounced; their
        // completion is what flushes the media servers' update queues.
        self.check_health_after_import(&events);
        for e in events {
            if let ImportEvent::SeriesScanned { series_id } = &e {
                self.after_scan(*series_id).await;
            }
            if let ImportEvent::RescanRequested { series_id } = &e {
                if let Err(err) = self
                    .queue
                    .push(
                        CommandBody::RescanSeries {
                            series_id: Some(*series_id),
                        },
                        CommandPriority::Normal,
                        CommandTrigger::Unspecified,
                        self.now(),
                    )
                    .await
                {
                    tracing::warn!(error = %err, "failed to queue the rescan");
                }
                continue;
            }
            self.emit(Event::Import(Box::new(e)));
        }
    }

    /// `RefreshSeriesService.Execute`: the series asked for, or every one
    /// (by sort title) that is due unless the refresh is manual; each
    /// followed by its tag update and rescan.
    async fn refresh_series(
        &self,
        id: i64,
        series_ids: &[i64],
        is_new: bool,
        trigger: CommandTrigger,
    ) -> anyhow::Result<()> {
        let manual = trigger == CommandTrigger::Manual;
        // `SeriesRefreshStartingEvent`.
        let augment = self.augment();
        if manual && self.scene.mappings_due() {
            crate::tv::scene::update_mappings(&self.db, &augment, &self.scene).await;
        }
        crate::tv::scene::refresh_starting(&augment, &self.scene, manual).await;

        if !series_ids.is_empty() {
            for &series_id in series_ids {
                match self.refresh_one(id, series_id).await {
                    Ok(_) => {
                        self.update_tags(series_id).await;
                        self.rescan_after_refresh(id, series_id, is_new, trigger).await
                    }
                    Err(crate::tv::refresh::RefreshError::NotFound { series, .. }) => {
                        tracing::error!(
                            "Series '{}' (tvdbid {}) was not found, it may have been removed from TheTVDB.",
                            series.title,
                            series.tvdb_id
                        );
                    }
                    Err(crate::tv::refresh::RefreshError::Other(e)) => {
                        tracing::error!(error = %e, "Couldn't refresh info for series {series_id}");
                        self.update_tags(series_id).await;
                        self.rescan_after_refresh(id, series_id, is_new, trigger).await;
                        return Err(e);
                    }
                }
            }
            return Ok(());
        }

        let now = self.now();
        let mut all = self
            .db
            .call(move |conn| {
                let series = store::series::list(conn)?;
                let mut out = Vec::new();
                for s in series {
                    let episodes = if manual {
                        Vec::new()
                    } else {
                        store::episodes::list_episodes(conn, s.id, None)?
                    };
                    let due = manual || crate::tv::refresh::should_refresh(&s, &episodes, now);
                    out.push((
                        s.sort_title.clone().unwrap_or_default(),
                        s.id,
                        s.title.clone(),
                        due,
                    ));
                }
                Ok(out)
            })
            .await?;
        // `OrderBy(SortTitle)`: ordinal, stable.
        all.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, series_id, title, due) in all {
            if due {
                match self.refresh_one(id, series_id).await {
                    Ok(_) => {}
                    Err(crate::tv::refresh::RefreshError::NotFound { series, .. }) => {
                        tracing::error!(
                            "Series '{}' (tvdbid {}) was not found, it may have been removed from TheTVDB.",
                            series.title,
                            series.tvdb_id
                        );
                        continue;
                    }
                    Err(crate::tv::refresh::RefreshError::Other(e)) => {
                        tracing::error!(error = %e, "Couldn't refresh info for {title}");
                    }
                }
            } else {
                tracing::info!("Skipping refresh of series: {title}");
            }
            self.update_tags(series_id).await;
            self.rescan_after_refresh(id, series_id, false, trigger).await;
        }
        Ok(())
    }

    /// `RefreshSeriesService.UpdateTags`: the auto tagging rules applied
    /// to the series as stored, written and announced when its tags
    /// changed. The originals pass the series as it was before the refresh
    /// when every series is refreshed, and write it back whole; here the
    /// rules see what the refresh just stored and only the tags are written.
    async fn update_tags(&self, series_id: i64) {
        let updated = self
            .db
            .call(move |conn| crate::tv::autotag::update_stored(conn, series_id))
            .await;
        match updated {
            Ok(Some(_)) => self.emit(Event::SeriesUpdated { series_id }),
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "Couldn't update the tags of series {series_id}"),
        }
    }

    /// `RefreshSeriesInfo` and the handlers of what it publishes.
    async fn refresh_one(
        &self,
        id: i64,
        series_id: i64,
    ) -> Result<crate::tv::refresh::Refreshed, crate::tv::refresh::RefreshError> {
        let title = self
            .db
            .call(move |conn| {
                Ok(store::series::get_many(conn, &[series_id])?
                    .pop()
                    .map(|s| s.title))
            })
            .await?;
        if let Some(title) = title {
            self.progress(id, format!("Updating {title}"));
        }
        let provider = self.metadata();
        match crate::tv::refresh::refresh_series_info(&self.db, provider.as_ref(), series_id, self.now())
            .await
        {
            Ok(refreshed) => {
                self.episodes_refreshed(&refreshed);
                let removed: Vec<i64> = refreshed.removed.iter().map(|e| e.id).collect();
                self.queue_changed(self.downloads.episodes_removed(&removed, self.now()).await);
                self.series_updated(&refreshed.series).await;
                Ok(refreshed)
            }
            Err(crate::tv::refresh::RefreshError::NotFound {
                series,
                marked_deleted,
            }) => {
                if marked_deleted {
                    self.series_updated(&series).await;
                }
                Err(crate::tv::refresh::RefreshError::NotFound {
                    series,
                    marked_deleted,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// `EpisodeRefreshedService.Handle(EpisodeInfoRefreshedEvent)`: the
    /// monitored episodes added (or given an absolute number) that aired in
    /// the last two weeks, kept for the search after the scan.
    fn episodes_refreshed(&self, refreshed: &crate::tv::refresh::Refreshed) {
        let series = &refreshed.series;
        if series.add_options.is_some() {
            return;
        }
        if !series.monitored {
            tracing::debug!("Series is not monitored");
            return;
        }
        let now = self.now();
        let recent = |e: &crate::model::Episode| {
            e.monitored
                && e.air_date_utc.is_some_and(|d| {
                    d >= now - chrono::Duration::days(14) && d <= now + chrono::Duration::days(1)
                })
        };
        let mut ids: Vec<i64> = refreshed
            .added
            .iter()
            .filter(|e| recent(e))
            .map(|e| e.id)
            .collect();
        ids.extend(
            refreshed
                .updated
                .iter()
                .filter(|e| refreshed.absolute_added.contains(&e.id) && recent(e))
                .map(|e| e.id),
        );
        let mut seen = std::collections::HashSet::new();
        ids.retain(|i| seen.insert(*i));
        if !ids.is_empty() {
            self.refreshed_search.lock().unwrap().insert(series.id, ids);
        }
    }

    /// `EpisodeRefreshedService.Search`, after the series was scanned: the
    /// kept episodes still without a file are searched for.
    async fn search_refreshed(&self, series_id: i64) {
        let Some(ids) = self.refreshed_search.lock().unwrap().remove(&series_id) else {
            return;
        };
        let missing = self
            .db
            .call(move |conn| {
                Ok(store::episodes::list_episodes_by_ids(conn, &ids)?
                    .into_iter()
                    .filter(|e| e.episode_file_id == 0)
                    .map(|e| e.id)
                    .collect::<Vec<_>>())
            })
            .await;
        if let Some(missing) = missing.ok().filter(|m| !m.is_empty()) {
            if let Err(e) = self
                .queue
                .push(
                    CommandBody::EpisodeSearch { episode_ids: missing },
                    CommandPriority::Normal,
                    CommandTrigger::Unspecified,
                    self.now(),
                )
                .await
            {
                tracing::warn!(error = %e, "failed to queue the search of refreshed episodes");
            }
        }
    }

    /// The handlers of `SeriesUpdatedEvent`: the series resource, XEM's
    /// scene numbering, the covers (and the resource again when they
    /// changed).
    async fn series_updated(&self, series: &crate::model::Series) {
        self.emit(Event::SeriesUpdated { series_id: series.id });
        let augment = self.augment();
        crate::tv::scene::series_updated(&self.db, &augment, &self.scene, series).await;
        let root = self.media_cover_dir.read().unwrap().clone();
        if let Some(root) = root {
            if crate::tv::covers::ensure_covers(&root, &augment, series).await {
                self.emit(Event::SeriesUpdated { series_id: series.id });
            }
        }
    }

    /// `RescanSeries` after a refresh, as `RescanAfterRefresh` says.
    async fn rescan_after_refresh(&self, id: i64, series_id: i64, is_new: bool, trigger: CommandTrigger) {
        use crate::store::config::RescanAfterRefresh;
        let setting = self
            .db
            .call(|conn| Ok(crate::store::config::MediaManagementConfig::load(conn)?.rescan_after_refresh))
            .await
            .unwrap_or_default();
        if !is_new {
            let skip = match setting {
                RescanAfterRefresh::Never => true,
                RescanAfterRefresh::AfterManual => trigger != CommandTrigger::Manual,
                RescanAfterRefresh::Always => false,
            };
            if skip {
                // `SeriesScanSkippedEvent`, handled as a scan.
                self.after_scan(series_id).await;
                return;
            }
        }
        if let Err(e) = self.rescan_series(id, Some(series_id)).await {
            tracing::error!(error = %e, "Couldn't rescan series {series_id}");
        }
    }

    /// `SeriesScannedHandler`: what follows the scan of a series; for a
    /// series being added, the post-add actions (the monitoring preset, the
    /// notification, the searches asked for), once.
    async fn after_scan(&self, series_id: i64) {
        let now = self.now();
        let added = self
            .db
            .call(move |conn| {
                let Some(mut series) = store::series::get(conn, series_id)? else {
                    return Ok(None);
                };
                let Some(options) = series.add_options.clone() else {
                    return Ok(None);
                };
                tracing::info!(
                    "[{}] was recently added, performing post-add actions",
                    series.title
                );
                let monitoring = crate::tv::monitoring::MonitoringOptions::from_json(&options);
                crate::tv::series::set_episode_monitored_status(conn, &mut series, &monitoring, now)?;
                store::series::clear_add_options(conn, series_id)?;
                Ok(Some((series, options)))
            })
            .await;
        let (series, options) = match added {
            Ok(Some(x)) => x,
            Ok(None) => {
                self.search_refreshed(series_id).await;
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "post-add actions failed for series {series_id}");
                return;
            }
        };
        // `SeriesAddCompletedEvent`.
        self.notifier.on_series_add(&series, now).await;
        self.emit(Event::SeriesUpdated { series_id });
        let flag = |k: &str| {
            options
                .get(k)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        let (missing, cutoff) = (
            flag("searchForMissingEpisodes"),
            flag("searchForCutoffUnmetEpisodes"),
        );
        let mut searches = Vec::new();
        if missing && cutoff {
            searches.push(CommandBody::SeriesSearch { series_id });
        } else {
            if missing {
                searches.push(CommandBody::MissingEpisodeSearch {
                    series_id: Some(series_id),
                    monitored: true,
                });
            }
            if cutoff {
                searches.push(CommandBody::CutoffUnmetEpisodeSearch {
                    series_id: Some(series_id),
                    monitored: true,
                });
            }
        }
        for body in searches {
            if let Err(e) = self
                .queue
                .push(body, CommandPriority::Normal, CommandTrigger::Unspecified, now)
                .await
            {
                tracing::warn!(error = %e, "failed to queue the search of an added series");
            }
        }
    }

    /// `SeriesAddedHandler` and `SceneMappingService.Handle(SeriesAddedEvent)`:
    /// a new series is refreshed (and scanned) as new; the scene mappings
    /// are updated if they never were since the start.
    pub async fn series_added(&self, series_id: i64) -> anyhow::Result<()> {
        let tvdb_id = self
            .db
            .call(move |conn| {
                Ok(store::series::get_many(conn, &[series_id])?
                    .pop()
                    .map(|s| s.tvdb_id))
            })
            .await?;
        if let Some(tvdb_id) = tvdb_id {
            self.queue_changed(self.downloads.series_added(tvdb_id, self.now()).await);
        }
        if self.scene.mappings_never_updated() {
            crate::tv::scene::update_mappings(&self.db, &self.augment(), &self.scene).await;
        }
        self.queue
            .push(
                CommandBody::RefreshSeries {
                    series_ids: vec![series_id],
                    is_new_series: true,
                },
                CommandPriority::Normal,
                CommandTrigger::Unspecified,
                self.now(),
            )
            .await?;
        Ok(())
    }

    /// `TrackedDownloadRefreshedEvent` after the tracked downloads were
    /// matched again: the queue goes out when one was.
    fn queue_changed(&self, remapped: anyhow::Result<bool>) {
        match remapped {
            Ok(true) => self.emit(Event::QueueUpdated),
            Ok(false) => {}
            Err(e) => tracing::error!(error = %e, "Couldn't update the tracked downloads"),
        }
    }

    /// The handlers of `SeriesDeletedEvent` outside the database: the
    /// folders when asked, the covers, the notifications, the media
    /// servers' queue.
    pub async fn series_deleted(&self, series: Vec<crate::model::Series>, delete_files: bool) {
        let now = self.now();
        let gone: Vec<(i64, i64)> = series.iter().map(|s| (s.id, s.tvdb_id)).collect();
        self.queue_changed(self.downloads.series_deleted(&gone, now).await);
        if delete_files {
            let (db, list) = (self.db.clone(), series.clone());
            let _ = tokio::task::spawn_blocking(move || {
                let config =
                    db.call_blocking(|conn| crate::store::config::MediaManagementConfig::load(conn))?;
                let remaining = db.call_blocking(|conn| store::series::all_paths(conn))?;
                for s in &list {
                    crate::tv::series::delete_series_folder(&config, s, &remaining);
                }
                anyhow::Ok(())
            })
            .await;
        }
        // The originals delete the covers in a background handler, after
        // the notifications have read their URLs.
        self.notifier.on_series_delete(&series, delete_files, now).await;
        let root = self.media_cover_dir.read().unwrap().clone();
        if let Some(root) = root {
            for s in &series {
                crate::tv::covers::delete_covers(&root, s.id).await;
            }
        }
        if delete_files {
            // `DeleteCompletedEvent`, once per series.
            for _ in &series {
                self.notifier
                    .on_import_events(&[ImportEvent::DeleteCompleted], now)
                    .await;
            }
        }
    }

    /// `MoveSeriesService.MoveSingleSeries`.
    async fn move_series(
        &self,
        id: i64,
        series_id: i64,
        source: &str,
        destination: &str,
        position: Option<(usize, usize)>,
    ) -> anyhow::Result<()> {
        let title = self
            .db
            .call(move |conn| {
                Ok(store::series::get_many(conn, &[series_id])?
                    .pop()
                    .map(|s| s.title))
            })
            .await?
            .unwrap_or_default();
        if !source.starts_with('/') {
            tracing::warn!(
                "Folder '{source}' for '{title}' is invalid, unable to move series. Try moving files manually"
            );
            return Ok(());
        }
        if !std::path::Path::new(source).is_dir() {
            tracing::debug!("Folder '{source}' for '{title}' does not exist, not moving.");
            return Ok(());
        }
        match position {
            Some((i, n)) => self.progress(
                id,
                format!(
                    "Moving {title} from '{source}' to '{destination}' ({}/{n})",
                    i + 1
                ),
            ),
            None => self.progress(id, format!("Moving {title} from '{source}' to '{destination}'")),
        }
        if crate::mediafiles::path_equals(source, destination) {
            self.progress(
                id,
                format!("{title} is already in the specified location '{destination}'."),
            );
            return Ok(());
        }
        let (src, dst) = (
            std::path::PathBuf::from(source),
            std::path::PathBuf::from(destination),
        );
        let moved =
            tokio::task::spawn_blocking(move || crate::mediafiles::transfer::move_folder(&src, &dst)).await?;
        match moved {
            Ok(()) => {
                self.progress(id, format!("{title} moved successfully to {destination}"));
                self.emit(Event::SeriesUpdated { series_id });
            }
            Err(e) => {
                tracing::error!(error = %e, "Unable to move series from '{source}' to '{destination}'. Try moving files manually");
                let source = source.to_string();
                self.db
                    .call(move |conn| {
                        conn.execute(
                            "UPDATE series SET path = ?2 WHERE id = ?1",
                            rusqlite::params![series_id, source],
                        )?;
                        Ok(())
                    })
                    .await?;
            }
        }
        Ok(())
    }

    /// `MoveSeriesService.Execute(BulkMoveSeriesCommand)`.
    async fn bulk_move_series(&self, id: i64, root: &str, series: &[(i64, String)]) -> anyhow::Result<()> {
        self.progress(id, format!("Moving {} series to '{root}'", series.len()));
        for (i, (series_id, source)) in series.iter().enumerate() {
            let sid = *series_id;
            let folder = self
                .db
                .call(move |conn| {
                    let naming = store::config::naming(conn)?;
                    Ok(store::series::get(conn, sid)?.map(|s| {
                        crate::naming::series_folder(&crate::mediafiles::approve::naming_series(&s), &naming)
                    }))
                })
                .await?;
            let Some(folder) = folder else { continue };
            let destination = crate::mediafiles::join(root, &folder);
            self.move_series(id, sid, source, &destination, Some((i, series.len())))
                .await?;
        }
        self.progress(id, format!("Finished moving {} series to '{root}'", series.len()));
        Ok(())
    }

    /// `DiskScanService.Execute(RescanSeriesCommand)`.
    async fn rescan_series(&self, id: i64, series_id: Option<i64>) -> anyhow::Result<()> {
        let now = self.now();
        let (env, series) = self
            .db
            .call(move |conn| {
                let series = match series_id {
                    Some(sid) => store::series::get(conn, sid)?
                        .map(|s| vec![s])
                        .ok_or_else(|| anyhow::anyhow!("Series with ID {sid} does not exist"))?,
                    None => store::series::list(conn)?,
                };
                Ok((Environment::load(conn, now)?, series))
            })
            .await?;
        for s in series {
            let (db, env2) = (self.db.clone(), env.clone());
            let (messages, events) = tokio::task::spawn_blocking(move || {
                let mut messages = Vec::new();
                let events = scan::scan(&db, &env2, &s, now, &mut |m| messages.push(m));
                (messages, events)
            })
            .await?;
            for m in messages {
                self.progress(id, m);
            }
            self.emit_import_events(events?.into_vec()).await;
        }
        Ok(())
    }

    /// `DownloadedEpisodesCommandService.Execute(DownloadedEpisodesScanCommand)`.
    async fn downloaded_episodes_scan(
        &self,
        id: i64,
        path: Option<&str>,
        download_client_id: Option<&str>,
        import_mode: ImportMode,
    ) -> anyhow::Result<()> {
        let Some(path) = path.map(str::to_string) else {
            anyhow::bail!("A path must be provided (Parameter 'path')");
        };
        let now = self.now();
        let env = self.db.call(move |conn| Environment::load(conn, now)).await?;
        let mut results = Vec::new();
        if !std::path::Path::new(&path).exists() {
            tracing::warn!("Folder/File specified for import scan [{path}] doesn't exist.");
        } else {
            let mut tracked = download_client_id.and_then(|d| self.downloads.tracked(d));
            if download_client_id.is_some() && tracked.is_none() {
                tracing::warn!(
                    "External directory scan request for unknown download {}, attempting normal import. [{path}]",
                    download_client_id.unwrap_or("")
                );
            }
            if let Some(t) = tracked.as_ref() {
                tracing::debug!(
                    "External directory scan request for known download {}. [{path}]",
                    t.download_id()
                );
            }
            let db = self.db.clone();
            let (imported, updated, verify_events) = tokio::task::spawn_blocking(move || {
                let (series, item) = match tracked.as_ref() {
                    Some(t) => (
                        t.remote.as_ref().and_then(|r| r.series.clone()),
                        Some(t.item.clone()),
                    ),
                    None => (None, None),
                };
                let imported = downloaded::process_path(
                    &db,
                    &env,
                    &path,
                    import_mode,
                    series.as_ref(),
                    item.as_ref(),
                    now,
                );
                let mut events = crate::mediafiles::Events::default();
                if let Some(t) = tracked.as_mut() {
                    completed::verify_import(&db, t, &imported.results, now, &mut events);
                }
                (imported, tracked, events)
            })
            .await?;
            if let Some(t) = updated {
                self.downloads.update_tracked(t);
                self.downloads.rebuild_queue_now(now);
            }
            results = imported.results;
            let mut events = imported.events;
            events.extend(verify_events);
            for series_id in imported.rescan_series {
                events.push(ImportEvent::RescanRequested { series_id });
            }
            self.emit_import_events(events.into_vec()).await;
        }
        if results.iter().all(|r| r.episode_file.is_none()) {
            // The command completes, but reports as unsuccessful.
            self.progress(id, "Failed to import".into());
            self.queue.set_result(id, super::CommandResult::Unsuccessful);
        }
        Ok(())
    }

    /// `SeriesSearchService`.
    async fn series_search(&self, id: i64, series_id: i64, trigger: Trigger) -> anyhow::Result<()> {
        let now = self.now();
        let series = self
            .db
            .call(move |conn| store::series::get(conn, series_id))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Series with ID {series_id} does not exist"))?;
        let mut downloaded = 0;
        if !series.seasons.iter().any(|s| s.monitored) {
            tracing::debug!(
                series = series.title,
                "no seasons are monitored, searching for all monitored episodes"
            );
            let episodes: Vec<Episode> = self
                .db
                .call(move |conn| store::episodes::list_episodes(conn, series_id, None))
                .await?
                .into_iter()
                .filter(|e| e.monitored && e.episode_file_id == 0 && e.air_date_utc.is_some_and(|a| a < now))
                .collect();
            for episode in episodes {
                let decisions = self
                    .search
                    .episode_search_for(episode, trigger, now)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                downloaded += self.process(decisions).await?.grabbed.len();
            }
        } else {
            let mut seasons: Vec<_> = series.seasons.iter().collect();
            seasons.sort_by_key(|s| s.season_number);
            for season in seasons {
                if !season.monitored {
                    tracing::debug!(
                        series = series.title,
                        season = season.season_number,
                        "season is not monitored, skipping search"
                    );
                    continue;
                }
                let decisions = self
                    .search
                    .season_search(series_id, season.season_number, false, true, trigger, now)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                downloaded += self.process(decisions).await?.grabbed.len();
            }
        }
        self.progress(
            id,
            format!("Series search completed. {downloaded} reports downloaded."),
        );
        Ok(())
    }

    /// Episode ids of the tracked downloads (`QueueService.GetQueue`),
    /// which the missing and cutoff searches skip.
    fn queued_episode_ids(&self) -> Vec<i64> {
        self.downloads
            .queue()
            .iter()
            .filter_map(|q| q.episode.as_ref().map(|e| e.id))
            .collect()
    }

    /// `Execute(MissingEpisodeSearchCommand)`.
    async fn missing_search(
        &self,
        id: i64,
        series_id: Option<i64>,
        monitored: bool,
        trigger: Trigger,
    ) -> anyhow::Result<()> {
        let now = self.now();
        let episodes: Vec<Episode> = match series_id {
            Some(series_id) => self
                .db
                .call(move |conn| store::episodes::list_episodes(conn, series_id, None))
                .await?
                .into_iter()
                .filter(|e| {
                    e.monitored == monitored
                        && e.episode_file_id == 0
                        && e.air_date_utc.is_some_and(|a| a < now)
                })
                .collect(),
            None => {
                self.db
                    .call(move |conn| {
                        let paging = store::paging::Paging {
                            page: 1,
                            page_size: 1_000_000,
                            sort_key: "id".into(),
                            descending: false,
                        };
                        Ok(store::episodes::missing_episodes(conn, &paging, monitored, now)?.0)
                    })
                    .await?
            }
        };
        let queued = self.queued_episode_ids();
        let missing: Vec<Episode> = episodes.into_iter().filter(|e| !queued.contains(&e.id)).collect();
        self.search_bulk(id, missing, monitored, trigger).await
    }

    /// `Execute(CutoffUnmetEpisodeSearchCommand)`.
    async fn cutoff_unmet_search(
        &self,
        id: i64,
        series_id: Option<i64>,
        monitored: bool,
        trigger: Trigger,
    ) -> anyhow::Result<()> {
        let episodes: Vec<Episode> = self
            .db
            .call(move |conn| {
                let profiles = store::catalog::list_quality_profiles(conn)?;
                let mut out: Vec<Episode> = store::episodes::episodes_with_files(conn)?
                    .into_iter()
                    .filter(|f| series_id.is_none_or(|id| f.episode.series_id == id))
                    .filter(|f| {
                        profiles
                            .iter()
                            .find(|p| p.id == f.quality_profile_id)
                            .is_some_and(|p| profile::qualities_below_cutoff(p).contains(&f.quality_id))
                    })
                    .filter(|f| {
                        if monitored {
                            f.episode.monitored && f.series_monitored
                        } else {
                            !f.episode.monitored || !f.series_monitored
                        }
                    })
                    .map(|f| f.episode)
                    .collect();
                out.sort_by_key(|e| e.id);
                Ok(out)
            })
            .await?;
        let queued = self.queued_episode_ids();
        let unmet: Vec<Episode> = episodes.into_iter().filter(|e| !queued.contains(&e.id)).collect();
        self.search_bulk(id, unmet, monitored, trigger).await
    }

    /// `SearchForBulkEpisodes`: by series and season, the longest unsearched
    /// first; a season with several episodes is one season search.
    async fn search_bulk(
        &self,
        id: i64,
        episodes: Vec<Episode>,
        monitored_only: bool,
        trigger: Trigger,
    ) -> anyhow::Result<()> {
        self.progress(id, format!("Performing search for {} episodes", episodes.len()));
        let mut groups: Vec<(i64, i64, Vec<Episode>)> = Vec::new();
        for e in &episodes {
            match groups
                .iter_mut()
                .find(|(s, season, _)| *s == e.series_id && *season == e.season_number)
            {
                Some((_, _, list)) => list.push(e.clone()),
                None => groups.push((e.series_id, e.season_number, vec![e.clone()])),
            }
        }
        groups.sort_by_key(|(_, _, list)| list.iter().map(|e| e.last_search_time.unwrap_or_default()).min());
        let mut downloaded = 0;
        for (series_id, season_number, group) in groups {
            let now = self.now();
            let decisions = if group.len() > 1 {
                match self
                    .search
                    .season_search_episodes(series_id, season_number, group, monitored_only, trigger, now)
                    .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!(series_id, season_number, error = %e, "unable to search for episodes in season");
                        continue;
                    }
                }
            } else {
                let episode = group.into_iter().next().expect("a group has an episode");
                match self.search.episode_search_for(episode, trigger, now).await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!(series_id, season_number, error = %e, "unable to search for episode");
                        continue;
                    }
                }
            };
            downloaded += self.process(decisions).await?.grabbed.len();
        }
        self.progress(
            id,
            format!(
                "Completed search for {} episodes. {} reports downloaded.",
                episodes.len(),
                downloaded
            ),
        );
        Ok(())
    }
}
