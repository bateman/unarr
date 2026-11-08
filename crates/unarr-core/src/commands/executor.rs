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
use crate::health::{self, CheckSet, HealthCheck};
use crate::indexers::IndexerService;
use crate::model::Episode;
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
    /// The provider statuses the last status-driven run saw, so a refresh
    /// that changed nothing does not ask the clients again.
    health_statuses: Mutex<Option<String>>,
    is_docker: bool,
    /// Set at shutdown: the workers take no further command.
    stopping: std::sync::atomic::AtomicBool,
}

impl Runtime {
    /// Open the queue over `db`, bring the scheduled tasks in line with
    /// the defaults, and hand the queue to the downloads for the searches
    /// a failed download triggers.
    pub async fn new(db: Db, downloads: Arc<Downloads>, clock: Clock) -> anyhow::Result<Arc<Runtime>> {
        crate::time::process_start();
        let queue = Arc::new(CommandQueue::open(db.clone()).await?);
        let now = clock();
        db.call(move |conn| scheduler::ensure_defaults(conn, now)).await?;
        downloads.set_commands(queue.clone());
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Runtime {
            search: SearchService::new(db.clone()).with_queue(downloads.queue_source()),
            indexers: IndexerService::new(db.clone()),
            db,
            queue,
            downloads,
            clock,
            events,
            health: Mutex::new(Vec::new()),
            health_runs: Mutex::new(Vec::new()),
            health_statuses: Mutex::new(None),
            is_docker: std::path::Path::new("/.dockerenv").exists(),
            stopping: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// The health verdicts that stand.
    pub fn health(&self) -> Vec<HealthCheck> {
        self.health.lock().unwrap().clone()
    }

    /// Run a set of health checks now and keep what they found; `true`
    /// when a verdict changed.
    pub async fn check_health(&self, set: CheckSet) -> bool {
        let clients = self.downloads.clients();
        let ctx = health::Context {
            db: &self.db,
            indexers: &self.indexers,
            clients,
            now: self.now(),
            is_docker: self.is_docker,
        };
        let run = health::run(&ctx, set).await;
        let changed = health::merge(&mut self.health.lock().unwrap(), run);
        if changed {
            self.emit(Event::HealthChanged);
        }
        changed
    }

    /// Run a set of health checks in the background after the originals'
    /// debounce, as a provider change or the start asks for.
    pub fn check_health_later(self: &Arc<Self>, set: CheckSet) {
        let runtime = self.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(HEALTH_DELAY).await;
            runtime.check_health(set).await;
        });
        let mut runs = self.health_runs.lock().unwrap();
        runs.retain(|h| !h.is_finished());
        runs.push(handle);
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
        if let Err(e) = self.queue.record_started(&model).await {
            tracing::warn!(error = %e, "failed to record the command's start");
        }
        self.broadcast(&model);
        let outcome = self.run(&model).await;
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
        if matches!(
            ended.body,
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
            CommandBody::RefreshSeries { .. } | CommandBody::RescanSeries { .. } => {
                tracing::warn!(name = model.name(), "metadata refreshes and disk scans arrive with the importer; the command completes without effect");
                Ok(())
            }
            CommandBody::CheckHealth => {
                self.check_health(CheckSet::All).await;
                Ok(())
            }
            CommandBody::Housekeeping
            | CommandBody::ApplicationUpdateCheck { .. }
            | CommandBody::UpdateSceneMapping
            | CommandBody::CleanUpRecycleBin
            | CommandBody::ImportListSync { .. }
            | CommandBody::Backup { .. } => {
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

    async fn refresh_monitored_downloads(&self) -> anyhow::Result<()> {
        if self.downloads.refresh(self.now()).await? {
            self.emit(Event::QueueUpdated);
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
