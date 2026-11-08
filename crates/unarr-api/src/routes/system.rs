//! `system/status`, `health` and `/initialize.json`.

use axum::extract::{Path, State};
use axum::http::{header, Uri};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use unarr_core::model::ScheduledTask;
use unarr_core::store;
use unarr_core::store::paging::Paging;
use unarr_core::time;

use crate::compat;
use crate::error::{found, ApiResult};
use crate::json;
use crate::query::Params;
use crate::resources::PagingResource;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemResource {
    pub app_name: &'static str,
    pub instance_name: String,
    pub version: &'static str,
    pub build_time: &'static str,
    pub is_debug: bool,
    pub is_production: bool,
    pub is_admin: bool,
    pub is_user_interactive: bool,
    pub startup_path: String,
    pub app_data: String,
    pub os_name: String,
    pub os_version: String,
    pub is_net_core: bool,
    pub is_linux: bool,
    pub is_osx: bool,
    pub is_windows: bool,
    pub is_docker: bool,
    pub mode: &'static str,
    pub branch: String,
    pub authentication: &'static str,
    pub migration_version: i64,
    pub url_base: String,
    pub runtime_version: &'static str,
    pub runtime_name: &'static str,
    #[serde(with = "json::ts")]
    pub start_time: DateTime<Utc>,
    pub package_version: &'static str,
    pub package_author: &'static str,
    pub package_update_mechanism: &'static str,
    pub database_version: &'static str,
    pub database_type: &'static str,
}

pub async fn status(State(state): State<AppState>) -> ApiResult<Json<SystemResource>> {
    let personality = state.personality();
    let settings = state.settings();
    let runtime = state.runtime();
    let migration = state
        .db()
        .call(|conn| store::config::meta(conn, "source.migration"))
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| compat::emulated_migration(personality));

    Ok(Json(SystemResource {
        app_name: personality.app_name(),
        instance_name: settings.instance_name.clone(),
        version: compat::emulated_version(personality),
        build_time: compat::emulated_build_time(personality),
        is_debug: cfg!(debug_assertions),
        is_production: true,
        is_admin: false,
        is_user_interactive: runtime.is_user_interactive,
        startup_path: runtime.startup_path.display().to_string(),
        app_data: runtime.app_data.display().to_string(),
        os_name: runtime.os_name.clone(),
        os_version: runtime.os_version.clone(),
        // The originals always report .NET here; the UI shows it in About.
        is_net_core: true,
        is_linux: cfg!(target_os = "linux"),
        is_osx: cfg!(target_os = "macos"),
        is_windows: cfg!(target_os = "windows"),
        is_docker: runtime.is_docker,
        mode: if runtime.is_user_interactive {
            "console"
        } else {
            "service"
        },
        branch: settings.host.branch.clone(),
        authentication: settings.host.authentication_method.api_name(),
        migration_version: migration,
        url_base: settings.url_base.clone(),
        runtime_version: env!("CARGO_PKG_RUST_VERSION"),
        runtime_name: "Rust",
        start_time: runtime.start_time,
        package_version: compat::UNARR_VERSION,
        package_author: compat::UNARR_PACKAGE_AUTHOR,
        package_update_mechanism: "external",
        database_version: rusqlite::version(),
        database_type: "sqLite",
    }))
}

/// `HealthResource`: the verdicts that stand, as the runtime keeps them;
/// none before it runs, which is also what the originals answer when
/// everything is fine.
pub async fn health(State(state): State<AppState>) -> Json<Vec<Value>> {
    let checks = state.commands().map(|r| r.health()).unwrap_or_default();
    Json(checks.iter().map(health_resource).collect())
}

pub fn health_resource(h: &unarr_core::health::HealthCheck) -> Value {
    json!({
        "source": h.source,
        "type": h.kind.as_str(),
        "message": h.message,
        "wikiUrl": h.wiki_url,
    })
}

/// The bootstrap document the web UI fetches before anything else.
pub async fn initialize_json(State(state): State<AppState>) -> impl IntoResponse {
    let personality = state.personality();
    let settings = state.settings();
    let version = compat::emulated_version(personality);
    let body = serde_json::json!({
        "apiRoot": format!("{}{}", settings.url_base, personality.api_prefix()),
        "apiKey": settings.host.api_key,
        "release": format!("{version}-{}", settings.host.branch),
        "version": version,
        "instanceName": settings.instance_name,
        "theme": settings.host.theme,
        "branch": settings.host.branch.to_lowercase(),
        // Never report to the originals' Sentry: that is their telemetry.
        "analytics": false,
        "userHash": user_hash(&settings.host.api_key),
        "urlBase": settings.url_base,
        "isProduction": true,
    });
    ([(header::CONTENT_TYPE, "application/json")], body.to_string())
}

