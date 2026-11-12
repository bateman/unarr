//! Notifications, after the originals' `NotificationService`,
//! `NotificationFactory` and `NotificationStatusService`: the stored
//! connections of kind Notification, which of them a given event reaches
//! (its `on*` flag, its tags against the series', the back-off of one that
//! keeps failing) and the three implementations this phase drives: Discord,
//! a generic webhook and a Plex Media Server. Other implementations are
//! stored and listed as the originals would, but send nothing.
//!
//! The runtime hands the events over as they happen: a grab, the import
//! pipeline's events (`EpisodeImportedEvent`, `DownloadCompletedEvent`,
//! `EpisodeFileDeletedEvent`, `SeriesRenamedEvent`,
//! `ManualInteractionRequiredEvent`) and the health verdicts. Plex updates
//! are queued per server and flushed on `ProcessQueue`, as the originals
//! do on `DownloadsProcessedEvent`, `DeleteCompletedEvent`,
//! `RenameCompletedEvent` and `HealthCheckCompleteEvent`.

pub mod discord;
pub mod plex;
pub mod webhook;

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use crate::db::Db;
use crate::decision::RemoteEpisode;
use crate::downloadclients::{DownloadClientDefinition, ItemClientInfo};
use crate::downloads::TrackedDownload;
use crate::health::{HealthCheck, HealthType};
use crate::indexers::status;
use crate::mediafiles::events::EpisodeFileRenamed;
use crate::mediafiles::local::{DeleteReason, DeletedEpisodeFile, GrabbedRelease};
use crate::mediafiles::ImportEvent;
use crate::model::{Episode, EpisodeFile, Provider, ProviderKind, QualityModel, Series, SeriesType, Tag};
use crate::store;
use crate::store::provider_status::ProviderStatus;
use crate::validation::ValidationFailure;

const KIND: ProviderKind = ProviderKind::Notification;

/// The implementations this phase sends through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Discord,
    Webhook,
    PlexServer,
}

impl Kind {
    pub fn from_implementation(name: &str) -> Option<Kind> {
        match name {
            "Discord" => Some(Kind::Discord),
            "Webhook" => Some(Kind::Webhook),
            "PlexServer" => Some(Kind::PlexServer),
            _ => None,
        }
    }
}

/// `NotificationDefinition`: a stored connection with its event flags.
#[derive(Debug, Clone, PartialEq)]
pub struct NotificationDefinition {
    pub id: i64,
    pub name: String,
    pub implementation: String,
    pub settings: Value,
    pub tags: Vec<i64>,
    pub on_grab: bool,
    pub on_download: bool,
    pub on_upgrade: bool,
    pub on_import_complete: bool,
    pub on_rename: bool,
    pub on_series_add: bool,
    pub on_series_delete: bool,
    pub on_episode_file_delete: bool,
    pub on_episode_file_delete_for_upgrade: bool,
    pub on_health_issue: bool,
    pub on_health_restored: bool,
    pub on_application_update: bool,
    pub on_manual_interaction_required: bool,
    pub include_health_warnings: bool,
}

/// The `on*` attributes a notification resource carries, in the originals'
/// order, with the default a missing one takes.
pub const FLAGS: &[&str] = &[
    "onGrab",
    "onDownload",
    "onUpgrade",
    "onImportComplete",
    "onRename",
    "onSeriesAdd",
    "onSeriesDelete",
    "onEpisodeFileDelete",
    "onEpisodeFileDeleteForUpgrade",
    "onHealthIssue",
    "onHealthRestored",
    "onApplicationUpdate",
    "onManualInteractionRequired",
    "includeHealthWarnings",
];

impl NotificationDefinition {
    pub fn from_provider(p: &Provider) -> NotificationDefinition {
        let flag = |name: &str| p.attributes[name].as_bool().unwrap_or(false);
        NotificationDefinition {
            id: p.id,
            name: p.name.clone(),
            implementation: p.implementation.clone(),
            settings: p.settings.clone(),
            tags: p.tags.clone(),
            on_grab: flag("onGrab"),
            on_download: flag("onDownload"),
            on_upgrade: flag("onUpgrade"),
            on_import_complete: flag("onImportComplete"),
            on_rename: flag("onRename"),
            on_series_add: flag("onSeriesAdd"),
            on_series_delete: flag("onSeriesDelete"),
            on_episode_file_delete: flag("onEpisodeFileDelete"),
            on_episode_file_delete_for_upgrade: flag("onEpisodeFileDeleteForUpgrade"),
            on_health_issue: flag("onHealthIssue"),
            on_health_restored: flag("onHealthRestored"),
            on_application_update: flag("onApplicationUpdate"),
            on_manual_interaction_required: flag("onManualInteractionRequired"),
            include_health_warnings: flag("includeHealthWarnings"),
        }
    }

