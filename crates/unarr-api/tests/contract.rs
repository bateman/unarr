//! Contract tests: replay the requests recorded from the real Sonarr
//! (`tests/fixtures/sonarr/golden/`) against Unarr on the imported fixture
//! and compare status and JSON body.
//!
//! Each golden has a comparison mode. `Exact` compares the whole body after
//! removing the listed volatile keys (matched by name anywhere in the tree).
//! `Shape` compares only the key structure, for responses whose values depend
//! on the machine or the moment (disk space, logs, task timings).
//! Goldens listed in `PENDING` are not implemented yet and are skipped.
//!
//! The recording ran with the fixture's root folder `/tv` and the mock
//! clients' `/downloads` as real directories; the replay points both at a
//! temporary directory (the library paths are rewritten after the import,
//! the mock is started on it) and maps them back before comparing, so a
//! golden that names `/tv/The Pitt/...` matches what Unarr wrote there.
//! `mock` lines of the request list go to the Rust mock in-process.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use unarr_api::{AppState, LogBuffer, RuntimeInfo, Settings};
use unarr_core::config::{AuthenticationRequired, HostConfig};
use unarr_core::{Db, Personality};

mod common;
use common::*;

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

/// The import phase's goldens, taken out of this list step by step as the
/// work lands.
const PENDING: &[&str] = &[];

/// The goldens of the import phase: the files, history rows and queue
/// items they show carry the clock of the moment the originals imported.
fn import_phase(name: &str) -> bool {
    !name.ends_with("-404")
        && [
            "import-",
            "upgrade-",
            "manualimport-",
            "rescan-",
            "scan-",
            "rename-",
            "delete-",
            "episodefile-",
        ]
        .iter()
        .any(|p| name.starts_with(p))
}

fn mode(name: &str) -> Mode {
    match name {
        // Phase 3b, series management. The originals download an added
        // series' covers while the notification goes out, so which URLs
        // carry a `lastWrite` there is a race.
        "series-add-notifications" => Mode::Exact(&["timestamp", "dateAdded", "url"]),
        "series-add-history" => Mode::Exact(&["date", "age", "ageHours", "ageMinutes", "lastSearchTime"]),
        // The folder the replay's library is in has another parent.
        "filesystem-tv" => Mode::Exact(&["lastModified", "parent"]),
        n if n.starts_with("filesystem-") => Mode::Exact(&["lastModified"]),
        n if n.contains("feed-calendar") && !n.starts_with("noauth") => Mode::Calendar,
        n if n.starts_with("mediacover-api-") => Mode::Exact(&["traceId"]),
        "blocklist-protocol-invalid" | "blocklist-protocol-comma" | "blocklist-protocol-empty" => {
            Mode::Exact(&["traceId"])
        }
        "series-after-delete-pete"
        | "series-after-importlist-sync-54"
        | "importlist-get-missing"
        | "importlist-update-missing" => Mode::Exact(&["traceId"]),
        // `System.ApplicationException` with the originals' stack.
        "series-delete-404" => Mode::Exact(&["description"]),
        n if !n.ends_with("-404") && n.starts_with("episode-")
            || n.starts_with("episodes-after-")
            || n.starts_with("parse-")
            || (n.starts_with("series-") && !n.ends_with("-404")) =>
        {
            Mode::Exact(&["lastSearchTime", "added", "dateAdded"])
        }
        // Phase 3b's refresh commands: the originals' ids and clocks, and a
        // posted one may have started by the time it is answered.
        "refresh-series-silo" | "refresh-series-stick" | "refresh-series-all" => Mode::Exact(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
            "status",
            "result",
            "message",
        ]),
        n if n.starts_with("refresh-series-") && n.ends_with("-get") => Mode::Exact(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
        ]),
        // What the refreshes left: episodes searched and files imported
        // earlier in the recording carry its clock.
        n if n.starts_with("refresh-series-") => Mode::Exact(&["lastSearchTime", "dateAdded"]),
        // What the notification targets received: Discord embeds carry
        // the moment they were built, webhook payloads the files' clock.
        "delete-notifications" => Mode::Multiset(&["timestamp", "dateAdded"]),
        // The Discord test message names the local time it was posted at.
        "notifications-after-test" => Mode::Exact(&["content"]),
        n if n.ends_with("-notifications") => Mode::Exact(&["timestamp", "dateAdded"]),
        // A target that does not resolve or refuses the connection: the
        // originals let the exception escape as a 500.
        "notification-testall" | "notification-test-webhook-unreachable" => Mode::Shape,
        n if n.ends_with("-refresh") || (n.contains("-refresh-") && !n.contains("queue")) => Mode::Exact(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
            "status",
            "result",
            "message",
        ]),
        "import-episodefile-missing" | "notification-delete-missing" | "notification-update-missing" => {
            Mode::Exact(&["traceId"])
        }
        // `ModelNotFoundException`s carry their stack in the description.
        "episodefile-delete-missing" | "rename-preview-missing" | "manualimport-reprocess-noseries" => {
            Mode::Exact(&["description"])
        }
        // The originals fail to re-evaluate a file nothing can parse.
        "manualimport-reprocess" => Mode::Shape,
        // The items' ids hash their path, which the replay moves.
        n if n.starts_with("manualimport-") && !n.contains("queue") && !n.contains("history") => {
            Mode::Exact(&["id", "dateAdded", "lastSearchTime"])
        }
        n if import_phase(n) && n.contains("queue") => {
            Mode::Unordered(&["estimatedCompletionTime", "added", "lastSearchTime", "dateAdded"])
        }
        n if import_phase(n) && n.contains("history") => Mode::Exact(&[
            "date",
            "age",
            "ageHours",
            "ageMinutes",
            "lastSearchTime",
            "dateAdded",
        ]),
        n if import_phase(n) => Mode::Exact(&["dateAdded", "lastSearchTime"]),
        "system-status" | "apikey-query-system-status" | "oldkey-system-status" => Mode::Exact(&[
            "isLinux",
            "isOsx",
            "isWindows",
            "startTime",
            "appData",
            "startupPath",
            "osName",
            "osVersion",
            "buildTime",
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
            "isDebug",
        ]),
        "session-initialize-json" => Mode::Exact(&["userHash", "analytics"]),
        "noauth-initialize-json" => Mode::Html,
        "config-host" => Mode::Exact(&["logLevel"]),
        n if n.starts_with("rootfolder") => {
            Mode::Exact(&["freeSpace", "totalSpace", "accessible", "unmappedFolders"])
        }
        n if n.ends_with("-404") || n == "downloadclient-update-missing" || n == "command-get-missing" => {
            Mode::Exact(&["traceId"])
        }
        // Commands carry the originals' ids and clocks; a freshly posted
        // one may already have started, or finished, when it is read back.
        n if n.starts_with("command-") && n.ends_with("-get") => Mode::Exact(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
        ]),
        "command-list" => Mode::ManualCommands(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
        ]),
        "command-unknown" => Mode::Shape,
        "command-cancel-completed" => Mode::Exact(&["description"]),
        n if n.starts_with("command-") => Mode::Exact(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
            "status",
            "result",
            "message",
        ]),
        // The originals' exceptions carry their stack in the description.
        "config-mediamanagement-put-invalid"
        | "qualityprofile-delete-in-use"
        | "customformat-create-badregex"
        | "customformat-delete" => Mode::Exact(&["description"]),
        // The originals answer the quality definitions from a five-second
        // cache, so right after a bulk update they show what was there
        // before; Unarr shows what it wrote.
        "qualitydefinition-update" | "qualitydefinition-after-update" | "qualitydefinition-1-before" => {
            Mode::Shape
        }
        "release-episode-missing" | "release-grab-uncached" | "release-grab-unknown-client" => Mode::Shape,
        // The queue's times follow the clock the client was read at; the
        // history and blocklist rows a grab leaves carry the grab's.
        n if n.starts_with("queue") => Mode::Unordered(&["estimatedCompletionTime", "added"]),
        // The originals keep verdicts in a dictionary.
        "health-after" | "health-after-importlist-sync" => Mode::Unordered(&[]),
        "history-after-grab"
        | "history-download-id"
        | "history-episode-1-grabbed"
        | "history-after-delete"
        | "history-after-mark-failed"
        | "history-after-command" => Mode::Exact(&["date", "age", "ageHours", "ageMinutes"]),
        "history-mark-failed-missing" => Mode::Exact(&["description"]),
        // The salt is drawn anew for every password.
        "config-host-put-credentials" | "config-host-after-credentials" | "config-host-put-apikey" => {
            Mode::Exact(&["password"])
        }
        // What the originals themselves logged after the clearing.
        "log-after-clear" => Mode::Exact(&["records", "totalRecords"]),
        "blocklist-after-delete" => Mode::Exact(&["date"]),
        // Ages in hours and minutes carry the fraction of a second the
        // recording happened at.
        n if n.starts_with("release") => Mode::Exact(&["ageHours", "ageMinutes"]),
        // The socket error text in the detail is the runtime's (.NET's
        // "Connection refused (127.0.0.1:1)" against Rust's).
        n if n.starts_with("downloadclient-unreachable") || n == "downloadclient-testall" => {
            Mode::Exact(&["detailedDescription"])
        }
        "diskspace" | "system-task" | "system-task-after" | "log" | "update" | "command"
        | "signalr-negotiate" => Mode::Shape,
        // The zip carries each application's name, version and clock.
        "system-backup" => Mode::Exact(&["id", "name", "path", "size", "time"]),
        // Sonarr builds `contentsUrl` from a route that no longer exists
        // (`/api/v1//sonarr.txt`); Unarr's names the route that answers.
        "log-file" => Mode::Exact(&["lastWriteTime", "contentsUrl"]),
        _ => Mode::Exact(&[]),
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sonarr")
}