/// Eight hex characters derived from the installation, like the originals'
/// anonymous token. Not cryptographic; it only has to be stable.
fn user_hash(seed: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in seed.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

// --- scheduled tasks

/// `RefreshSeriesCommand` -> `Refresh Series`, as the originals split names.
pub fn split_camel_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A .NET `TimeSpan` string: `[d.]hh:mm:ss.fffffff`.
fn timespan(d: Duration) -> String {
    let secs = d.num_seconds().max(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let frac = (d.num_nanoseconds().unwrap_or(0).max(0) % 1_000_000_000) / 100;
    if days > 0 {
        format!("{days}.{h:02}:{m:02}:{s:02}.{frac:07}")
    } else {
        format!("{h:02}:{m:02}:{s:02}.{frac:07}")
    }
}

fn task_resource(t: &ScheduledTask) -> Value {
    let task_name = t
        .type_name
        .rsplit('.')
        .next()
        .unwrap_or(&t.type_name)
        .trim_end_matches("Command")
        .to_string();
    let epoch = time::parse_utc("0001-01-01T00:00:00Z").unwrap_or_default();
    let last_execution = t.last_execution.unwrap_or(epoch);
    let last_start = t.last_start_time.unwrap_or(epoch);
    json!({
        "id": t.id,
        "name": split_camel_case(&task_name),
        "taskName": task_name,
        "interval": t.interval_minutes,
        "lastExecution": time::format_utc(last_execution),
        "lastStartTime": time::format_utc(last_start),
        "nextExecution": time::format_utc(last_execution + Duration::minutes(t.interval_minutes)),
        "lastDuration": timespan(last_execution - last_start),
    })
}

pub async fn tasks(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let mut all = state
        .db()
        .call(|c| store::providers::list_scheduled_tasks(c))
        .await?;
    let mut out: Vec<Value> = all.iter_mut().map(|t| task_resource(t)).collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(Json(out))
}

pub async fn task(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = state
        .db()
        .call(|c| store::providers::list_scheduled_tasks(c))
        .await?;
    Ok(Json(found(all.iter().find(|t| t.id == id).map(task_resource))?))
}

// --- logs

pub async fn log(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<PagingResource<Value>>> {
    let p = Params::from_uri(&uri);
    let paging = Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        &["time", "level", "logger", "message", "id"],
        "time",
    );
    // Levels the originals accept, each including everything more severe.
    let max_level = match p.get("level").map(|l| l.to_ascii_lowercase()).as_deref() {
        Some("fatal") => 0,
        Some("error") => 1,
        Some("warn") => 2,
        Some("info") => 3,
        Some("debug") => 4,
        _ => 5,
    };
    let rank = |level: &str| match level {
        "fatal" => 0,
        "error" => 1,
        "warn" => 2,
        "info" => 3,
        "debug" => 4,
        _ => 5,
    };
    let mut entries: Vec<_> = state
        .logs()
        .entries()
        .into_iter()
        .filter(|e| rank(e.level) <= max_level)
        .collect();
    match paging.sort_key.as_str() {
        "level" => entries.sort_by_key(|e| rank(e.level)),
        "logger" => entries.sort_by(|a, b| a.logger.cmp(&b.logger)),
        "message" => entries.sort_by(|a, b| a.message.cmp(&b.message)),
        // Newest first is the buffer's order; `time` and `id` agree with it.
        _ => entries.sort_by_key(|e| std::cmp::Reverse(e.id)),
    }
    if !paging.descending {
        entries.reverse();
    }
    let total = entries.len() as i64;
    let records = paging
        .slice(&entries)
        .iter()
        .map(|e| {
            let mut v = json!({
                "id": e.id,
                "time": time::format_utc(e.time),
                "level": e.level,
                "logger": e.logger,
                "message": e.message,
            });
            if let Some(ex) = &e.exception {
                v["exception"] = json!(ex);
            }
            v
        })
        .collect();
    Ok(Json(PagingResource::new(&paging, total, records)))
}

// --- updates

/// The originals list their recent releases here. Unarr is not updated by
/// this mechanism, so it reports the one release it mirrors as installed.
pub async fn update(State(state): State<AppState>) -> Json<Vec<Value>> {
    let personality = state.personality();
    let version = compat::emulated_version(personality);
    Json(vec![json!({
        "version": version,
        "branch": state.settings().host.branch,
        "releaseDate": compat::emulated_build_time(personality),
        "fileName": format!("unarrd {}", compat::UNARR_VERSION),
        "url": "https://github.com/bateman/unarr/releases",
        "installed": true,
        "installedOn": time::format_utc(state.runtime().start_time),
        "installable": false,
        "latest": true,
        "changes": {
            "new": [format!("Unarr {} mirroring the {} {} API", compat::UNARR_VERSION, personality.app_name(), version)],
            "fixed": [],
        },
        "hash": "",
    })])
}

// --- localization

const LOCALIZATION_EN: &str = include_str!("../../schemas/sonarr/localization-en.json");

/// The UI strings of the mirrored release (English).
pub async fn localization() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        format!("{{\"strings\":{}}}", LOCALIZATION_EN.trim()),
    )
}

pub async fn localization_language() -> Json<Value> {
    Json(json!({"identifier": "en"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_names_split_like_the_originals() {
        assert_eq!(
            split_camel_case("ApplicationUpdateCheck"),
            "Application Update Check"
        );
        assert_eq!(split_camel_case("RssSync"), "Rss Sync");
        assert_eq!(timespan(Duration::seconds(3661)), "01:01:01.0000000");
        assert_eq!(timespan(Duration::seconds(90_000)), "1.01:00:00.0000000");
    }
}