    pub fn kind(&self) -> Option<Kind> {
        Kind::from_implementation(&self.implementation)
    }

    /// `NotificationDefinition.Enable`: any event flag set.
    pub fn enabled(&self) -> bool {
        self.on_grab
            || self.on_download
            || self.on_upgrade
            || self.on_import_complete
            || self.on_rename
            || self.on_series_add
            || self.on_series_delete
            || self.on_episode_file_delete
            || self.on_episode_file_delete_for_upgrade
            || self.on_health_issue
            || self.on_health_restored
            || self.on_application_update
            || self.on_manual_interaction_required
    }

    /// `definition.Settings.Validate()` for the implementations driven
    /// here; the others' settings are stored as given.
    pub fn validate_settings(&self) -> Vec<ValidationFailure> {
        match self.kind() {
            Some(Kind::Discord) => discord::Settings::from_value(&self.settings).validate(),
            Some(Kind::Webhook) => webhook::Settings::from_value(&self.settings).validate(),
            Some(Kind::PlexServer) => plex::Settings::from_value(&self.settings).validate(),
            None => Vec::new(),
        }
    }

    /// `ShouldHandleSeries`: no tags, or one in common with the series.
    fn handles(&self, series: &Series) -> bool {
        self.tags.is_empty() || self.tags.iter().any(|t| series.tags.contains(t))
    }

    /// `ShouldHandleHealthFailure`.
    fn handles_health(&self, check: &HealthCheck) -> bool {
        check.kind == HealthType::Error || (check.kind == HealthType::Warning && self.include_health_warnings)
    }
}

/// What a test can end in: failures the API lists, or an error the
/// originals let escape (a connection that was refused, a name that does
/// not resolve), answered with a 500.
#[derive(Debug)]
pub enum TestOutcome {
    Failures(Vec<ValidationFailure>),
    Error(String),
}

/// Who this instance says it is: `InstanceName` for the payloads,
/// `UrlBase` for the cover URLs, the machine name Discord signs embeds
/// with when no author is configured.
#[derive(Debug, Clone)]
pub struct Identity {
    pub instance_name: String,
    pub url_base: String,
    pub machine_name: String,
    /// `BuildInfo.AppName` and `BuildInfo.Version`, which Plex is told.
    pub app_name: String,
    pub app_version: String,
    /// Where the series covers are, for the `lastWrite` of their URLs.
    pub media_cover_dir: Option<std::path::PathBuf>,
}

impl Default for Identity {
    fn default() -> Identity {
        Identity {
            instance_name: "Sonarr".into(),
            url_base: String::new(),
            machine_name: machine_name(),
            app_name: "Sonarr".into(),
            app_version: "4.0.20.3014".into(),
            media_cover_dir: None,
        }
    }
}

static PINNED_MACHINE_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Pin the machine name Discord embeds carry; for the contract replay,
/// which compares against a recording made on another machine.
pub fn pin_machine_name(name: &str) {
    let _ = PINNED_MACHINE_NAME.set(name.to_string());
}

/// `Environment.MachineName`.
pub fn machine_name() -> String {
    if let Some(name) = PINNED_MACHINE_NAME.get() {
        return name.clone();
    }
    if let Ok(name) = std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")) {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

/// `GrabMessage`.
pub struct GrabMessage<'a> {
    pub series: &'a Series,
    pub episodes: &'a [Episode],
    pub remote: &'a RemoteEpisode,
    pub quality: QualityModel,
    pub download_client_name: &'a str,
    pub download_client_type: &'a str,
    pub download_id: Option<&'a str>,
}

/// `DownloadMessage`.
pub struct DownloadMessage<'a> {
    pub series: &'a Series,
    pub episodes: &'a [Episode],
    pub file: &'a EpisodeFile,
    pub old_files: &'a [DeletedEpisodeFile],
    pub source_path: &'a str,
    pub client: Option<&'a ItemClientInfo>,
    pub download_id: Option<&'a str>,
    pub release: Option<&'a GrabbedRelease>,
    pub custom_formats: &'a [crate::model::CustomFormat],
    pub custom_format_score: i64,
}

