//! Contract tests: replay the requests recorded from the real Sonarr
//! (`tests/fixtures/sonarr/golden/`) against Unarr on the imported fixture
//! and compare status and JSON body.
//!
//! Each golden has a comparison mode. `Exact` compares the whole body after
//! removing the listed volatile keys (matched by name anywhere in the tree).
//! `Shape` compares only the key structure, for responses whose values depend
//! on the machine or the moment (disk space, logs, task timings).
//! Goldens listed in `PENDING` are not implemented yet and are skipped.

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

/// Goldens not yet implemented. Shrinks as phase 1 progresses; empty at exit.
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
    /// Key structure only.
    Shape,
    /// An HTML page: status and the page title.
    Html,
}

fn mode(name: &str) -> Mode {
    match name {
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

fn build_state() -> AppState {
    build_state_with(|_| {})
}

/// The fixture imported, with `config.xml` adjusted by `adjust`.
fn build_state_with(adjust: impl FnOnce(&mut HostConfig)) -> AppState {
    let dir = fixture_dir();
    let db = Db::open_in_memory().expect("in-memory db");
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
    let mut host = unarr_import::config_xml::read(&dir.join("config.xml"), 8989).expect("config.xml");
    adjust(&mut host);
    let settings = Settings::from_host_config(host, Personality::Sonarr);
    let runtime = RuntimeInfo::detect(dir);
    let logs = LogBuffer::new();
    logs.push("info", "unarr", "contract test".into(), None);
    AppState::new(Personality::Sonarr, db, settings, runtime, logs)
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
                path: seconds.trim().to_string(),
                request_body: None,
                status: 0,
                body: Value::Null,
                recorded_at: None,
            });
            continue;
        }
        let mut parts = line.splitn(4, ' ');
        let name = parts.next().unwrap().to_string();
        let method = parts.next().unwrap().to_string();
        let path = parts.next().unwrap().to_string();
        let body = parts.next().map(|body_file| {
            std::fs::read_to_string(golden_dir.join(body_file.trim()))
                .unwrap_or_else(|e| panic!("body file {body_file} for {name}: {e}"))
        });
        let file = golden_dir.join(format!("{name}.json"));
        let Ok(text) = std::fs::read_to_string(&file) else {
            panic!("no golden recorded for {name}: run `make golden`");
        };
        let recorded: Value = serde_json::from_str(&text).unwrap();
        out.push(Golden {
            name,
            method,
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

async fn send(app: &axum::Router, g: &Golden, session: &str, path: &str) -> (StatusCode, Value) {
    let mut req = Request::builder().method(g.method.as_str()).uri(path);
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

/// The mock indexers the write goldens were recorded against, on the port
/// the recorded provider settings carry.
async fn start_mock() -> unarr_mock::Mock {
    unarr_mock::Mock::start(unarr_mock::fixtures_dir(), 18989)
        .await
        .expect("port 18989 free for the mock indexers (stop a stale scripts/mock-servarr.py)")
}

#[tokio::test]
async fn golden_responses_match() {
    let _mock = start_mock().await;
    let state = build_state();
    state.start_runtime().await.expect("the command runtime");
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
        checked += 1;
        state.set_now(g.recorded_at);
        let path = substitute_ids(&g.path, &ids);
        let (status, body) = send(&app, g, &session, &path).await;
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
}

#[tokio::test]
async fn pending_list_only_names_recorded_goldens() {
    let names: BTreeSet<String> = load_goldens()
        .into_iter()
        .filter(|g| g.method != "sleep")
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
