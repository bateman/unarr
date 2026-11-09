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

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

/// The import phase's goldens, taken out of this list step by step as the
/// work lands.
const PENDING: &[&str] = &[];

enum Mode {
    /// Full comparison, ignoring the named keys wherever they appear.
    Exact(&'static [&'static str]),
    /// `Exact`, with every list of identified objects compared in id order:
    /// the originals list the queue from a concurrent dictionary, so ties
    /// on the sort key come out in any order.
    Unordered(&'static [&'static str]),
    /// `Exact` over the commands a client started: the originals' own
    /// refreshes and the scheduler's commands come and go with the clock.
    ManualCommands(&'static [&'static str]),
    /// `Exact` over a list whose order the originals do not fix: the
    /// notifications a delete leaves, where the media server's refresh
    /// runs on a background handler and lands wherever it may.
    Multiset(&'static [&'static str]),
    /// Key structure only.
    Shape,
    /// An HTML page: status and the page title.
    Html,
}

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
        "system-status" | "apikey-query-system-status" => Mode::Exact(&[
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
        "release-episode-missing" | "release-grab-uncached" | "release-grab-unknown-client" => Mode::Shape,
        // The queue's times follow the clock the client was read at; the
        // history and blocklist rows a grab leaves carry the grab's.
        n if n.starts_with("queue") => Mode::Unordered(&["estimatedCompletionTime", "added"]),
        // The originals keep verdicts in a dictionary.
        "health-after" => Mode::Unordered(&[]),
        "history-after-grab"
        | "history-download-id"
        | "history-episode-1-grabbed"
        | "history-after-delete"
        | "history-after-command" => Mode::Exact(&["date", "age", "ageHours", "ageMinutes"]),
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
    if bundled.is_file() && std::env::var_os("UNARR_FFPROBE").is_none() {
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
        let root = std::env::temp_dir().join(format!("unarr_contract_{}", std::process::id()));
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

/// Every string in `v` with the replay directories mapped back to the
/// recorded ones.
fn unmap_paths(v: &Value, mappings: &[(String, &str)]) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (replay, recorded) in mappings {
                if out.contains(replay.as_str()) {
                    out = out.replace(replay.as_str(), recorded);
                }
            }
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| unmap_paths(v, mappings)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), unmap_paths(v, mappings)))
                .collect(),
        ),
        _ => v.clone(),
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
    let runtime = RuntimeInfo::detect(dir);
    let logs = LogBuffer::new();
    logs.push("info", "unarr", "contract test".into(), None);
    AppState::new(Personality::Sonarr, db, settings, runtime, logs)
}

/// A list in a fixed order, for `Mode::Multiset`.
fn sorted_list(v: Value) -> Value {
    match v {
        Value::Array(mut items) => {
            items.sort_by_key(|i| i.to_string());
            Value::Array(items)
        }
        other => other,
    }
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

/// Drop ignored keys and normalise numbers so `100` equals `100.0`.
fn normalize(v: &Value, ignore: &[&str]) -> Value {
    normalize_with(v, ignore, false)
}

fn normalize_with(v: &Value, ignore: &[&str], sort_by_id: bool) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !ignore.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), normalize_with(v, ignore, sort_by_id)))
                .collect(),
        ),
        Value::Array(items) => {
            let mut out: Vec<Value> = items
                .iter()
                .map(|v| normalize_with(v, ignore, sort_by_id))
                .collect();
            if sort_by_id && out.iter().all(|v| v.get("id").and_then(Value::as_i64).is_some()) {
                out.sort_by_key(|v| v["id"].as_i64());
            } else if sort_by_id
                && out
                    .iter()
                    .all(|v| v.get("source").and_then(Value::as_str).is_some())
            {
                out.sort_by_key(|v| v["source"].as_str().map(String::from));
            }
            Value::Array(out)
        }
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => Value::from(f as i64),
            _ => v.clone(),
        },
        _ => v.clone(),
    }
}

/// Key paths of a JSON tree, arrays reduced to their first element.
fn shape(v: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                let path = format!("{prefix}/{k}");
                out.insert(path.clone());
                shape(v, &path, out);
            }
        }
        Value::Array(items) => {
            out.insert(format!("{prefix}[]"));
            if let Some(first) = items.first() {
                shape(first, &format!("{prefix}[]"), out);
            }
        }
        _ => {}
    }
}