/// `ImportCompleteMessage`.
pub struct ImportCompleteMessage<'a> {
    pub series: &'a Series,
    pub episodes: &'a [Episode],
    pub files: &'a [EpisodeFile],
    pub client: Option<&'a ItemClientInfo>,
    pub download_id: Option<&'a str>,
    pub release: Option<&'a GrabbedRelease>,
    pub source_path: &'a str,
    pub source_title: Option<&'a str>,
    pub destination_path: String,
    pub release_group: Option<&'a str>,
    pub release_quality: QualityModel,
}

/// `EpisodeDeleteMessage`.
pub struct DeleteMessage<'a> {
    pub series: &'a Series,
    pub episodes: &'a [Episode],
    pub file: &'a EpisodeFile,
    pub reason: DeleteReason,
}

/// `ManualInteractionRequiredMessage`.
pub struct ManualInteractionMessage<'a> {
    pub series: Option<&'a Series>,
    pub episodes: &'a [Episode],
    pub remote: Option<&'a RemoteEpisode>,
    pub quality: Option<QualityModel>,
    pub tracked: &'a TrackedDownload,
    pub release: Option<&'a GrabbedRelease>,
}

/// What the payload builders read besides the message: the series' tag
/// labels, the application URL and the instance's identity.
pub struct Context<'a> {
    pub identity: &'a Identity,
    pub application_url: &'a str,
    pub tags: &'a [Tag],
    pub now: DateTime<Utc>,
}

impl Context<'_> {
    /// `GetTagLabels`: the series' tag labels, sorted.
    pub fn tag_labels(&self, series: &Series) -> Vec<String> {
        let mut labels: Vec<String> = self
            .tags
            .iter()
            .filter(|t| series.tags.contains(&t.id) && !t.label.trim().is_empty())
            .map(|t| t.label.clone())
            .collect();
        labels.sort();
        labels
    }
}

/// `NotificationService` plus the status service: dispatches events to the
/// stored connections and remembers which keep failing.
pub struct Notifier {
    db: Db,
    http: reqwest::Client,
    identity: Identity,
    /// `PlexServerService._versionCache`: the version seen per host, for
    /// two hours.
    plex_versions: Mutex<HashMap<String, (String, DateTime<Utc>)>>,
    /// `MediaServerUpdateQueue`: the series whose Plex section awaits a
    /// refresh, per host, in the order they came.
    plex_pending: Mutex<HashMap<String, Vec<Series>>>,
    /// The process start the status service's grace period counts from.
    started_at: DateTime<Utc>,
}

/// `NotificationStatusService`: five levels at most, no block within five
/// minutes of the first failure.
fn policy(started_at: DateTime<Utc>) -> status::Policy {
    status::Policy {
        max_level: 5,
        minimum_time_since_initial_failure: Duration::minutes(5),
        started_at: Some(started_at),
    }
}

impl Notifier {
    pub fn new(db: Db, identity: Identity, started_at: DateTime<Utc>) -> Notifier {
        Self::with_client(db, crate::http::shared().clone(), identity, started_at)
    }

