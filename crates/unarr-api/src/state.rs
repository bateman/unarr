//! Shared state handed to every handler.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use unarr_core::commands::{Event, Runtime};
use unarr_core::decision::RemoteEpisode;
use unarr_core::downloadclients::DownloadClientService;
use unarr_core::downloads::Downloads;

use chrono::{DateTime, Utc};
use serde_json::json;
use unarr_core::config::HostConfig;
use unarr_core::{Db, Personality};

use crate::logbuf::LogBuffer;
use crate::routes::signalr::Hub;

/// Host settings as the API reports them, derived from `config.xml`.
#[derive(Debug, Clone)]
pub struct Settings {
    pub host: HostConfig,
    /// Instance name after the originals' validation rule.
    pub instance_name: String,
    /// Normalised URL base: empty or `/prefix`.
    pub url_base: String,
    /// The hosts requests may name, from `AllowedHosts`.
    pub allowed_hosts: crate::hosts::AllowedHosts,
}

impl Settings {
    pub fn from_host_config(host: HostConfig, personality: Personality) -> Settings {
        let allowed_hosts = crate::hosts::AllowedHosts::parse(&host.allowed_hosts);
        if allowed_hosts.allows_everything() {
            tracing::info!("AllowedHosts is not configured, accepting requests for any host");
        } else {
            tracing::info!(?allowed_hosts, "accepting requests for the configured hosts only");
        }
        Settings {
            instance_name: host.effective_instance_name(personality.app_name()),
            url_base: host.normalized_url_base(),
            allowed_hosts,
            host,
        }
    }
}

/// Facts about the running process, for `system/status`.
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    pub start_time: DateTime<Utc>,
    /// Directory holding Unarr's databases and `config.xml`.
    pub app_data: PathBuf,
    /// Directory the executable runs from.
    pub startup_path: PathBuf,
    /// Where series images live (`<dir>/<seriesId>/poster.jpg`...).
    pub media_cover_dir: PathBuf,
    pub os_name: String,
    pub os_version: String,
    pub is_docker: bool,
    pub is_user_interactive: bool,
}

impl RuntimeInfo {
    /// Probe the current process and host.
    pub fn detect(app_data: PathBuf) -> RuntimeInfo {
        let (os_name, os_version) = os_release();
        RuntimeInfo {
            start_time: Utc::now(),
            startup_path: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from(".")),
            media_cover_dir: app_data.join("MediaCover"),
            app_data,
            os_name,
            os_version,
            is_docker: std::path::Path::new("/.dockerenv").exists(),
            is_user_interactive: std::io::IsTerminal::is_terminal(&std::io::stdin()),
        }
    }
}

/// `ID` and `VERSION_ID` from `/etc/os-release`, as the originals report
/// `osName` and `osVersion` on Linux.
fn os_release() -> (String, String) {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let name = field("ID");
    let version = field("VERSION_ID");
    if name.is_empty() {
        (std::env::consts::OS.to_string(), String::new())
    } else {
        (name, version)
    }
}

struct Inner {
    personality: Personality,
    db: Db,
    settings: Settings,
    runtime: RuntimeInfo,
    logs: LogBuffer,
    hub: Hub,
    /// When set, the moment every time-dependent answer is computed for.
    /// Contract tests pin it to the instant each golden was recorded. Shared
    /// with the command runtime, whose clock reads it.
    fixed_now: Arc<Mutex<Option<DateTime<Utc>>>>,
    /// `ReleaseController`'s cache: the releases a search listed, by
    /// `{indexerId}_{guid}`, for half an hour, so a grab can find them.
    releases: Mutex<HashMap<String, (RemoteEpisode, DateTime<Utc>)>>,
    /// Grabs, tracked downloads and the queue.
    downloads: Arc<Downloads>,
    /// The command queue, its workers and the scheduler, once started.
    commands: OnceLock<Arc<Runtime>>,
    /// `ReleasePushController.PushLock`: one push decided and processed
    /// at a time, so two pushes of one episode do not both grab.
    push_lock: tokio::sync::Mutex<()>,
}

/// `TimeSpan.FromMinutes(30)`.
const RELEASE_CACHE_TTL: chrono::Duration = chrono::Duration::minutes(30);