/// Describe the first few differences between two normalised values.
fn diff(path: &str, want: &Value, got: &Value, out: &mut Vec<String>) {
    if out.len() >= 12 {
        return;
    }
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                match g.get(k) {
                    None => out.push(format!("{path}/{k}: missing (expected {})", short(wv))),
                    Some(gv) => diff(&format!("{path}/{k}"), wv, gv, out),
                }
            }
            for k in g.keys() {
                if !w.contains_key(k) {
                    out.push(format!("{path}/{k}: unexpected key"));
                }
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            if w.len() != g.len() {
                out.push(format!("{path}: {} items expected, {} got", w.len(), g.len()));
                for extra in g.iter().skip(w.len()) {
                    out.push(format!("{path}[+]: unexpected {}", short(extra)));
                }
                for missing in w.iter().skip(g.len()) {
                    out.push(format!("{path}[-]: missing {}", short(missing)));
                }
            }
            for (i, (wv, gv)) in w.iter().zip(g).enumerate() {
                diff(&format!("{path}[{i}]"), wv, gv, out);
            }
        }
        _ => {
            if want != got {
                out.push(format!("{path}: expected {}, got {}", short(want), short(got)));
            }
        }
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 80 {
        format!("{}...", &s[..80])
    } else {
        s
    }
}

struct Golden {
    name: String,
    /// `sleep` for a pause in the recording: the replay waits for the
    /// command queue instead.
    method: String,
    /// A `mock` line: sent to the mock services, not to Unarr.
    to_mock: bool,
    path: String,
    /// JSON request body, from the file the fourth column names.
    request_body: Option<String>,
    status: u16,
    body: Value,
    /// When the real application answered; Unarr's clock is pinned to it.
    recorded_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn load_goldens() -> Vec<Golden> {
    let golden_dir = fixture_dir().join("golden");
    let list = std::fs::read_to_string(golden_dir.join("requests.txt")).expect("requests.txt");
    let mut out = Vec::new();
    for line in list.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `sleep N` lines pace the recording while the originals' commands
        // ran; the replay waits for Unarr's instead.
        if let Some(seconds) = line.strip_prefix("sleep ") {
            out.push(Golden {
                name: String::new(),
                method: "sleep".into(),
                to_mock: false,
                path: seconds.trim().to_string(),
                request_body: None,
                status: 0,
                body: Value::Null,
                recorded_at: None,
            });
            continue;
        }
        // `mock <name> METHOD path [body]`: for the mock services; `-` as
        // the name means the answer was not recorded.
        let (to_mock, line) = match line.strip_prefix("mock ") {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        let mut parts = line.splitn(4, ' ');
        let name = parts.next().unwrap().to_string();
        let method = parts.next().unwrap().to_string();
        let path = parts.next().unwrap().to_string();
        let body = parts.next().map(|body_file| {
            std::fs::read_to_string(golden_dir.join(body_file.trim()))
                .unwrap_or_else(|e| panic!("body file {body_file} for {name}: {e}"))
        });
        if to_mock && name == "-" {
            out.push(Golden {
                name,
                method,
                to_mock,
                path,
                request_body: body,
                status: 0,
                body: Value::Null,
                recorded_at: None,
            });
            continue;
        }
        let file = golden_dir.join(format!("{name}.json"));
        let Ok(text) = std::fs::read_to_string(&file) else {
            panic!("no golden recorded for {name}: run `make golden`");
        };
        let recorded: Value = serde_json::from_str(&text).unwrap();
        out.push(Golden {
            name,
            method,
            to_mock,
            path,
            request_body: body,
            status: recorded["status"].as_u64().unwrap() as u16,
            body: recorded["body"].clone(),
            recorded_at: recorded["recordedAt"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&chrono::Utc)),
        });
    }
    out
}

/// Log in through the form as the fixture user and return the session cookie.
async fn login_cookie(app: &axum::Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=unarr&password=fixture&rememberMe=on"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND, "login should redirect");
    let cookie = response
        .headers()
        .get("set-cookie")
        .expect("login sets the session cookie")
        .to_str()
        .unwrap()
        .to_string();
    cookie.split(';').next().unwrap().to_string()
}