    pub fn with_client(
        db: Db,
        http: reqwest::Client,
        identity: Identity,
        started_at: DateTime<Utc>,
    ) -> Notifier {
        Notifier {
            db,
            http,
            identity,
            plex_versions: Mutex::new(HashMap::new()),
            plex_pending: Mutex::new(HashMap::new()),
            started_at,
        }
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// The stored connections of kind Notification, by id.
    pub(crate) async fn providers(&self) -> anyhow::Result<Vec<Provider>> {
        self.db
            .call(|conn| store::providers::list_providers(conn, KIND))
            .await
    }

    /// `NotificationStatusService.GetBlockedProviders`: the ids in back-off
    /// at `now`.
    pub(crate) async fn blocked(&self, now: DateTime<Utc>) -> Vec<i64> {
        self.db
            .call(|conn| store::provider_status::provider_statuses(conn, KIND))
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, s)| s.is_disabled(now))
            .map(|(id, _)| id)
            .collect()
    }

    /// Every stored notification, by id.
    pub async fn definitions(&self) -> anyhow::Result<Vec<NotificationDefinition>> {
        Ok(self
            .providers()
            .await?
            .iter()
            .map(NotificationDefinition::from_provider)
            .collect())
    }

    /// `GetAvailableProviders` through `FilterBlockedNotifications`: the
    /// enabled ones not in back-off, when `filter_blocked`.
    async fn available(&self, now: DateTime<Utc>, filter_blocked: bool) -> Vec<NotificationDefinition> {
        let defs = match self.definitions().await {
            Ok(defs) => defs,
            Err(e) => {
                tracing::error!(error = %e, "failed to load the notifications");
                return Vec::new();
            }
        };
        let blocked: Vec<i64> = if filter_blocked {
            self.blocked(now).await
        } else {
            Vec::new()
        };
        defs.into_iter()
            .filter(|d| d.enabled())
            .filter(|d| {
                if blocked.contains(&d.id) {
                    tracing::debug!(notification = %d.name, "temporarily ignoring notification due to recent failures");
                    false
                } else {
                    true
                }
            })
            .collect()
    }

    pub(crate) async fn context_parts(&self) -> (String, Vec<Tag>) {
        self.db
            .call(|conn| {
                Ok((
                    store::config::config(conn, "ApplicationUrl")?.unwrap_or_default(),
                    store::catalog::list_tags(conn)?,
                ))
            })
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "failed to load the notification context");
                (String::new(), Vec::new())
            })
    }

    async fn status(&self, id: i64) -> Option<ProviderStatus> {
        self.db
            .call(move |conn| store::provider_status::provider_status(conn, KIND, id))
            .await
            .ok()
    }

    async fn store_status(&self, id: i64, status: ProviderStatus) {
        if let Err(e) = self
            .db
            .call(move |conn| store::provider_status::upsert_provider_status(conn, KIND, id, &status))
            .await
        {
            tracing::error!(notification = id, error = %e, "failed to store the notification status");
        }
    }

    pub async fn record_success(&self, id: i64) {
        if id <= 0 {
            return;
        }
        let Some(mut s) = self.status(id).await else {
            return;
        };
        if status::record_success(&mut s) {
            self.store_status(id, s).await;
        }
    }

    pub async fn record_failure(&self, id: i64, now: DateTime<Utc>) {
        if id <= 0 {
            return;
        }
        let Some(mut s) = self.status(id).await else {
            return;
        };
        status::record_failure_with(&mut s, now, Duration::zero(), true, &policy(self.started_at));
        self.store_status(id, s).await;
    }

    /// Send through one connection and record the outcome, as each of the
    /// originals' handlers does per notification.
    async fn deliver(
        &self,
        def: &NotificationDefinition,
        what: &str,
        now: DateTime<Utc>,
        result: Result<(), String>,
    ) {
        self.deliver_to(def.id, &def.name, what, now, result).await;
    }

    /// The same for a connection known by its id and name.
    pub(crate) async fn deliver_to(
        &self,
        id: i64,
        name: &str,
        what: &str,
        now: DateTime<Utc>,
        result: Result<(), String>,
    ) {
        match result {
            Ok(()) => self.record_success(id).await,
            Err(e) => {
                self.record_failure(id, now).await;
                tracing::warn!(notification = %name, error = %e, "unable to send {what} notification");
            }
        }
    }

    /// `Handle(EpisodeGrabbedEvent)`.
    pub async fn on_grab(
        &self,
        remote: &RemoteEpisode,
        client: &DownloadClientDefinition,
        download_id: Option<&str>,
        now: DateTime<Utc>,
    ) {
        let Some(series) = remote.series.as_ref() else {
            return;
        };
        let quality = remote
            .parsed
            .as_ref()
            .map(|p| p.quality.quality)
            .unwrap_or_default();
        let message = GrabMessage {
            series,
            episodes: &remote.episodes,
            remote,
            quality,
            download_client_name: &client.name,
            download_client_type: client.kind.display_name(),
            download_id,
        };
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_grab || !def.handles(series) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_grab(&self.http, &def, &message, &ctx).await,
                Some(Kind::Webhook) => webhook::on_grab(&self.http, &def, &message, &ctx).await,
                _ => Ok(()),
            };
            self.deliver(&def, "OnGrab", now, result).await;
        }
    }

    /// The import pipeline's events, in order, through the originals'
    /// synchronous handlers.
    pub async fn on_import_events(&self, events: &[ImportEvent], now: DateTime<Utc>) {
        for e in events {
            match e {
                ImportEvent::EpisodeImported(ev) => {
                    if !ev.new_download {
                        continue;
                    }
                    let Some(series) = ev.local.series.as_ref() else {
                        continue;
                    };
                    let message = DownloadMessage {
                        series,
                        episodes: &ev.local.episodes,
                        file: &ev.file,
                        old_files: &ev.old_files,
                        source_path: &ev.local.path,
                        client: ev.client.as_ref(),
                        download_id: ev.download_id.as_deref(),
                        release: ev.local.release.as_ref(),
                        custom_formats: &ev.local.custom_formats,
                        custom_format_score: ev.local.custom_format_score,
                    };
                    self.on_download(&message, now).await;
                }
                ImportEvent::DownloadCompleted(ev) => {
                    let Some(remote) = ev.tracked.remote.as_ref() else {
                        continue;
                    };
                    let Some(series) = remote.series.as_ref() else {
                        continue;
                    };
                    let parsed = remote.parsed.clone().unwrap_or_default();
                    let paths: Vec<String> = ev
                        .files
                        .iter()
                        .map(|f| crate::mediafiles::join(&series.path, &f.relative_path))
                        .collect();
                    let source_path = ev
                        .tracked
                        .import_item
                        .as_ref()
                        .or(Some(&ev.tracked.item))
                        .and_then(|i| i.output_path.clone())
                        .unwrap_or_default();
                    let message = ImportCompleteMessage {
                        series,
                        episodes: &remote.episodes,
                        files: &ev.files,
                        client: Some(&ev.tracked.item.client),
                        download_id: Some(ev.tracked.download_id()),
                        release: ev.release.as_ref(),
                        source_path: &source_path,
                        source_title: None,
                        destination_path: crate::mediafiles::longest_common_path(&paths),
                        release_group: parsed.release_group.as_deref(),
                        release_quality: parsed.quality.quality,
                    };
                    self.on_import_complete(&message, now).await;
                }
                ImportEvent::EpisodeFileDeleted(ev) => {
                    if ev.episodes.is_empty() {
                        tracing::trace!("skipping notification for deleted file without an episode");
                        continue;
                    }
                    let message = DeleteMessage {
                        series: &ev.series,
                        episodes: &ev.episodes,
                        file: &ev.file,
                        reason: ev.reason,
                    };
                    self.on_episode_file_delete(&message, now).await;
                }
                ImportEvent::SeriesRenamed { renamed, .. } => {
                    if let Some(first) = renamed.first() {
                        self.on_rename(&first.series, renamed, now).await;
                    }
                }
                ImportEvent::ManualInteractionRequired(ev) => {
                    let remote = ev.tracked.remote.as_ref();
                    let series = remote.and_then(|r| r.series.as_ref());
                    let episodes: &[Episode] = remote.map(|r| r.episodes.as_slice()).unwrap_or(&[]);
                    let quality = remote.and_then(|r| r.parsed.as_ref()).map(|p| p.quality.quality);
                    let message = ManualInteractionMessage {
                        series,
                        episodes,
                        remote,
                        quality,
                        tracked: &ev.tracked,
                        release: ev.release.as_ref(),
                    };
                    self.on_manual_interaction_required(&message, now).await;
                }
                ImportEvent::DeleteCompleted => self.process_queue(now).await,
                ImportEvent::EpisodeImportFailed(_)
                | ImportEvent::EpisodeFileRenamed(_)
                | ImportEvent::SeriesScanned { .. }
                | ImportEvent::SeriesScanSkipped { .. }
                | ImportEvent::RescanRequested { .. } => {}
            }
        }
    }

    /// `Handle(SeriesAddCompletedEvent)`: the series once it was added,
    /// refreshed and scanned.
    pub async fn on_series_add(&self, series: &Series, now: DateTime<Utc>) {
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_series_add || !def.handles(series) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_series_add(&self.http, &def, series, &ctx).await,
                Some(Kind::Webhook) => webhook::on_series_add(&self.http, &def, series, &ctx).await,
                _ => Ok(()),
            };
            self.deliver(&def, "OnSeriesAdd", now, result).await;
        }
    }

    /// `Handle(SeriesDeletedEvent)`: one message per series; Plex updates
    /// its library when the files went.
    pub async fn on_series_delete(&self, series: &[Series], deleted_files: bool, now: DateTime<Utc>) {
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for one in series {
            for def in self.available(now, true).await {
                if !def.on_series_delete || !def.handles(one) {
                    continue;
                }
                let result = match def.kind() {
                    Some(Kind::Discord) => {
                        discord::on_series_delete(&self.http, &def, one, deleted_files, &ctx).await
                    }
                    Some(Kind::Webhook) => {
                        webhook::on_series_delete(&self.http, &def, one, deleted_files, &ctx).await
                    }
                    Some(Kind::PlexServer) => {
                        if deleted_files {
                            self.plex_update_if_enabled(&def, one);
                        }
                        Ok(())
                    }
                    None => Ok(()),
                };
                self.deliver(&def, "OnSeriesDelete", now, result).await;
            }
        }
    }

    /// `Handle(EpisodeImportedEvent)`.
    async fn on_download(&self, message: &DownloadMessage<'_>, now: DateTime<Utc>) {
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_download || !def.handles(message.series) {
                continue;
            }
            if !message.old_files.is_empty() && !def.on_upgrade {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_download(&self.http, &def, message, &ctx).await,
                Some(Kind::Webhook) => webhook::on_download(&self.http, &def, message, &ctx).await,
                Some(Kind::PlexServer) => {
                    self.plex_update_if_enabled(&def, message.series);
                    Ok(())
                }
                None => Ok(()),
            };
            self.deliver(&def, "OnDownload", now, result).await;
        }
    }

    /// `Handle(DownloadCompletedEvent)` and
    /// `Handle(UntrackedDownloadCompletedEvent)`.
    pub async fn on_import_complete(&self, message: &ImportCompleteMessage<'_>, now: DateTime<Utc>) {
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_import_complete || !def.handles(message.series) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_import_complete(&self.http, &def, message, &ctx).await,
                Some(Kind::Webhook) => webhook::on_import_complete(&self.http, &def, message, &ctx).await,
                Some(Kind::PlexServer) => {
                    self.plex_update_if_enabled(&def, message.series);
                    Ok(())
                }
                None => Ok(()),
            };
            self.deliver(&def, "OnImportComplete", now, result).await;
        }
    }

    /// `Handle(EpisodeFileDeletedEvent)`.
    async fn on_episode_file_delete(&self, message: &DeleteMessage<'_>, now: DateTime<Utc>) {
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_episode_file_delete {
                continue;
            }
            if message.reason == DeleteReason::Upgrade && !def.on_episode_file_delete_for_upgrade {
                continue;
            }
            if !def.handles(message.series) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_episode_file_delete(&self.http, &def, message, &ctx).await,
                Some(Kind::Webhook) => webhook::on_episode_file_delete(&self.http, &def, message, &ctx).await,
                Some(Kind::PlexServer) => {
                    self.plex_update_if_enabled(&def, message.series);
                    Ok(())
                }
                None => Ok(()),
            };
            self.deliver(&def, "OnEpisodeFileDelete", now, result).await;
        }
    }

    /// `Handle(SeriesRenamedEvent)`.
    async fn on_rename(&self, series: &Series, renamed: &[EpisodeFileRenamed], now: DateTime<Utc>) {
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_rename || !def.handles(series) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_rename(&self.http, &def, series).await,
                Some(Kind::Webhook) => webhook::on_rename(&self.http, &def, series, renamed, &ctx).await,
                Some(Kind::PlexServer) => {
                    self.plex_update_if_enabled(&def, series);
                    Ok(())
                }
                None => Ok(()),
            };
            self.deliver(&def, "OnRename", now, result).await;
        }
    }

    /// `Handle(ManualInteractionRequiredEvent)`.
    async fn on_manual_interaction_required(
        &self,
        message: &ManualInteractionMessage<'_>,
        now: DateTime<Utc>,
    ) {
        // The originals build the message from the series or, failing
        // that, the download's title, and send nothing when both are blank.
        if message.series.is_none() && message.tracked.item.title.trim().is_empty() {
            return;
        }
        let (application_url, tags) = self.context_parts().await;
        let ctx = Context {
            identity: &self.identity,
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_manual_interaction_required {
                continue;
            }
            // `ShouldHandleSeries(definition, message.Episode.Series)`: the
            // originals dereference the remote episode here and throw
            // without one, which the handler's catch turns into a failure.
            let Some(series) = message.series else {
                self.deliver(
                    &def,
                    "OnManualInteractionRequired",
                    now,
                    Err("Object reference not set to an instance of an object.".into()),
                )
                .await;
                continue;
            };
            if !def.handles(series) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => {
                    discord::on_manual_interaction_required(&self.http, &def, message, &ctx).await
                }
                Some(Kind::Webhook) => {
                    webhook::on_manual_interaction_required(&self.http, &def, message, &ctx).await
                }
                _ => Ok(()),
            };
            self.deliver(&def, "OnManualInteractionRequired", now, result)
                .await;
        }
    }

    /// `Handle(HealthCheckFailedEvent)`, outside the startup grace period.
    pub async fn on_health_issue(&self, check: &HealthCheck, now: DateTime<Utc>) {
        for def in self.available(now, true).await {
            if !def.on_health_issue || !def.handles_health(check) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => {
                    discord::on_health_issue(&self.http, &def, check, &self.identity, now).await
                }
                Some(Kind::Webhook) => {
                    webhook::on_health(&self.http, &def, check, &self.identity, false).await
                }
                _ => Ok(()),
            };
            self.deliver(&def, "OnHealthIssue", now, result).await;
        }
    }

    /// `Handle(HealthCheckRestoredEvent)`, outside the startup grace period.
    pub async fn on_health_restored(&self, previous: &HealthCheck, now: DateTime<Utc>) {
        for def in self.available(now, true).await {
            if !def.on_health_restored || !def.handles_health(previous) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => {
                    discord::on_health_restored(&self.http, &def, previous, &self.identity, now).await
                }
                Some(Kind::Webhook) => {
                    webhook::on_health(&self.http, &def, previous, &self.identity, true).await
                }
                _ => Ok(()),
            };
            self.deliver(&def, "OnHealthRestored", now, result).await;
        }
    }

    /// `PlexServer.UpdateIfEnabled`: the series joins the server's queue.
    fn plex_update_if_enabled(&self, def: &NotificationDefinition, series: &Series) {
        let settings = plex::Settings::from_value(&def.settings);
        if !settings.update_library {
            return;
        }
        tracing::debug!(series = %series.title, "scheduling library update");
        let mut pending = self.plex_pending.lock().unwrap();
        let queue = pending.entry(settings.host.clone()).or_default();
        if let Some(existing) = queue.iter_mut().find(|s| s.id == series.id) {
            *existing = series.clone();
        } else {
            queue.push(series.clone());
        }
    }

    /// `NotificationService.ProcessQueue`: every enabled connection flushes
    /// what it queued; only Plex queues anything.
    pub async fn process_queue(&self, now: DateTime<Utc>) {
        for def in self.available(now, false).await {
            if def.kind() != Some(Kind::PlexServer) {
                continue;
            }
            let settings = plex::Settings::from_value(&def.settings);
            let items: Vec<Series> = {
                let mut pending = self.plex_pending.lock().unwrap();
                pending.remove(&settings.host).unwrap_or_default()
            };
            if items.is_empty() || !settings.update_library {
                continue;
            }
            tracing::debug!(count = items.len(), "performing library update");
            if let Err(e) = plex::update_library(self, &settings, &items, now).await {
                tracing::warn!(host = %settings.host, error = %e, "failed to update Plex host");
            }
        }
    }

    /// `_providerFactory.Test(definition)`: the connection test; the status
    /// of a stored connection is not touched by a test, as the originals'
    /// notification factory does not record one.
    pub async fn test(&self, def: &NotificationDefinition, now: DateTime<Utc>) -> TestOutcome {
        match def.kind() {
            Some(Kind::Discord) => discord::test(&self.http, def, &self.identity, now).await,
            Some(Kind::Webhook) => {
                let (application_url, tags) = self.context_parts().await;
                let ctx = Context {
                    identity: &self.identity,
                    application_url: &application_url,
                    tags: &tags,
                    now,
                };
                webhook::test(&self.http, def, &ctx).await
            }
            Some(Kind::PlexServer) => plex::test(self, &plex::Settings::from_value(&def.settings), now).await,
            None => TestOutcome::Failures(vec![ValidationFailure::error(
                "",
                &format!(
                    "Test was aborted due to an error: Unarr does not drive {} notifications yet",
                    def.implementation
                ),
            )]),
        }
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub(crate) fn db(&self) -> &Db {
        &self.db
    }

    pub(crate) fn plex_versions(&self) -> &Mutex<HashMap<String, (String, DateTime<Utc>)>> {
        &self.plex_versions
    }
}

