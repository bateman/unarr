//! Contract tests of the Radarr personality: replay the requests recorded
//! from the real Radarr (`tests/fixtures/radarr/golden/`) against Unarr on
//! the imported fixture and compare status and JSON body.
//!
//! The comparison modes are those of the Sonarr replay (`tests/common`).
//! Milestone 4a only reads, so there is no mock, no replay directory and
//! no command to wait for: each request is answered from the import, with
//! Unarr's clock pinned to the instant the golden was recorded.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use unarr_api::{AppState, LogBuffer, RuntimeInfo, Settings};
use unarr_core::{Db, Personality};

mod common;
use common::*;

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

/// Goldens recorded and not implemented yet, taken out of this list as the
/// work lands. The naming examples need the movie file name builder, which
/// comes with the milestone that renames (4c).
const PENDING: &[&str] = &["config-naming-examples"];

fn mode(name: &str) -> Mode {
    match name {
        n if n.contains("feed-calendar") && !n.starts_with("noauth") => Mode::Calendar,
        // What the machine, the build and the moment decide. Unarr's own
        // identity goes in the package fields, which the original leaves
        // out when it is not packaged.
        "system-status" | "apikey-query-system-status" => Mode::Exact(&[
            "isLinux",
            "isOsx",
            "isWindows",
            "startTime",
            "appData",
            "startupPath",
            "osName",
            "osVersion",
            "runtimeVersion",
            "runtimeName",
            "isNetCore",
            "mode",
            "isUserInteractive",
            "packageVersion",
            "packageAuthor",
            "packageUpdateMechanism",
            "databaseVersion",
            "isDocker",
            "isContainerized",
            "isDebug",
        ]),
        "session-initialize-json" => Mode::Exact(&["userHash", "analytics"]),
        "noauth-initialize-json" => Mode::Html,
        "config-host" => Mode::Exact(&["logLevel"]),
        n if n.starts_with("rootfolder") => {
            Mode::Exact(&["freeSpace", "totalSpace", "accessible", "unmappedFolders"])
        }
        n if n.ends_with("-404") => Mode::Exact(&["traceId"]),
        "diskspace" | "system-task" | "system-task-1" | "log" | "update" | "signalr-negotiate" => Mode::Shape,
        // Radarr builds `contentsUrl` from a route that no longer exists
        // (`/api/v1//radarr.txt`); Unarr's names the route that answers.
        // Its ids follow the order the directory lists the files in.
        "log-file" => Mode::Exact(&["lastWriteTime", "contentsUrl", "id"]),
        _ => Mode::Exact(&[]),
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/radarr")
}

/// The fixture imported into an in-memory database with Radarr's schema.
fn build_state() -> AppState {
    let dir = fixture_dir();
    let db = Db::open_in_memory_for(Personality::Radarr).expect("in-memory db");
    unarr_import::radarr::import(&dir.join("radarr.db"), &db).expect("import fixture");
    let host = unarr_import::config_xml::read(&dir.join("config.xml"), Personality::Radarr.default_port())
        .expect("config.xml");
    let settings = Settings::from_host_config(host, Personality::Radarr);
    // An application data folder of its own, with the log files the
    // original had written by `log-file`: written at one instant, so the
    // list (last written first) keeps the order of their names.
    let app_data = std::env::temp_dir().join(format!("unarr_contract_radarr_{}", std::process::id()));
    let logs_dir = app_data.join("logs");
    std::fs::create_dir_all(&logs_dir).expect("app data");
    let written = std::time::SystemTime::now();
    for name in ["radarr.debug.txt", "radarr.txt"] {
        std::fs::write(logs_dir.join(name), "").expect("log file");
        std::fs::File::options()
            .write(true)
            .open(logs_dir.join(name))
            .and_then(|f| f.set_modified(written))
            .expect("log file time");
    }
    let mut runtime = RuntimeInfo::detect(app_data);
    runtime.media_cover_dir =
        std::env::temp_dir().join(format!("unarr_contract_radarr_covers_{}", std::process::id()));
    let logs = LogBuffer::new();
    logs.push("info", "unarr", "contract test".into(), None);
    AppState::new(Personality::Radarr, db, settings, runtime, logs)
}

async fn send(app: &axum::Router, g: &Golden, session: &str) -> (StatusCode, Value) {
    let mut req = Request::builder().method(g.method.as_str()).uri(&g.path);
    if g.name.starts_with("session") {
        req = req.header("cookie", session);
    } else if !g.name.starts_with("noauth") && !g.path.contains("apikey=") {
        req = req.header("X-Api-Key", API_KEY);
    }
    let body = match &g.request_body {
        Some(json) => {
            req = req.header("content-type", "application/json");
            Body::from(json.clone())
        }
        None => Body::empty(),
    };
    let mut response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    // The recorder (urllib) follows redirects; do the same, once.
    if response.status().is_redirection() {
        if let Some(location) = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
        {
            response = app
                .clone()
                .oneshot(Request::builder().uri(&location).body(Body::empty()).unwrap())
                .await
                .unwrap();
        }
    }
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

#[tokio::test]
async fn radarr_golden_responses_match() {
    let state = build_state();
    let goldens = load_goldens(&fixture_dir().join("golden"));
    state.start_runtime().await.expect("the runtime");
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;
    let mut failures = Vec::new();
    let mut checked = 0;
    // `UNARR_CONTRACT_ONLY=prefix,prefix` replays part of the list.
    let only: Vec<String> = std::env::var("UNARR_CONTRACT_ONLY")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();

    for g in &goldens {
        // `UNARR_CONTRACT_ALL=1` replays the pending ones too, to see how far
        // each is.
        if PENDING.contains(&g.name.as_str()) && std::env::var_os("UNARR_CONTRACT_ALL").is_none() {
            continue;
        }
        if !only.is_empty() && !only.iter().any(|p| g.name.starts_with(p.as_str())) {
            continue;
        }
        // The mount table and free space are Linux facts; the golden was
        // recorded there.
        if g.name == "diskspace" && !cfg!(target_os = "linux") {
            continue;
        }
        checked += 1;
        state.set_now(g.recorded_at);
        let (status, body) = send(&app, g, &session).await;
        // `UNARR_CONTRACT_DUMP=name,name` prints what Unarr answered.
        if std::env::var("UNARR_CONTRACT_DUMP")
            .map(|v| v.split(',').any(|n| n == g.name))
            .unwrap_or(false)
        {
            eprintln!(
                "--- {} answered {status}:\n{}",
                g.name,
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );
        }
        if status.as_u16() != g.status {
            failures.push(format!(
                "{}: status {} expected, {} got",
                g.name, g.status, status
            ));
            continue;
        }
        let problems = compare(mode(&g.name), &g.body, &body);
        if !problems.is_empty() {
            failures.push(format!(
                "{} ({} {}):\n    {}",
                g.name,
                g.method,
                g.path,
                problems.join("\n    ")
            ));
        }
    }

    assert!(checked > 0, "no goldens checked");
    assert!(
        failures.is_empty(),
        "{} of {} goldens differ:\n{}",
        failures.len(),
        checked,
        failures.join("\n")
    );
}

#[tokio::test]
async fn pending_list_only_names_recorded_goldens() {
    let names: BTreeSet<String> = load_goldens(&fixture_dir().join("golden"))
        .into_iter()
        .map(|g| g.name)
        .collect();
    for p in PENDING {
        assert!(names.contains(*p), "PENDING names unknown golden {p}");
    }
}