/// `{id:name}` in a path: the id the named response carried.
fn substitute_ids(path: &str, ids: &std::collections::HashMap<String, i64>) -> String {
    let mut path = path.to_string();
    while let Some(start) = path.find("{id:") {
        let end = path[start..]
            .find('}')
            .map(|e| start + e)
            .expect("a closing brace");
        let name = &path[start + 4..end];
        let id = ids
            .get(name)
            .unwrap_or_else(|| panic!("no id recorded for {name} before {path}"));
        path = format!("{}{}{}", &path[..start], id, &path[end + 1..]);
    }
    path
}

/// The recording's `/tv` and `/downloads` in a request's path or body
/// become the replay directories: after `=` or `"`, up to the next `/`,
/// `"`, `&` or the end, so that `/downloads/tv/...` maps once.
fn map_paths(text: &str, mappings: &[(String, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'outer: while !rest.is_empty() {
        for (replay, recorded) in mappings {
            for lead in ['=', '"'] {
                let needle = format!("{lead}{recorded}");
                if let Some(after) = rest.strip_prefix(needle.as_str()) {
                    if after.is_empty() || after.starts_with(['/', '"', '&']) {
                        out.push(lead);
                        out.push_str(replay);
                        rest = after;
                        continue 'outer;
                    }
                }
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
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
    state.start_runtime().await.expect("the command runtime");
    // The recording's one NZBGet grab got this drone id from the originals'
    // random GUID; the queue and history goldens name it.
    unarr_core::downloadclients::nzbget::pin_drone_ids(&["c4119cd06a044d60be1043396370761b"]);
    let app = unarr_api::router(state.clone());
    let goldens = load_goldens();
    let session = login_cookie(&app).await;
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
        let (status, body) = send(&app, g, &session, &path, &mappings).await;
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
        if let Some(id) = body.get("id").and_then(Value::as_i64) {
            ids.insert(g.name.clone(), id);
        }
        if status.as_u16() != g.status {
            failures.push(format!(
                "{}: status {} expected, {} got",
                g.name, g.status, status
            ));
            continue;
        }
        let mut problems = Vec::new();
        match mode(&g.name) {
            Mode::Exact(ignore) => {
                let want = normalize(&g.body, ignore);
                let got = normalize(&body, ignore);
                diff("", &want, &got, &mut problems);
            }
            Mode::Unordered(ignore) => {
                let want = normalize_with(&g.body, ignore, true);
                let got = normalize_with(&body, ignore, true);
                diff("", &want, &got, &mut problems);
            }
            Mode::Multiset(ignore) => {
                let want = sorted_list(normalize(&g.body, ignore));
                let got = sorted_list(normalize(&body, ignore));
                diff("", &want, &got, &mut problems);
            }
            Mode::ManualCommands(ignore) => {
                let manual = |v: &Value| -> Value {
                    Value::Array(
                        v.as_array()
                            .map(|a| a.iter().filter(|c| c["trigger"] == "manual").cloned().collect())
                            .unwrap_or_default(),
                    )
                };
                let want = normalize(&manual(&g.body), ignore);
                let got = normalize(&manual(&body), ignore);
                diff("", &want, &got, &mut problems);
            }
            Mode::Html => {
                let title = |v: &Value| {
                    v.as_str()
                        .and_then(|s| s.split("<title>").nth(1))
                        .and_then(|s| s.split("</title>").next())
                        .map(str::to_string)
                };
                if title(&g.body).is_none() || title(&g.body) != title(&body) {
                    problems.push(format!(
                        "html title: expected {:?}, got {:?}",
                        title(&g.body),
                        title(&body)
                    ));
                }
            }
            Mode::Shape => {
                let (mut want, mut got) = (BTreeSet::new(), BTreeSet::new());
                shape(&g.body, "", &mut want);
                shape(&body, "", &mut got);
                for k in want.difference(&got) {
                    problems.push(format!("{k}: missing"));
                }
                for k in got.difference(&want) {
                    problems.push(format!("{k}: unexpected key"));
                }
            }
        }
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
    let names: BTreeSet<String> = load_goldens()
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

/// A URL base with capitals keeps them; only the API part is case-insensitive.
#[tokio::test]
async fn url_base_keeps_its_case() {
    let app = unarr_api::router(build_state_with(|h| h.url_base = "/Sonarr".into()));
    for (uri, expected) in [
        ("/Sonarr/api/v3/health", StatusCode::OK),
        ("/Sonarr/api/v3/episodeFile?seriesId=22", StatusCode::OK),
        ("/sonarr/api/v3/health", StatusCode::NOT_FOUND),
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