/// `GetTitle`: `{Series} - {season}x{ep:02}... - {titles joined " + "}`,
/// the air date and title for a daily series, cut at 256 characters.
pub fn title(series: &Series, episodes: &[Episode]) -> String {
    if series.series_type == SeriesType::Daily {
        if let Some(e) = episodes.first() {
            return format!(
                "{} - {} - {}",
                series.title,
                e.air_date.map(|d| d.to_string()).unwrap_or_default(),
                e.title.clone().unwrap_or_default()
            );
        }
    }
    let numbers: String = episodes
        .iter()
        .map(|e| format!("x{:02}", e.episode_number))
        .collect();
    let titles: Vec<String> = episodes
        .iter()
        .map(|e| e.title.clone().unwrap_or_default())
        .collect();
    let season = episodes.first().map(|e| e.season_number).unwrap_or(0);
    let title = format!(
        "{} - {}{} - {}",
        series.title,
        season,
        numbers,
        titles.join(" + ")
    );
    if title.chars().count() > 256 {
        format!("{}...", title.chars().take(253).collect::<String>())
    } else {
        title
    }
}

/// `BytesToString`: one decimal, .NET's rounding to even, the unit of the
/// largest power of 1024 that fits (`117.2 KB`, `1.3 GB`, `3 GB`).
pub fn bytes_to_string(bytes: i64) -> String {
    const SUFFIXES: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let abs = bytes.unsigned_abs() as f64;
    let place = (abs.ln() / 1024f64.ln()).floor() as usize;
    let place = place.min(SUFFIXES.len() - 1);
    let num = round_half_even(abs / 1024f64.powi(place as i32), 1);
    let num = if bytes < 0 { -num } else { num };
    format!("{} {}", format_number(num), SUFFIXES[place])
}

