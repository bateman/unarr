//! Shared state handed to every handler.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
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
    /// Contract tests pin it to the instant each golden was recorded.
    fixed_now: Mutex<Option<DateTime<Utc>>>,
}

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
        AppState(Arc::new(Inner {
            personality,
            db,
            settings,
            runtime,
            logs,
            hub: Hub::new(),
            fixed_now: Mutex::new(None),
        }))
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