/// Cheaply cloneable handle to everything a handler needs.
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    pub fn new(
        personality: Personality,
        db: Db,
        settings: Settings,
        runtime: RuntimeInfo,
        logs: LogBuffer,
    ) -> AppState {
        let downloads = Arc::new(Downloads::new(db.clone(), DownloadClientService::new(db.clone())));
        AppState(Arc::new(Inner {
            personality,
            downloads,
            db,
            settings,
            runtime,
            logs,
            hub: Hub::new(),
            fixed_now: Arc::new(Mutex::new(None)),
            releases: Mutex::new(HashMap::new()),
            commands: OnceLock::new(),
            push_lock: tokio::sync::Mutex::new(()),
        }))
    }

    /// Start the command runtime: the queue, its workers, the scheduler,
    /// and the relay of its events to SignalR. Needs a Tokio runtime; call
    /// once.
    pub async fn start_runtime(&self) -> anyhow::Result<()> {
        if self.0.commands.get().is_some() {
            return Ok(());
        }
        let fixed = self.0.fixed_now.clone();
        let clock: unarr_core::commands::executor::Clock =
            Arc::new(move || fixed.lock().unwrap().unwrap_or_else(Utc::now));
        let identity = unarr_core::notifications::Identity {
            instance_name: self.settings().instance_name.clone(),
            url_base: self.settings().url_base.clone(),
            app_name: self.personality().app_name().to_string(),
            app_version: crate::compat::emulated_version(self.personality()).to_string(),
            ..unarr_core::notifications::Identity::default()
        };
        let runtime = Runtime::new(self.db().clone(), self.downloads().clone(), clock, identity).await?;
        runtime.start();
        let mut events = runtime.subscribe();
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(Event::CommandUpdated(c)) => state.hub().broadcast(
                        "command",
                        json!({"action": "updated", "resource": crate::routes::command::command_resource(&c)}),
                    ),
                    Ok(Event::QueueUpdated) => crate::routes::queue::broadcast_queue(&state),
                    Ok(Event::HealthChanged) => state.hub().broadcast("health", json!({"action": "sync"})),
                    Ok(Event::Import(e)) => crate::relay::relay_import(&state, &e).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let _ = self.0.commands.set(runtime);
        Ok(())
    }

    /// A provider changed: the health checks that follow one, in the
    /// background, when the runtime is up.
    pub fn providers_changed(&self) {
        if let Ok(runtime) = self.commands() {
            runtime.check_health_later(unarr_core::health::CheckSet::Providers);
        }
    }

    /// The command runtime; an error before `start_runtime`.
    pub fn commands(&self) -> anyhow::Result<&Arc<Runtime>> {
        self.0
            .commands
            .get()
            .ok_or_else(|| anyhow::anyhow!("the command runtime is not running"))
    }

    /// Remember a release a search listed.
    pub fn cache_release(&self, key: String, remote: RemoteEpisode) {
        let now = self.now();
        let mut cache = self.0.releases.lock().unwrap();
        cache.retain(|_, (_, at)| now - *at < RELEASE_CACHE_TTL);
        cache.insert(key, (remote, now));
    }

    /// A release a recent search listed.
    pub fn cached_release(&self, key: &str) -> Option<RemoteEpisode> {
        let now = self.now();
        let cache = self.0.releases.lock().unwrap();
        cache
            .get(key)
            .filter(|(_, at)| now - *at < RELEASE_CACHE_TTL)
            .map(|(r, _)| r.clone())
    }

    pub fn push_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.0.push_lock
    }

    pub fn downloads(&self) -> &Arc<Downloads> {
        &self.0.downloads
    }

    /// The current time, or the pinned one.
    pub fn now(&self) -> DateTime<Utc> {
        self.0.fixed_now.lock().unwrap().unwrap_or_else(Utc::now)
    }

    /// Pin (or unpin) the clock. For tests.
    pub fn set_now(&self, at: Option<DateTime<Utc>>) {
        *self.0.fixed_now.lock().unwrap() = at;
    }

    /// The SignalR fan-out to connected UIs.
    pub fn hub(&self) -> &Hub {
        &self.0.hub
    }

    pub fn logs(&self) -> &LogBuffer {
        &self.0.logs
    }

    pub fn personality(&self) -> Personality {
        self.0.personality
    }

    pub fn db(&self) -> &Db {
        &self.0.db
    }

    pub fn settings(&self) -> &Settings {
        &self.0.settings
    }

    pub fn runtime(&self) -> &RuntimeInfo {
        &self.0.runtime
    }
}