/// `Math.Round(x, digits)` with `MidpointRounding.ToEven`.
fn round_half_even(x: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    let scaled = x * scale;
    let floor = scaled.floor();
    let diff = scaled - floor;
    let rounded = if (diff - 0.5).abs() < 1e-9 {
        if floor % 2.0 == 0.0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        scaled.round()
    };
    rounded / scale
}

/// A double as .NET's default `ToString` writes it: no trailing `.0`.
pub fn format_number(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// `IsValidUrl` as a settings rule: `Invalid Url: '{url}'` unless the
/// value is an absolute, well-formed URL without surrounding spaces.
pub fn valid_url(property: &str, url: &str) -> Option<ValidationFailure> {
    let ok = !url.trim().is_empty()
        && !url.starts_with(' ')
        && !url.ends_with(' ')
        && reqwest::Url::parse(url).is_ok_and(|u| u.host_str().is_some());
    (!ok).then(|| {
        ValidationFailure::setting(property, &format!("Invalid Url: '{url}'"), serde_json::json!(url))
    })
}

/// `Quality.Name` of a quality model.
pub fn quality_name(q: &QualityModel) -> &'static str {
    crate::quality::Quality::by_id(q.quality_id).name
}

/// The time in the originals' `DateTime.UtcNow.ToString("O")` shape.
pub fn timestamp(now: DateTime<Utc>) -> String {
    let fraction = now.timestamp_subsec_nanos() / 100;
    format!("{}.{:07}Z", now.format("%Y-%m-%dT%H:%M:%S"), fraction)
}