/// The goldens were recorded with the originals' bundled ffprobe, which
/// `make golden` leaves under `.cache`; the replay probes with the same
/// build when it is there, else with `UNARR_FFPROBE` or the `PATH`'s.
fn use_bundled_ffprobe() {
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/sonarr/Sonarr/ffprobe");
    // The bundled build is the original's Linux one: elsewhere it does not
    // run, and the system's is better than none.
    let runs = std::process::Command::new(&bundled)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if runs && std::env::var_os("UNARR_FFPROBE").is_none() {
        unarr_core::mediafiles::mediainfo::set_ffprobe_path(Some(bundled));
    }
}

fn build_state() -> AppState {
    build_state_with(|_| {})
}

/// The fixture imported, with `config.xml` adjusted by `adjust`.
fn build_state_with(adjust: impl FnOnce(&mut HostConfig)) -> AppState {
    build_state_in(None, adjust)
}

/// The directories the replay stands in for the recording's `/tv` and
/// `/downloads`, under one temporary root.
struct ReplayDirs {
    root: PathBuf,
    tv: PathBuf,
    downloads: PathBuf,
}

impl ReplayDirs {
    fn create() -> ReplayDirs {
        ReplayDirs::create_named("replay")
    }

    /// Tests run side by side in one process: each takes its own name.
    fn create_named(name: &str) -> ReplayDirs {
        let root = std::env::temp_dir().join(format!("unarr_contract_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tv = root.join("tv");
        let downloads = root.join("downloads");
        std::fs::create_dir_all(&tv).expect("temp tv dir");
        std::fs::create_dir_all(&downloads).expect("temp downloads dir");
        ReplayDirs { root, tv, downloads }
    }

    /// `(replay path, recorded path)` pairs.
    fn mappings(&self) -> Vec<(String, &'static str)> {
        vec![
            (self.downloads.to_string_lossy().into_owned(), "/downloads"),
            (self.tv.to_string_lossy().into_owned(), "/tv"),
        ]
    }
}

impl Drop for ReplayDirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The fixture imported, its library moved under `dirs` when given.
fn build_state_in(dirs: Option<&ReplayDirs>, adjust: impl FnOnce(&mut HostConfig)) -> AppState {
    use_bundled_ffprobe();
    let dir = fixture_dir();
    let db = Db::open_in_memory().expect("in-memory db");
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
    if let Some(dirs) = dirs {
        let tv = dirs.tv.to_string_lossy().into_owned();
        db.call_blocking(move |conn| {
            conn.execute("UPDATE root_folders SET path = ?1 WHERE path = '/tv'", [&tv])?;
            conn.execute(
                "UPDATE series SET path = ?1 || substr(path, 4) WHERE path LIKE '/tv/%'",
                [&tv],
            )?;
            // The import list's root folder names the same path.
            conn.execute(
                "UPDATE providers SET settings = replace(settings, '\"/tv\"', '\"' || ?1 || '\"'),
                    attributes = replace(attributes, '\"/tv\"', '\"' || ?1 || '\"')",
                [&tv],
            )?;
            Ok(())
        })
        .expect("move the library under the replay directory");
    }
    let mut host = unarr_import::config_xml::read(&dir.join("config.xml"), 8989).expect("config.xml");
    adjust(&mut host);
    let settings = Settings::from_host_config(host, Personality::Sonarr);
    // The replay writes into its app data (backups, a staged restore):
    // a copy under the replay's directory, never the fixture.
    let app_data = match dirs {
        Some(dirs) => {
            let app_data = dirs.root.join("appdata");
            let logs = app_data.join("logs");
            std::fs::create_dir_all(&logs).expect("replay app data");
            std::fs::copy(dir.join("config.xml"), app_data.join("config.xml")).expect("config.xml copy");
            // The log files the original had written by `log-file`.
            // Written at one instant, so the list (last written first)
            // keeps the order of their names, as recorded.
            let written = std::time::SystemTime::now();
            for name in ["sonarr.debug.txt", "sonarr.txt"] {
                std::fs::write(logs.join(name), "").expect("log file");
                std::fs::File::options()
                    .write(true)
                    .open(logs.join(name))
                    .and_then(|f| f.set_modified(written))
                    .expect("log file time");
            }
            app_data
        }
        None => dir.clone(),
    };
    let mut runtime = RuntimeInfo::detect(app_data);
    // Covers the refreshes download go to the replay's directory, never
    // into the fixture.
    runtime.media_cover_dir = match dirs {
        Some(dirs) => dirs.root.join("MediaCover"),
        None => std::env::temp_dir().join(format!("unarr_contract_covers_{}", std::process::id())),
    };
    let logs = LogBuffer::new();
    logs.push("info", "unarr", "contract test".into(), None);
    AppState::new(Personality::Sonarr, db, settings, runtime, logs)
}

/// What a mock golden cannot hold Unarr to: the Plex refreshes around a
/// library scan ride on the originals' minute refresh, which ran where it
/// happened to during the recording; Unarr flushes them with the scan
/// that imported, so the two snapshots split them differently.
fn mock_filter(name: &str, v: Value) -> Value {
    match (name, v) {
        ("rescan-notifications" | "scan-notifications", Value::Array(items)) => {
            Value::Array(items.into_iter().filter(|i| i["service"] != "plex").collect())
        }
        (_, v) => v,
    }
}

async fn send(
    app: &axum::Router,
    g: &Golden,
    session: &str,
    path: &str,
    mappings: &[(String, &str)],
) -> (StatusCode, Value) {
    let path = map_paths(path, mappings);
    let mut req = Request::builder().method(g.method.as_str()).uri(path);
    if g.name.starts_with("session") {
        req = req.header("cookie", session);
    } else if !g.name.starts_with("noauth") && !g.path.contains("apikey=") {
        req = req.header("X-Api-Key", API_KEY);
    }
    let body = match &g.request_body {
        Some(json) => {
            req = req.header("content-type", "application/json");
            Body::from(map_paths(json, mappings))
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

/// The mock indexers and clients the write goldens were recorded against,
/// on the port the recorded provider settings carry, reporting their
/// downloads under the replay directory.
async fn start_mock(dirs: &ReplayDirs) -> unarr_mock::Mock {
    unarr_mock::Mock::start_with(
        unarr_mock::fixtures_dir(),
        18989,
        unarr_mock::MockDirs {
            downloads: Some(dirs.downloads.clone()),
            tv: Some(dirs.tv.clone()),
            movies: None,
        },
    )
    .await
    .expect("port 18989 free for the mock indexers (stop a stale scripts/mock-servarr.py)")
}

#[tokio::test]
async fn golden_responses_match() {
    let dirs = ReplayDirs::create();
    let mappings = dirs.mappings();
    let mock = start_mock(&dirs).await;
    // The recording machine's name, which Discord embeds carry as author;
    // pinned before the runtime builds its identity from the hostname.
    unarr_core::notifications::pin_machine_name("vm");
    let state = build_state_in(Some(&dirs), |_| {});
    // The metadata hosts the recording reached through the mock's tunnels.
    state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock(&mock.url()));
    let goldens = load_goldens(&fixture_dir().join("golden"));
    // The recorder marks every scheduled task as just run before it starts
    // (scripts/sonarr-fixture.sh), so that no refresh, RSS sync or
    // housekeeping runs while it records; the replay starts the same way.
    if let Some(start) = goldens.iter().filter_map(|g| g.recorded_at).min() {
        state
            .db()
            .call(move |conn| {
                let ids: Vec<i64> = conn
                    .prepare("SELECT id FROM scheduled_tasks")?
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                for id in ids {
                    unarr_core::store::providers::set_scheduled_task_execution(conn, id, start, start)?;
                }
                Ok(())
            })
            .await
            .expect("freeze the scheduled tasks");
    }
    state.start_runtime().await.expect("the command runtime");
    // The recording's one NZBGet grab got this drone id from the originals'
    // random GUID; the queue and history goldens name it.
    unarr_core::downloadclients::nzbget::pin_drone_ids(&["c860499553f64d7b8170eae60cd1a5b3"]);
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;
    let mut host_password: Option<String> = None;
    let mut failures = Vec::new();
    let mut checked = 0;
    let mut ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    for g in &goldens {
        if g.method == "sleep" {
            // The recording's clock moved on while the originals' background
            // work ran; the checks Unarr runs meanwhile see the same clock.
            let seconds: i64 = g.path.parse().expect("sleep seconds");
            let now = state.now() + chrono::Duration::seconds(seconds);
            state.set_now(Some(now));
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().unwrap().wait_idle(),
            )
            .await
            .expect("commands finish within a minute");
            continue;
        }
        if PENDING.contains(&g.name.as_str()) {
            continue;
        }
        // The mount table and free space are Linux facts; the golden was
        // recorded there.
        if g.name == "diskspace" && !cfg!(target_os = "linux") {
            continue;
        }
        let path = substitute_ids(&g.path, &ids);
        if g.to_mock {
            // The control calls name the recording's directories.
            let mut path = path;
            for (replay, recorded) in &mappings {
                path = path.replace(&format!("path={recorded}"), &format!("path={replay}"));
            }
            let body = g.request_body.as_deref().unwrap_or("").as_bytes();
            let (status, bytes) = mock.call(&g.method, &path, body);
            if g.name == "-" {
                // A control call that fails (an item a pending golden never
                // grabbed, say) shows up in the goldens that follow.
                if !status.is_success() {
                    eprintln!(
                        "mock {} {} answered {status}: {}",
                        g.method,
                        g.path,
                        String::from_utf8_lossy(&bytes)
                    );
                }
                continue;
            }
            checked += 1;
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let body = unmap_paths(&body, &mappings);
            if std::env::var("UNARR_CONTRACT_DUMP")
                .map(|v| v.split(',').any(|n| n == g.name))
                .unwrap_or(false)
            {
                eprintln!(
                    "--- {} (mock) answered {status}:\n{}",
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
            let mut problems = Vec::new();
            let (want, got) = match mode(&g.name) {
                Mode::Exact(ignore) => (normalize(&g.body, ignore), normalize(&body, ignore)),
                Mode::Multiset(ignore) => (
                    sorted_list(normalize(&g.body, ignore)),
                    sorted_list(normalize(&body, ignore)),
                ),
                _ => panic!("mock goldens compare exactly"),
            };
            let (want, got) = (mock_filter(&g.name, want), mock_filter(&g.name, got));
            diff("", &want, &got, &mut problems);
            if !problems.is_empty() {
                failures.push(format!(
                    "{} (mock {} {}):\n    {}",
                    g.name,
                    g.method,
                    g.path,
                    problems.join("\n    ")
                ));
            }
            continue;
        }
        checked += 1;
        state.set_now(g.recorded_at);
        if g.name == "health-after-importlist-sync" {
            // The originals' scheduler refreshed the queue every minute of
            // the recording, the replay runs no schedule: the refresh that
            // found the unreachable clients after the startup grace period
            // is run here.
            let runtime = state.commands().unwrap();
            runtime
                .queue()
                .push(
                    unarr_core::commands::CommandBody::RefreshMonitoredDownloads,
                    unarr_core::commands::CommandPriority::Normal,
                    unarr_core::commands::CommandTrigger::Scheduled,
                    state.now(),
                )
                .await
                .expect("a queued refresh");
            tokio::time::timeout(std::time::Duration::from_secs(60), runtime.wait_idle())
                .await
                .expect("the refresh finishes within a minute");
        }
        // A client sends the stored password hash back as it read it; the
        // recording's hash carries the recording's salt, so the replay
        // sends the one Unarr answered with.
        let own_hash;
        let g = match (&host_password, &g.request_body) {
            (Some(hash), Some(text)) if g.method == "PUT" && g.path.ends_with("/config/host") => {
                let mut sent: Value = serde_json::from_str(text).unwrap();
                if sent["passwordConfirmation"].as_str().unwrap_or("").is_empty()
                    && sent["password"].as_str().is_some_and(|p| !p.is_empty())
                {
                    sent["password"] = Value::String(hash.clone());
                }
                own_hash = Golden {
                    request_body: Some(sent.to_string()),
                    ..g.clone()
                };
                &own_hash
            }
            _ => g,
        };
        let (status, body) = send(&app, g, &session, &path, &mappings).await;
        if g.path.ends_with("/config/host") && status.is_success() {
            if let Some(hash) = body["password"].as_str().filter(|h| !h.is_empty()) {
                host_password = Some(hash.to_string());
            }
        }
        if g.name == "command-backup" {
            // The recording read the backup back five seconds later.
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().unwrap().wait_idle(),
            )
            .await
            .expect("the backup finishes within a minute");
        }
        if g.method == "PUT" && g.path.starts_with("/api/v3/importlist") {
            // Saving a list queues its sync, which the originals had run by
            // the next request of the recording (the series it adds, the
            // notifications it sends).
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().unwrap().wait_idle(),
            )
            .await
            .expect("the list's sync finishes within a minute");
        }
        let body = unmap_paths(&body, &mappings);
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
        // As the recorder: the answer's id, or its first item's.
        let answered = body
            .get("id")
            .or_else(|| body.get(0).and_then(|first| first.get("id")));
        if let Some(id) = answered.and_then(Value::as_i64) {
            ids.insert(g.name.clone(), id);
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
    drop(mock);
    drop(dirs);
}

#[tokio::test]
async fn pending_list_only_names_recorded_goldens() {
    let names: BTreeSet<String> = load_goldens(&fixture_dir().join("golden"))
        .into_iter()
        .filter(|g| g.method != "sleep" && g.name != "-")
        .map(|g| g.name)
        .collect();
    for p in PENDING {
        assert!(names.contains(*p), "PENDING names unknown golden {p}");
    }
}

#[tokio::test]
async fn api_requires_the_key() {
    let app = unarr_api::router(build_state());
    for (uri, key, expected) in [
        ("/api/v3/health", None, StatusCode::UNAUTHORIZED),
        ("/api/v3/health", Some("wrong"), StatusCode::UNAUTHORIZED),
        ("/api/v3/health", Some(API_KEY), StatusCode::OK),
        (
            "/api/v3/health?apikey=wrong",
            Some(API_KEY),
            StatusCode::UNAUTHORIZED,
        ),
        // Forms authentication in the fixture: the browser is sent to the login form.
        ("/initialize.json", None, StatusCode::FOUND),
        // The originals do not take the API key for pages either.
        ("/initialize.json", Some(API_KEY), StatusCode::FOUND),
        // Deep links are pages too: the guard covers the SPA fallback.
        ("/settings", None, StatusCode::FOUND),
        ("/series/the-pitt", None, StatusCode::FOUND),
        ("/login", None, StatusCode::OK),
    ] {
        let mut req = Request::builder().uri(uri);
        if let Some(k) = key {
            req = req.header("X-Api-Key", k);
        }
        let response = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{uri} with key {key:?}");
    }
}

async fn status_of(app: &axum::Router, req: Request<Body>) -> StatusCode {
    app.clone().oneshot(req).await.unwrap().status()
}

fn get(uri: &str) -> axum::http::request::Builder {
    Request::builder().uri(uri)
}

/// The ways a session can fail to open a page, each sent back to the login.
#[tokio::test]
async fn ui_auth_rejects_bad_sessions_and_forwarded_locals() {
    let state = build_state();
    let app = unarr_api::router(state.clone());
    let cookie = login_cookie(&app).await;
    let page = |cookie: &str| {
        get("/initialize.json")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(status_of(&app, page(&cookie)).await, StatusCode::OK);

    // A signature with one character changed.
    let mut tampered = cookie.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    assert_eq!(
        status_of(&app, page(&tampered)).await,
        StatusCode::FOUND,
        "tampered"
    );

    // A session past its expiry (rememberMe lasts seven days).
    state.set_now(Some(chrono::Utc::now() + chrono::Duration::days(8)));
    assert_eq!(status_of(&app, page(&cookie)).await, StatusCode::FOUND, "expired");
    state.set_now(None);

    // The local-address exemption trusts the peer address, never a proxy header.
    let local_state =
        build_state_with(|h| h.authentication_required = AuthenticationRequired::DisabledForLocalAddresses);
    let local_app = unarr_api::router(local_state);
    let from = |ip: &str, forwarded: bool| {
        let mut b = get("/initialize.json").extension(ConnectInfo(
            format!("{ip}:40000").parse::<std::net::SocketAddr>().unwrap(),
        ));
        if forwarded {
            b = b.header("x-forwarded-for", "203.0.113.9");
        }
        b.body(Body::empty()).unwrap()
    };
    assert_eq!(
        status_of(&local_app, from("192.168.1.5", false)).await,
        StatusCode::OK,
        "LAN peer"
    );
    assert_eq!(
        status_of(&local_app, from("192.168.1.5", true)).await,
        StatusCode::FOUND,
        "LAN peer behind a proxy"
    );
    assert_eq!(
        status_of(&local_app, from("8.8.8.8", false)).await,
        StatusCode::FOUND,
        "remote peer"
    );
}

/// `returnUrl` stays a local path: no other host, no backslash trick, and it
/// is encoded when the login fails.
#[tokio::test]
async fn login_return_url_is_kept_local() {
    let app = unarr_api::router(build_state());
    let post = |query: &str, body: &str| {
        Request::builder()
            .method("POST")
            .uri(format!("/login{query}"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let location = |response: axum::response::Response| {
        response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    };
    for bad in [
        "//evil.example/x",
        "/\\evil.example/x",
        "/\t/evil.example",
        "/evil.example\r\n",
        "https://evil.example/",
        "evil",
    ] {
        let encoded = bad
            .replace('\\', "%5C")
            .replace('\t', "%09")
            .replace('\r', "%0D")
            .replace('\n', "%0A");
        let r = app
            .clone()
            .oneshot(post(
                &format!("?returnUrl={encoded}"),
                "username=unarr&password=fixture",
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FOUND);
        assert_eq!(location(r), "/", "returnUrl {bad:?}");
    }
    let r = app
        .clone()
        .oneshot(post(
            "?returnUrl=%2Fseries%2Fthe-pitt",
            "username=unarr&password=fixture",
        ))
        .await
        .unwrap();
    assert_eq!(location(r), "/series/the-pitt");
    let r = app
        .clone()
        .oneshot(post(
            "?returnUrl=%2Fseries%3Fa%3D1%26b%3D2",
            "username=unarr&password=wrong",
        ))
        .await
        .unwrap();
    assert_eq!(
        location(r),
        "/login?returnUrl=/series%3Fa%3D1%26b%3D2&loginFailed=true"
    );
}

/// The URL base is matched in any case, as the API part is (recorded on
/// Sonarr 4.0.20 and Radarr 6.4.4: `/TV/ping` under the base `/tv` is
/// answered); a path that only starts with its letters is outside it.
#[tokio::test]
async fn url_base_is_matched_in_any_case() {
    let app = unarr_api::router(build_state_with(|h| h.url_base = "/Sonarr".into()));
    for (uri, expected) in [
        ("/Sonarr/api/v3/health", StatusCode::OK),
        ("/Sonarr/api/v3/episodeFile?seriesId=22", StatusCode::OK),
        ("/sonarr/api/v3/health", StatusCode::OK),
        ("/SONARR/API/V3/health", StatusCode::OK),
        // Outside, it is a page: the login first.
        ("/sonarrx/api/v3/health", StatusCode::FOUND),
    ] {
        let req = get(uri).header("X-Api-Key", API_KEY).body(Body::empty()).unwrap();
        assert_eq!(status_of(&app, req).await, expected, "{uri}");
    }
}

/// Query parameters are read the same way on every route: case-insensitive,
/// `True` accepted, empty values ignored.
#[tokio::test]
async fn series_query_parameters_are_lenient() {
    let app = unarr_api::router(build_state());
    for (uri, expected_len) in [
        ("/api/v3/series?TVDBID=448176", 1),
        ("/api/v3/series?tvdbId=", 38),
        ("/api/v3/series?includeSeasonImages=True", 38),
    ] {
        let response = app
            .clone()
            .oneshot(get(uri).header("X-Api-Key", API_KEY).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.as_array().unwrap().len(), expected_len, "{uri}");
    }
}

/// `AllowedHosts` from config.xml is enforced as the originals do: listed
/// hosts, loopback and the machine pass, anything else is a 400.
#[tokio::test]
async fn allowed_hosts_are_enforced() {
    let app = unarr_api::router(build_state_with(|h| {
        h.allowed_hosts = "sonarr.home, *.lan.example".into()
    }));
    for (host, expected) in [
        (Some("sonarr.home"), StatusCode::OK),
        (Some("sonarr.home:8989"), StatusCode::OK),
        (Some("tv.lan.example"), StatusCode::OK),
        (Some("localhost:8989"), StatusCode::OK),
        (Some("127.0.0.1"), StatusCode::OK),
        (Some("[::1]:8989"), StatusCode::OK),
        (None, StatusCode::OK),
        (Some("evil.example"), StatusCode::BAD_REQUEST),
        (Some("lan.example"), StatusCode::BAD_REQUEST),
    ] {
        let mut req = get("/api/v3/health").header("X-Api-Key", API_KEY);
        if let Some(h) = host {
            req = req.header("host", h);
        }
        assert_eq!(
            status_of(&app, req.body(Body::empty()).unwrap()).await,
            expected,
            "host {host:?}"
        );
    }
    // The fixture's `*`: everything passes.
    let open = unarr_api::router(build_state());
    let req = get("/api/v3/health")
        .header("X-Api-Key", API_KEY)
        .header("host", "evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(&open, req).await, StatusCode::OK);
}

/// A backup is listed, downloads as the list's `path` says (a page, behind
/// the login), and restores from an upload of the same zip.
#[tokio::test]
async fn backup_downloads_and_restores_from_an_upload() {
    let dirs = ReplayDirs::create_named("backup");
    let state = build_state_in(Some(&dirs), |_| {});
    state.start_runtime().await.expect("the command runtime");
    let app = unarr_api::router(state.clone());
    let app_data = state.runtime().app_data.clone();
    let api = |method: &str, uri: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("X-Api-Key", API_KEY)
    };
    let bytes_of = |response: axum::response::Response| async {
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
    };

    let post = api("POST", "/api/v3/command")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Backup","type":"manual"}"#))
        .unwrap();
    assert_eq!(status_of(&app, post).await, StatusCode::CREATED);
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        state.commands().unwrap().wait_idle(),
    )
    .await
    .expect("the backup finishes within a minute");

    let listed = app
        .clone()
        .oneshot(api("GET", "/api/v3/system/backup").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let listed: Value = serde_json::from_slice(&bytes_of(listed).await).unwrap();
    assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed}");
    let path = listed[0]["path"].as_str().unwrap().to_string();
    assert!(path.starts_with("/backup/manual/unarr_sonarr_backup_v"), "{path}");

    // The download is a page: the API key does not open it, a session does.
    assert_eq!(
        status_of(&app, api("GET", &path).body(Body::empty()).unwrap()).await,
        StatusCode::FOUND
    );
    let cookie = login_cookie(&app).await;
    let page = |uri: &str| {
        get(uri)
            .header("Cookie", cookie.clone())
            .body(Body::empty())
            .unwrap()
    };
    let download = app.clone().oneshot(page(&path)).await.unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let zip = bytes_of(download).await;
    assert!(zip.starts_with(b"PK"), "a zip archive");
    // Only what the list names is served.
    for uri in [
        "/backup/manual/config.xml",
        "/backup/manual/..%2F..%2Fconfig.xml",
        "/backup/scheduled/missing.zip",
    ] {
        assert_eq!(status_of(&app, page(uri)).await, StatusCode::NOT_FOUND, "{uri}");
    }

    let upload = |filename: &str, content: &[u8]| {
        let mut body = format!(
            "--XBOUNDARY\r\nContent-Disposition: form-data; name=\"restore\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n--XBOUNDARY--\r\n");
        api("POST", "/api/v3/system/backup/restore/upload")
            .header("Content-Type", "multipart/form-data; boundary=XBOUNDARY")
            .body(Body::from(body))
            .unwrap()
    };
    assert_eq!(
        status_of(&app, upload("backup.txt", &zip)).await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    // Not an archive: refused, nothing staged.
    assert_eq!(
        status_of(&app, upload("backup.zip", b"not a zip")).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(!app_data.join("restore").exists());

    let restored = app.clone().oneshot(upload("backup.zip", &zip)).await.unwrap();
    assert_eq!(restored.status(), StatusCode::OK);
    let restored: Value = serde_json::from_slice(&bytes_of(restored).await).unwrap();
    assert_eq!(restored, serde_json::json!({ "restartRequired": true }));
    assert!(app_data.join("restore").join("unarr-sonarr.db").is_file());
    assert!(app_data.join("restore").join("config.xml").is_file());
    assert!(!app_data.join(".restore-upload.zip").exists());

    let _ = std::fs::remove_dir_all(&dirs.root);
}

/// The session lasts seven days from its last use: past half its life a
/// request renews the cookie, and one left unused for seven days is gone.
#[tokio::test]
async fn sessions_slide_and_say_how_they_may_travel() {
    let state = build_state();
    let app = unarr_api::router(state.clone());
    let login = |form: &'static str, proto: Option<&'static str>| {
        let mut req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(proto) = proto {
            req = req.header("x-forwarded-proto", proto);
        }
        req.body(Body::from(form)).unwrap()
    };
    let set_cookie = |response: &axum::response::Response| {
        response
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_string())
    };

    // "Remember Me" keeps the cookie for seven days; without it the cookie
    // ends with the browser.
    let response = app
        .clone()
        .oneshot(login("username=unarr&password=fixture&rememberMe=on", None))
        .await
        .unwrap();
    let remembered = set_cookie(&response).expect("a cookie");
    assert!(remembered.contains("Max-Age=604800"), "{remembered}");
    assert!(
        remembered.contains("Path=/") && remembered.contains("HttpOnly"),
        "{remembered}"
    );
    assert!(!remembered.contains("Secure"), "{remembered}");
    let response = app
        .clone()
        .oneshot(login("username=unarr&password=fixture", None))
        .await
        .unwrap();
    let passing = set_cookie(&response).expect("a cookie");
    assert!(!passing.contains("Max-Age"), "{passing}");
    // The header of a peer nobody trusts does not make the cookie Secure:
    // the test's requests carry no peer address at all.
    let response = app
        .clone()
        .oneshot(login("username=unarr&password=fixture", Some("https")))
        .await
        .unwrap();
    assert!(!set_cookie(&response).expect("a cookie").contains("Secure"));

    let cookie = remembered.split(';').next().unwrap().to_string();
    let page = |cookie: &str| {
        get("/initialize.json")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap()
    };
    // Fresh: nothing to renew.
    let response = app.clone().oneshot(page(&cookie)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(set_cookie(&response).is_none());
    assert!(response
        .headers()
        .get("cache-control")
        .is_some_and(|v| v.to_str().unwrap().contains("no-store")));

    // Four days on, past half its life: renewed, and the new one lasts
    // until day eleven while the old one stops at day seven.
    let start = chrono::Utc::now();
    state.set_now(Some(start + chrono::Duration::days(4)));
    let response = app.clone().oneshot(page(&cookie)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let renewed = set_cookie(&response).expect("a renewed cookie");
    assert!(renewed.contains("Max-Age=604800"), "{renewed}");
    let renewed = renewed.split(';').next().unwrap().to_string();
    state.set_now(Some(start + chrono::Duration::days(8)));
    assert_eq!(status_of(&app, page(&cookie)).await, StatusCode::FOUND);
    assert_eq!(status_of(&app, page(&renewed)).await, StatusCode::OK);
    state.set_now(None);
}

/// The interface's Restart and Shutdown reach whoever runs the process.
#[tokio::test]
async fn restart_and_shutdown_are_handed_to_the_process() {
    for (path, key, expected) in [
        (
            "/api/v3/system/shutdown",
            "shuttingDown",
            unarr_api::Lifecycle::Shutdown,
        ),
        (
            "/api/v3/system/restart",
            "restarting",
            unarr_api::Lifecycle::Restart,
        ),
    ] {
        let state = build_state();
        let app = unarr_api::router(state.clone());
        // Not without the key.
        let anonymous = Request::builder()
            .method("POST")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&app, anonymous).await, StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("X-Api-Key", API_KEY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body[key], Value::Bool(true));
        let asked = tokio::time::timeout(std::time::Duration::from_secs(5), state.lifecycle_requested())
            .await
            .expect("the request is there to be read");
        assert_eq!(asked, expected);
    }
}

/// `TrustCgnatIpAddresses`: an address of `100.64.0.0/10` counts as local
/// for "Disabled for Local Addresses" once the setting is saved.
#[tokio::test]
async fn carrier_grade_nat_addresses_are_local_when_trusted() {
    let state =
        build_state_with(|h| h.authentication_required = AuthenticationRequired::DisabledForLocalAddresses);
    let app = unarr_api::router(state.clone());
    let from = |ip: &str| {
        get("/initialize.json")
            .extension(ConnectInfo(
                format!("{ip}:40000").parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(status_of(&app, from("100.100.1.1")).await, StatusCode::FOUND);

    let api = |method: &str, path: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("X-Api-Key", API_KEY)
    };
    let response = app
        .clone()
        .oneshot(api("GET", "/api/v3/config/host").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut host: Value = serde_json::from_slice(&bytes).unwrap();
    host["trustCgnatIpAddresses"] = Value::Bool(true);
    // The form asks for the hosts once authentication is not for everyone.
    host["allowedHosts"] = Value::String("sonarr.test".into());
    let save = api("PUT", "/api/v3/config/host")
        .header("content-type", "application/json")
        .body(Body::from(host.to_string()))
        .unwrap();
    let response = app.clone().oneshot(save).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    assert_eq!(status_of(&app, from("100.100.1.1")).await, StatusCode::OK);
    // Next to the range, and a public address: still asked to log in.
    assert_eq!(status_of(&app, from("100.128.0.1")).await, StatusCode::FOUND);
    assert_eq!(status_of(&app, from("8.8.8.8")).await, StatusCode::FOUND);
}

/// The cover route of the API: the file as it is, the full cover for a
/// resized one that was not made, 404 for what is not there, and only
/// with the key.
#[tokio::test]
async fn the_api_serves_covers_to_clients_with_the_key() {
    let state = build_state();
    let covers = state.runtime().media_cover_dir.join("777001");
    std::fs::create_dir_all(&covers).unwrap();
    std::fs::write(covers.join("poster.jpg"), b"full").unwrap();
    std::fs::write(covers.join("poster-500.jpg"), b"resized").unwrap();
    std::fs::write(covers.join("banner-70.jpg"), b"").unwrap();
    std::fs::write(covers.join("clearlogo.png"), b"logo").unwrap();
    let app = unarr_api::router(state);
    let fetch = |file: &str, key: bool| {
        let app = app.clone();
        let mut request = get(&format!("/api/v3/mediacover/777001/{file}"));
        if key {
            request = request.header("X-Api-Key", API_KEY);
        }
        async move {
            let response = app.oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
            let status = response.status();
            let kind = response
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap().to_string());
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, kind, String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    let (status, kind, body) = fetch("poster-500.jpg", true).await;
    assert_eq!(
        (status, kind.as_deref(), body.as_str()),
        (StatusCode::OK, Some("image/jpeg"), "resized")
    );
    let (status, kind, body) = fetch("clearlogo.png", true).await;
    assert_eq!(
        (status, kind.as_deref(), body.as_str()),
        (StatusCode::OK, Some("image/png"), "logo")
    );
    // Not made: the full cover.
    assert_eq!(fetch("poster-1000.jpg", true).await.2, "full");
    // Empty, and no full one to give instead.
    assert_eq!(fetch("banner-70.jpg", true).await.0, StatusCode::NOT_FOUND);
    assert_eq!(fetch("fanart.jpg", true).await.0, StatusCode::NOT_FOUND);
    // Not an image's name, and a way out of the folder.
    assert_eq!(fetch("notes.txt", true).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        fetch("..%2F..%2Fsonarr.db.jpg", true).await.0,
        StatusCode::NOT_FOUND
    );
    // Without the key.
    assert_eq!(fetch("poster.jpg", false).await.0, StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(&covers);
}

/// `SeriesEditedService`: a series saved with another type is refreshed;
/// saved with the same one, it is not.
#[tokio::test]
async fn changing_the_series_type_queues_a_refresh() {
    let state = build_state();
    // The refresh this queues must reach no metadata host: nothing listens
    // where it is sent.
    state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock("http://127.0.0.1:1"));
    state.start_runtime().await.expect("the command runtime");
    let app = unarr_api::router(state.clone());
    let api = |method: &str, path: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("X-Api-Key", API_KEY)
            .header("content-type", "application/json")
    };
    let response = app
        .clone()
        .oneshot(api("GET", "/api/v3/series/1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let series: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(series["seriesType"], "standard");
    let refreshes = |state: &AppState| {
        let state = state.clone();
        async move {
            state
                .db()
                .call(|conn| {
                    Ok(conn.query_row(
                        "SELECT count(*) FROM commands WHERE name = 'RefreshSeries'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap()
        }
    };
    let before = refreshes(&state).await;

    // Saved as it is: nothing queued.
    let save = api("PUT", "/api/v3/series/1")
        .body(Body::from(series.to_string()))
        .unwrap();
    assert_eq!(status_of(&app, save).await, StatusCode::ACCEPTED);
    assert_eq!(refreshes(&state).await, before);

    let mut anime = series.clone();
    anime["seriesType"] = Value::String("anime".into());
    let save = api("PUT", "/api/v3/series/1")
        .body(Body::from(anime.to_string()))
        .unwrap();
    assert_eq!(status_of(&app, save).await, StatusCode::ACCEPTED);
    assert_eq!(refreshes(&state).await, before + 1);
}

/// The originals' `CheckOn` attributes: a save of something a health
/// check reads asks for the checks tied to it, behind the debounce. What
/// each set holds is tested with the checks; here, which write asks for
/// which.
#[tokio::test]
async fn saves_ask_for_the_health_checks_tied_to_them() {
    use unarr_core::health::CheckSet;
    let dirs = ReplayDirs::create_named("checkon");
    let state = build_state_in(Some(&dirs), |_| {});
    // The list sync a list's save queues must reach no host.
    state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock("http://127.0.0.1:1"));
    state.start_runtime().await.expect("the command runtime");
    let app = unarr_api::router(state.clone());
    let runtime = state.commands().unwrap().clone();
    let call = |method: &'static str, path: String, body: Option<Value>| {
        let app = app.clone();
        async move {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("X-Api-Key", API_KEY)
                .header("content-type", "application/json")
                .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    // The start asks for every check.
    assert_eq!(runtime.take_pending_health_checks(), [CheckSet::All]);

    // The settings: every page raises `ConfigSavedEvent`, changed or not,
    // except the naming, which is not kept with them.
    for page in [
        "mediamanagement",
        "indexer",
        "downloadclient",
        "importlist",
        "ui",
        "host",
    ] {
        let (_, mut body) = call("GET", format!("/api/v3/config/{page}"), None).await;
        if page == "host" {
            // The page as the UI saves it, with the proxy filled in.
            let saved = fixture_dir().join("golden/bodies/config-host-proxy.json");
            body = serde_json::from_str(&std::fs::read_to_string(saved).unwrap()).unwrap();
        }
        let (status, _) = call("PUT", format!("/api/v3/config/{page}/1"), Some(body)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{page}");
        let asked = runtime.take_pending_health_checks();
        assert!(!asked.is_empty(), "{page}");
        assert!(
            asked.iter().all(|s| *s == CheckSet::ConfigSaved),
            "{page}: {asked:?}"
        );
    }
    let (_, body) = call("GET", "/api/v3/config/naming".into(), None).await;
    let (status, _) = call("PUT", "/api/v3/config/naming/1".into(), Some(body)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(runtime.take_pending_health_checks(), []);

    // Root folders: one added; a refused one asks for nothing, and neither
    // does a removal, which the original does by id without an event (its
    // log shows no check after one, nor after a mapping's).
    let folder = dirs.root.join("more");
    std::fs::create_dir_all(&folder).unwrap();
    let folder = folder.to_string_lossy().into_owned();
    let (status, _) = call(
        "POST",
        "/api/v3/rootfolder".into(),
        Some(serde_json::json!({"path": "/nowhere/at/all"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(runtime.take_pending_health_checks(), []);
    let (status, created) = call(
        "POST",
        "/api/v3/rootfolder".into(),
        Some(serde_json::json!({"path": folder})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(runtime.take_pending_health_checks(), [CheckSet::RootFolders]);
    let (status, _) = call("DELETE", format!("/api/v3/rootfolder/{}", created["id"]), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runtime.take_pending_health_checks(), []);

    // Remote path mappings: added and changed; not removed.
    let mapping =
        serde_json::json!({"host": "client.example", "remotePath": "/remote/", "localPath": folder});
    let (status, created) = call("POST", "/api/v3/remotepathmapping".into(), Some(mapping.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        runtime.take_pending_health_checks(),
        [CheckSet::RemotePathMappings]
    );
    let path = format!("/api/v3/remotepathmapping/{}", created["id"]);
    let (status, _) = call("PUT", path.clone(), Some(mapping)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        runtime.take_pending_health_checks(),
        [CheckSet::RemotePathMappings]
    );
    let (status, _) = call("DELETE", path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runtime.take_pending_health_checks(), []);

    // Import lists: changed and removed.
    let (_, lists) = call("GET", "/api/v3/importlist".into(), None).await;
    let mut list = lists[0].clone();
    let path = format!("/api/v3/importlist/{}?forceSave=true", list["id"]);
    list["name"] = Value::String("Another name".into());
    let (status, _) = call("PUT", path, Some(list.clone())).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(runtime.take_pending_health_checks(), [CheckSet::ImportLists]);
    let (status, _) = call("DELETE", format!("/api/v3/importlist/{}", list["id"]), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runtime.take_pending_health_checks(), [CheckSet::ImportLists]);

    // A series moved, then deleted.
    let (_, mut series) = call("GET", "/api/v3/series/1".into(), None).await;
    let from = series["path"].as_str().unwrap().to_string();
    std::fs::create_dir_all(&from).unwrap();
    let to = format!("{from} moved");
    series["path"] = Value::String(to.clone());
    let (status, _) = call("PUT", "/api/v3/series/1?moveFiles=true".into(), Some(series)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let mut asked = Vec::new();
    for _ in 0..200 {
        asked = runtime.take_pending_health_checks();
        if !asked.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(asked, [CheckSet::SeriesGone], "after the move");
    assert!(std::path::Path::new(&to).is_dir());
    let (status, _) = call("DELETE", "/api/v3/series/1".into(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runtime.take_pending_health_checks(), [CheckSet::SeriesGone]);
}

/// The calendar feed refuses a range no calendar reaches instead of
/// computing with it, and a key that is blank opens nothing.
#[tokio::test]
async fn the_feed_refuses_absurd_ranges_and_blank_keys() {
    let app = unarr_api::router(build_state());
    for query in [
        "pastDays=1000000000",
        "futureDays=9223372036854775807",
        "pastDays=-9223372036854775808",
    ] {
        let request = get(&format!("/feed/v3/calendar/Sonarr.ics?{query}"))
            .header("X-Api-Key", API_KEY)
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&app, request).await, StatusCode::BAD_REQUEST, "{query}");
    }
    let wide = get("/feed/v3/calendar/Sonarr.ics?pastDays=36500&futureDays=36500")
        .header("X-Api-Key", API_KEY)
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(&app, wide).await, StatusCode::OK);

    // An installation whose key came out blank: `apikey=` is not a key.
    let blank = unarr_api::router(build_state_with(|h| h.api_key = String::new()));
    for uri in [
        "/api/v3/system/status?apikey=",
        "/api/v3/system/status",
        "/feed/v3/calendar/Sonarr.ics?apikey=",
    ] {
        let request = get(uri).body(Body::empty()).unwrap();
        assert_eq!(
            status_of(&blank, request).await,
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
    }
    let header = get("/api/v3/system/status")
        .header("X-Api-Key", "")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(&blank, header).await, StatusCode::UNAUTHORIZED);
}

/// The hub messages this replay answers for: the lists another tab
/// shows, the episodes on the pages that list them, the commands. `health`
/// is compared apart, below; the queue's and the providers' messages are
/// the long list's business.
const HUB_NAMES: &[&str] = &[
    "tag",
    "rootfolder",
    "qualitydefinition",
    "system/task",
    "command",
    "episode",
    "wanted/missing",
    "wanted/cutoff",
    "episodefile",
    "series",
];

/// A hub message as it is compared: its name and action, and of what it
/// carries the id, the folder of a `rootfolder`, whether an episode is
/// marked as grabbed and comes with its series, file and images.
fn hub_line(message: &Value, mappings: &[(String, &str)]) -> Option<String> {
    let name = message["name"].as_str()?;
    if !HUB_NAMES.contains(&name) && name != "health" {
        return None;
    }
    let body = &message["body"];
    let mut line = format!("{name} {}", body["action"].as_str().unwrap_or("?"));
    let resource = unmap_paths(&body["resource"], mappings);
    match name {
        // Commands carry the ids each application numbers them with, and
        // the original sends the state one is in when its debounce ends:
        // which command, not in which state.
        "command" => {
            if let Some(command) = resource["name"].as_str() {
                line.push_str(&format!(" {command}"));
            }
        }
        "rootfolder" => line.push_str(&format!(
            " id={} path={}",
            resource["id"],
            resource["path"].as_str().unwrap_or("?")
        )),
        "episode" | "wanted/missing" | "wanted/cutoff" => {
            line.push_str(&format!(" id={}", resource["id"]));
            for key in ["grabbed", "series", "episodeFile", "images"] {
                if !resource[key].is_null() {
                    line.push_str(&format!(" +{key}"));
                }
            }
        }
        "series" => line.push_str(&format!(" id={}", resource["id"])),
        // The files the scans import are numbered by each application.
        _ => {}
    }
    Some(line)
}

/// Sonarr's hub around the writes whose lists another tab shows, against a
/// recording of its own (`tests/fixtures/sonarr/golden-hub`, `make golden`
/// with `GOLDEN_SIGNALR=1`): `tag` after a tag or an auto tagging rule
/// changes, `rootfolder` after one is added and nothing after one goes,
/// `qualitydefinition` after the reset and after no other save,
/// `system/task` after every command and `command` sync after the old ones
/// are cleaned up; an episode under the three names of the pages that list
/// episodes when it is grabbed, imported or loses its file; `health` after
/// every run of the checks. The same list shows the refresh of the
/// downloads a scan queues when it imports a file.
#[tokio::test]
async fn the_hub_tells_other_tabs_what_the_original_tells_them() {
    let golden_dir = fixture_dir().join("golden-hub");
    let dirs = ReplayDirs::create_named("hub");
    let mappings = dirs.mappings();
    // A mock of its own on a free port: the long list's holds the port
    // the fixture's providers name, which are pointed here instead.
    let mock = unarr_mock::Mock::start_with(
        unarr_mock::fixtures_dir(),
        0,
        unarr_mock::MockDirs {
            downloads: Some(dirs.downloads.clone()),
            tv: Some(dirs.tv.clone()),
            movies: None,
        },
    )
    .await
    .expect("a mock on a free port");
    let state = build_state_in(Some(&dirs), |_| {});
    let port = mock.port().to_string();
    state
        .db()
        .call(move |conn| {
            conn.execute(
                "UPDATE providers SET settings = replace(settings, ':18989', ':' || ?1)",
                [&port],
            )?;
            Ok(())
        })
        .await
        .expect("the providers pointed at this test's mock");
    state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock(&mock.url()));
    let goldens = load_goldens(&golden_dir);
    // As the recorder leaves them: every scheduled task just run.
    let start = goldens
        .iter()
        .filter_map(|g| g.recorded_at)
        .min()
        .expect("a recording time");
    state
        .db()
        .call(move |conn| {
            let ids: Vec<i64> = conn
                .prepare("SELECT id FROM scheduled_tasks")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for id in ids {
                unarr_core::store::providers::set_scheduled_task_execution(conn, id, start, start)?;
            }
            Ok(())
        })
        .await
        .expect("freeze the scheduled tasks");
    state.set_now(Some(start));
    state.start_runtime().await.expect("the command runtime");
    let mut hub = state.hub().subscribe();
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;

    // What the hub said, stretch by stretch: a stretch ends with a pause
    // of the list, when Unarr's own work is waited for.
    let mut stretches: Vec<Vec<String>> = vec![Vec::new()];
    let mut stretch_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut answers: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut failures = Vec::new();
    for g in &goldens {
        if g.method == "sleep" {
            let seconds: i64 = g.path.parse().expect("sleep seconds");
            state.set_now(Some(state.now() + chrono::Duration::seconds(seconds)));
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().unwrap().wait_idle(),
            )
            .await
            .expect("commands finish within a minute");
            // The relay sends on a task of its own.
            while let Ok(Ok(message)) =
                tokio::time::timeout(std::time::Duration::from_millis(300), hub.recv()).await
            {
                stretches
                    .last_mut()
                    .unwrap()
                    .extend(hub_line(&message, &mappings));
            }
            stretches.push(Vec::new());
            continue;
        }
        let path = substitute_ids(&g.path, &ids);
        if g.to_mock {
            let mut path = path;
            for (replay, recorded) in &mappings {
                path = path.replace(&format!("path={recorded}"), &format!("path={replay}"));
            }
            let (status, _) = mock.call(&g.method, &path, &[]);
            assert!(status.is_success(), "mock {} {path}: {status}", g.method);
            continue;
        }
        stretch_of.insert(g.name.clone(), stretches.len() - 1);
        // A body names the id of an earlier answer as the path does, and
        // the mock by the port it was recorded on.
        let mut sent = g.clone();
        sent.request_body = g.request_body.as_ref().map(|body| {
            ids.iter()
                .fold(body.clone(), |body, (name, id)| {
                    body.replace(&format!("\"{{id:{name}}}\""), &id.to_string())
                })
                .replace("18989", &mock.port().to_string())
        });
        let (status, body) = send(&app, &sent, &session, &path, &mappings).await;
        let body = unmap_paths(&body, &mappings);
        // As the recorder: the answer's id, or its first item's.
        let answered = body
            .get("id")
            .or_else(|| body.get(0).and_then(|first| first.get("id")));
        if let Some(id) = answered.and_then(Value::as_i64) {
            ids.insert(g.name.clone(), id);
        }
        if status.as_u16() != g.status {
            failures.push(format!("{}: status {} expected, {status} got", g.name, g.status));
        }
        // The resources these writes answer with, as recorded, compared
        // as the long list compares the steps of the same names; the
        // commands are compared for what the test is about, below, and
        // the grab's steps, which name the mock's port and number files,
        // are the long list's to compare.
        let compared = ![
            "command-",
            "indexer-",
            "downloadclient-",
            "release-",
            "episodefile",
        ]
        .iter()
        .any(|prefix| g.name.starts_with(prefix));
        if compared {
            let problems = compare(mode(&g.name), &g.body, &body);
            if !problems.is_empty() {
                failures.push(format!("{}:\n    {}", g.name, problems.join("\n    ")));
            }
        }
        answers.insert(g.name.clone(), body);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Each scan imported the file dropped for it, and the import asked for
    // the downloads to be refreshed: in the original's list of commands
    // and in Unarr's, after the scan and queued by nobody (`unspecified`,
    // where the scheduler's own refresh would say `scheduled`).
    for (list_name, scan) in [
        ("command-after-rescan", "RescanSeries"),
        ("command-after-downloaded-scan", "DownloadedEpisodesScan"),
    ] {
        let recorded = goldens
            .iter()
            .find(|g| g.name == list_name)
            .unwrap_or_else(|| panic!("{list_name} is recorded"));
        for (who, list) in [("the original", &recorded.body), ("Unarr", &answers[list_name])] {
            let commands: Vec<&Value> = list.as_array().into_iter().flatten().collect();
            let scanned = commands
                .iter()
                .find(|c| c["name"] == scan)
                .unwrap_or_else(|| panic!("{who} lists no {scan}: {list}"));
            let refreshed = commands.iter().any(|c| {
                c["name"] == "RefreshMonitoredDownloads"
                    && c["trigger"] == "unspecified"
                    && c["queued"].as_str() >= scanned["queued"].as_str()
            });
            assert!(
                refreshed,
                "{who} queued no refresh of the downloads after {scan}: {list}"
            );
        }
    }

    // The hub, stretch by stretch.
    let text = std::fs::read_to_string(golden_dir.join("signalr.json")).expect("signalr.json");
    let steps: Vec<Value> = serde_json::from_str(&text).expect("signalr.json");
    let mut original: Vec<Vec<String>> = vec![Vec::new(); stretches.len()];
    for step in &steps {
        let after = step["after"].as_str().unwrap_or("");
        let at = if after == "(start)" {
            0
        } else {
            *stretch_of
                .get(after)
                .unwrap_or_else(|| panic!("signalr.json names a step the list does not have: {after}"))
        };
        for message in step["messages"].as_array().into_iter().flatten() {
            original[at].extend(hub_line(message, &[]));
        }
    }
    // Two messages follow work that is not only the list's. `health`
    // follows every run of the checks, and the original's scheduler
    // refreshes the downloads each minute of a recording, where a client
    // that stops answering runs them again; `system/task` follows every
    // command, the scheduler's own among them, on either side. Both must
    // be there where the list makes the checks or a command run; `health`
    // nowhere the original ran no check.
    const HEALTH: &str = "health sync";
    const TASKS: &str = "system/task sync";
    let mut health_problems = Vec::new();
    for (line, steps) in [
        (
            HEALTH,
            &[
                "command-rescan-1",
                "command-downloaded-scan",
                "rootfolder-create",
                "remotepathmapping-create",
                "downloadclient-create-sabnzbd",
            ][..],
        ),
        (
            TASKS,
            &[
                "command-rescan-1",
                "command-downloaded-scan",
                "command-reset-quality-definitions",
                "command-messaging-cleanup",
                "release-grab-usenet",
            ][..],
        ),
    ] {
        for step in steps {
            let at = stretch_of[*step];
            for (who, lines) in [("the original", &original[at]), ("Unarr", &stretches[at])] {
                if !lines.iter().any(|l| l == line) {
                    health_problems.push(format!("{who} sent no `{line}` after {step}"));
                }
            }
        }
    }
    for (at, (want, got)) in original.iter_mut().zip(stretches.iter_mut()).enumerate() {
        if got.iter().any(|l| l == HEALTH) && !want.iter().any(|l| l == HEALTH) {
            health_problems.push(format!(
                "Unarr sent `health` in stretch {at}, where the original ran no check"
            ));
        }
        want.retain(|l| l != HEALTH && l != TASKS);
        got.retain(|l| l != HEALTH && l != TASKS);
    }
    assert!(health_problems.is_empty(), "{}", health_problems.join("\n"));
    let first_step = |at: usize| {
        let mut names: Vec<&String> = stretch_of
            .iter()
            .filter(|(_, stretch)| **stretch == at)
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names.first().map(|n| n.to_string()).unwrap_or_default()
    };
    let mut problems = Vec::new();
    assert!(original.iter().flatten().count() > 10, "the recording is read");
    for (at, (want, got)) in original.iter_mut().zip(stretches.iter_mut()).enumerate() {
        // Which messages and how many of each. Where the original says
        // one thing several times over and Unarr says it a known number of
        // times (or, for one command, once more), the step, the message and
        // Unarr's count are named here: the original's lines are compared
        // by presence, Unarr's by that exact count, so a change that makes
        // Unarr say it fifty times does not pass.
        const COUNTS: &[(&str, &[(&str, usize)])] = &[
            (
                "command-rescan-1",
                &[
                    ("episode updated", 1),
                    ("wanted/cutoff updated", 1),
                    ("wanted/missing updated", 1),
                ],
            ),
            (
                "command-after-rescan",
                &[
                    ("episode updated", 1),
                    ("wanted/cutoff updated", 1),
                    ("wanted/missing updated", 1),
                ],
            ),
            (
                "episodefile-delete",
                &[
                    ("episode updated", 1),
                    ("wanted/cutoff updated", 1),
                    ("wanted/missing updated", 1),
                ],
            ),
            ("release-episode", &[("episode updated", 1)]),
            (
                "command-messaging-cleanup",
                &[("command updated MessagingCleanup", 2)],
            ),
        ];
        let step = first_step(at);
        let counted: &[(&str, usize)] = COUNTS
            .iter()
            .find(|(name, _)| *name == step)
            .map(|(_, lines)| *lines)
            .unwrap_or(&[]);
        let rule = |line: &str| counted.iter().find(|(prefix, _)| line.starts_with(prefix));
        want.sort();
        got.sort();
        let mut distinct = got.clone();
        distinct.dedup();
        for line in &distinct {
            if let Some((_, expected)) = rule(line) {
                let sent = got.iter().filter(|l| *l == line).count();
                if sent != *expected {
                    problems.push(format!(
                        "stretch {at} ({step}): Unarr sent `{line}` {sent} times, {expected} expected"
                    ));
                }
            }
        }
        for lines in [&mut *want, &mut *got] {
            lines.dedup_by(|a, b| a == b && rule(a).is_some());
        }
        if want != got {
            problems.push(format!(
                "stretch {at} ({}):\n    the original: {want:?}\n    Unarr:        {got:?}",
                first_step(at)
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the hub differs from the original's (golden-hub/signalr.json):\n{}",
        problems.join("\n")
    );
    drop(mock);
    drop(dirs);
}

/// The `queue/status` messages on the hub from now on, in order.
async fn queue_statuses(hub: &mut tokio::sync::broadcast::Receiver<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    // The status follows the refresh, in a task of its own.
    while let Ok(Ok(message)) = tokio::time::timeout(std::time::Duration::from_secs(2), hub.recv()).await {
        if message["name"] == "queue/status" {
            out.push(message["body"].clone());
        }
    }
    out
}

/// `QueueStatusController` counts the pending releases with the queue, on
/// SignalR as in `GET /queue/status`.
#[tokio::test]
async fn the_queue_status_on_the_hub_counts_pending_releases() {
    use unarr_core::downloads::pending::{PendingRelease, PendingReleaseReason};
    use unarr_core::indexers::rss::ReleaseInfo;
    use unarr_core::store;

    let state = build_state();
    // Two releases held back by a delay profile, one episode each; no
    // download client to ask, so the queue itself is empty.
    state
        .db()
        .call(|conn| {
            conn.execute("DELETE FROM providers WHERE kind = 'downloadclient'", [])?;
            for (n, title) in [
                "The.Pitt.S01E01.1080p.WEB.h264-GRP",
                "The.Pitt.S01E02.1080p.WEB.h264-GRP",
            ]
            .into_iter()
            .enumerate()
            {
                store::pending::insert(
                    conn,
                    &PendingRelease {
                        id: 0,
                        series_id: 1,
                        title: title.into(),
                        added: chrono::Utc::now(),
                        release: ReleaseInfo {
                            guid: format!("pending-{n}"),
                            title: title.into(),
                            ..Default::default()
                        },
                        reason: PendingReleaseReason::Delay,
                        series_match_type: Default::default(),
                        release_source: Default::default(),
                    },
                )?;
            }
            Ok(())
        })
        .await
        .expect("pending releases");
    let mut hub = state.hub().subscribe();
    let app = unarr_api::router(state.clone());
    let call = |method: &str, uri: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("X-Api-Key", API_KEY)
            .body(Body::empty())
            .unwrap()
    };

    // The endpoint counts them.
    let response = app
        .clone()
        .oneshot(call("GET", "/api/v3/queue/status"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let answered: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(answered["totalCount"], 2, "{answered}");

    // One of the two removed: the hub says what the endpoint says.
    let response = app.clone().oneshot(call("GET", "/api/v3/queue")).await.unwrap();
    let page: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = page["records"][0]["id"].as_i64().expect("a pending queue item");
    let response = app
        .clone()
        .oneshot(call("DELETE", &format!("/api/v3/queue/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let announced = queue_statuses(&mut hub).await;
    let last = announced.last().expect("a queue status on the hub");
    assert_eq!(last["action"], "updated");
    assert_eq!(last["resource"]["totalCount"], 1, "{announced:?}");
    assert_eq!(last["resource"]["count"], 1, "{announced:?}");
}

/// What `tests/fixtures/sonarr/golden-events` recorded on the original:
/// the console's level saved with the host settings and handed to the
/// logging; the deprecated `CheckForFinishedDownload` run as the refresh
/// of the downloads, with the original's warning in the log; and the
/// Import Complete notification a manual import of a file no download
/// client knows sends (`UntrackedDownloadCompletedEvent`).
#[tokio::test]
async fn the_events_and_settings_of_the_audit_match_the_original() {
    use tracing_subscriber::layer::SubscriberExt;
    let golden_dir = fixture_dir().join("golden-events");
    let dirs = ReplayDirs::create_named("events");
    let mappings = dirs.mappings();
    let mock = unarr_mock::Mock::start_with(
        unarr_mock::fixtures_dir(),
        0,
        unarr_mock::MockDirs {
            downloads: Some(dirs.downloads.clone()),
            tv: Some(dirs.tv.clone()),
            movies: None,
        },
    )
    .await
    .expect("a mock on a free port");
    unarr_core::notifications::pin_machine_name("vm");
    let state = build_state_in(Some(&dirs), |_| {});
    let port = mock.port().to_string();
    let p = port.clone();
    state
        .db()
        .call(move |conn| {
            conn.execute(
                "UPDATE providers SET settings = replace(settings, ':18989', ':' || ?1)",
                [&p],
            )?;
            Ok(())
        })
        .await
        .expect("the providers pointed at this test's mock");
    state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock(&mock.url()));
    let goldens = load_goldens(&golden_dir);
    let start = goldens
        .iter()
        .filter_map(|g| g.recorded_at)
        .min()
        .expect("a recording time");
    state
        .db()
        .call(move |conn| {
            let ids: Vec<i64> = conn
                .prepare("SELECT id FROM scheduled_tasks")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for id in ids {
                unarr_core::store::providers::set_scheduled_task_execution(conn, id, start, start)?;
            }
            Ok(())
        })
        .await
        .expect("freeze the scheduled tasks");
    state.set_now(Some(start));
    // What the daemon's logging is told after each save of the settings.
    let told = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let t = told.clone();
    state.set_log_level_hook(move |host| {
        t.lock().unwrap().push((
            host.console_log_level.clone(),
            format!("{:?}", host.console_level()),
        ))
    });
    // The recent lines, as the daemon's logging fills them.
    let _logging =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(state.logs().clone()));
    state.start_runtime().await.expect("the command runtime");
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;
    let mut ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut answers: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut host_password: Option<String> = None;
    let mut failures = Vec::new();
    for g in &goldens {
        if g.method == "sleep" {
            let seconds: i64 = g.path.parse().expect("sleep seconds");
            state.set_now(Some(state.now() + chrono::Duration::seconds(seconds)));
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().unwrap().wait_idle(),
            )
            .await
            .expect("commands finish within a minute");
            continue;
        }
        let path = substitute_ids(&g.path, &ids);
        if g.to_mock {
            let mut path = path;
            for (replay, recorded) in &mappings {
                path = path.replace(&format!("path={recorded}"), &format!("path={replay}"));
            }
            let (status, bytes) = mock.call(&g.method, &path, &[]);
            assert!(status.is_success(), "mock {} {path}: {status}", g.method);
            if g.name != "-" {
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                answers.insert(g.name.clone(), unmap_paths(&body, &mappings));
            }
            continue;
        }
        let mut sent = g.clone();
        sent.request_body = g.request_body.as_ref().map(|body| {
            let mut body = body.replace("18989", &port);
            // The stored hash goes back as the client read it: Unarr's.
            if let (Some(hash), true) = (&host_password, g.path.ends_with("/config/host")) {
                let mut v: Value = serde_json::from_str(&body).unwrap();
                v["password"] = Value::String(hash.clone());
                body = v.to_string();
            }
            body
        });
        state.set_now(g.recorded_at);
        let (status, body) = send(&app, &sent, &session, &path, &mappings).await;
        let body = unmap_paths(&body, &mappings);
        if g.path.ends_with("/config/host") && g.method == "GET" {
            host_password = body["password"].as_str().map(str::to_string);
        }
        if let Some(id) = body.get("id").and_then(Value::as_i64) {
            ids.insert(g.name.clone(), id);
        }
        if status.as_u16() != g.status {
            failures.push(format!(
                "{}: status {} expected, {status} got: {body}",
                g.name, g.status
            ));
        }
        answers.insert(g.name.clone(), body);
    }
    // The console's level, as stored and read back.
    for name in [
        "config-host-put-console",
        "config-host-console",
        "config-host-put-console-blank",
    ] {
        let want = &goldens.iter().find(|g| g.name == name).unwrap().body["consoleLogLevel"];
        let got = &answers[name]["consoleLogLevel"];
        if want != got {
            failures.push(format!("{name}: consoleLogLevel {want} expected, {got} got"));
        }
    }
    // The commands, as the long list compares them.
    for name in [
        "command-check-finished",
        "command-check-finished-get",
        "command-manual-import-untracked-get",
        "manualimport-untracked-notifications",
    ] {
        let want = &goldens.iter().find(|g| g.name == name).unwrap().body;
        let problems = compare(mode(name), want, &answers[name]);
        if !problems.is_empty() {
            failures.push(format!("{name}:\n    {}", problems.join("\n    ")));
        }
    }
    // The warning, among the recent lines at warn.
    let warning = |v: &Value| {
        v["records"].as_array().into_iter().flatten().any(|r| {
            r["level"] == "warn"
                && r["message"].as_str().is_some_and(|m| {
                    m.starts_with("A third party app used the deprecated CheckForFinishedDownload command")
                })
        })
    };
    let recorded = &goldens.iter().find(|g| g.name == "log-warnings").unwrap().body;
    assert!(warning(recorded), "the recording has the warning");
    if !warning(&answers["log-warnings"]) {
        failures.push(format!(
            "log-warnings: no warning about the command in {}",
            answers["log-warnings"]
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Each save handed the logging the console's level.
    assert_eq!(
        told.lock().unwrap().as_slice(),
        [
            ("Warn".to_string(), "Warn".to_string()),
            (String::new(), "Info".to_string())
        ]
    );
}

/// `CleanupAdditionalUsers` and `FixFutureRunScheduledTasks`, against
/// what the original's database held after the same housekeeping
/// (`tests/fixtures/sonarr/golden-housekeeping`).
#[tokio::test]
async fn housekeeping_repairs_users_and_task_runs_as_the_original() {
    let state = build_state();
    housekeeping_repairs_as_the_original(&state, &fixture_dir().join("golden-housekeeping"), API_KEY).await;
}

/// How each answer of the audit's recording is compared.
fn audit_mode(name: &str) -> Mode {
    match name {
        // The exception the original's schema builder throws, with its stack.
        "metadata-create-unknown" => Mode::Exact(&["description"]),
        // As `config-host` of the long list.
        "config-host-by-id" => Mode::Exact(&["logLevel"]),
        n if n.ends_with("-missing") => Mode::Exact(&["traceId"]),
        _ => Mode::Exact(&[]),
    }
}

/// The routes the audit found Sonarr declaring and Unarr not, or only in
/// part (`docs/audit/7-triage.md`, "Routes and UI calls"), against a
/// recording of their own (`tests/fixtures/sonarr/golden-audit`): an
/// episode by id under the calendar and the wanted lists, the settings
/// pages and the dictionary by id, the quality size limits, the deprecated
/// language profile's writes, an exclusion by id, and the metadata
/// consumers added, tested and deleted.
#[tokio::test]
async fn the_audit_goldens_match() {
    let golden_dir = fixture_dir().join("golden-audit");
    let dirs = ReplayDirs::create_named("audit");
    let mappings = dirs.mappings();
    let state = build_state_in(Some(&dirs), |_| {});
    let goldens = load_goldens(&golden_dir);
    let start = goldens
        .iter()
        .filter_map(|g| g.recorded_at)
        .min()
        .expect("a recording time");
    state
        .db()
        .call(move |conn| {
            let ids: Vec<i64> = conn
                .prepare("SELECT id FROM scheduled_tasks")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for id in ids {
                unarr_core::store::providers::set_scheduled_task_execution(conn, id, start, start)?;
            }
            Ok(())
        })
        .await
        .expect("freeze the scheduled tasks");
    state.set_now(Some(start));
    state.start_runtime().await.expect("the command runtime");
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;
    let mut ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut failures = Vec::new();
    let mut checked = 0;
    for g in &goldens {
        assert!(
            g.method != "sleep" && !g.to_mock,
            "the audit list has no pauses and no mock"
        );
        state.set_now(g.recorded_at);
        let path = substitute_ids(&g.path, &ids);
        let (status, body) = send(&app, g, &session, &path, &mappings).await;
        let body = unmap_paths(&body, &mappings);
        if let Some(id) = body
            .get("id")
            .or_else(|| body.get(0).and_then(|first| first.get("id")))
            .and_then(Value::as_i64)
        {
            ids.insert(g.name.clone(), id);
        }
        checked += 1;
        if status.as_u16() != g.status {
            failures.push(format!(
                "{}: status {} expected, {status} got: {}",
                g.name,
                g.status,
                short(&body)
            ));
            continue;
        }
        let problems = compare(audit_mode(&g.name), &g.body, &body);
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
    assert_eq!(checked, 52, "every answer of the audit's recording");
    assert!(
        failures.is_empty(),
        "{} of {checked} audit goldens differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