/// The error a failed HTTP call reads as: the originals' `HttpException`
/// message for an error status, the transport's for anything else.
pub(crate) enum SendError {
    /// The target answered with an error status.
    Status(u16, String),
    /// The call never got an answer: the originals let these escape.
    Transport(String),
}

pub(crate) async fn send_json(
    http: &reqwest::Client,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, SendError> {
    let _ = http;
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            if status.is_client_error() || status.is_server_error() {
                let reason = status.canonical_reason().unwrap_or("");
                return Err(SendError::Status(
                    status.as_u16(),
                    format!("HTTP request failed: [{}:{}]", status.as_u16(), reason),
                ));
            }
            Ok(response)
        }
        Err(e) => Err(SendError::Transport(transport_message(&e))),
    }
}

/// The message .NET gives a connection failure, as far as it can be told
/// from the Rust side: `Connection refused (host:port)` and the like.
fn transport_message(e: &reqwest::Error) -> String {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    let mut innermost = e.to_string();
    while let Some(s) = source {
        innermost = s.to_string();
        source = s.source();
    }
    let target = e
        .url()
        .map(|u| {
            format!(
                "{}:{}",
                u.host_str().unwrap_or(""),
                u.port_or_known_default().unwrap_or(0)
            )
        })
        .unwrap_or_default();
    if innermost.contains("refused") {
        format!("Connection refused ({target})")
    } else if innermost.contains("resolve")
        || innermost.contains("dns")
        || innermost.contains("Name or service")
    {
        format!("Name or service not known ({target})")
    } else if e.is_timeout() {
        format!("The request was canceled due to the configured HttpClient.Timeout elapsing ({target})")
    } else {
        format!("{innermost} ({target})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_the_originals() {
        assert_eq!(bytes_to_string(119980), "117.2 KB");
        assert_eq!(bytes_to_string(1395864371), "1.3 GB");
        assert_eq!(bytes_to_string(3221225472), "3 GB");
        assert_eq!(bytes_to_string(1106640), "1.1 MB");
        assert_eq!(bytes_to_string(2684354560), "2.5 GB");
        assert_eq!(bytes_to_string(0), "0 B");
        assert_eq!(bytes_to_string(1536), "1.5 KB");
        // 2.25 rounds to even: 2.2.
        assert_eq!(bytes_to_string(2304), "2.2 KB");
    }

    #[test]
    fn timestamps_carry_seven_digits() {
        let t = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 11, 8, 20, 57, 28).unwrap()
            + Duration::nanoseconds(539_056_100);
        assert_eq!(timestamp(t), "2026-11-08T20:57:28.5390561Z");
    }
}
