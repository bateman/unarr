//! Contract tests of the Radarr personality: replay the requests recorded
//! from the real Radarr (`tests/fixtures/radarr/golden/`) against Unarr on
//! the imported fixture and compare status and JSON body.
//!
//! The comparison modes are those of the Sonarr replay (`tests/common`).
//! The reads of milestone 4a are answered from the import; the writes of
//! 4b go against the Rust mock's movie indexers and download clients on
//! the port the recording used. Unarr's clock is pinned to the instant
//! each golden was recorded and moves across the recording's sleeps.

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
/// work lands.
const PENDING: &[&str] = &[];
const PENDING_PREFIXES: &[&str] = &[];

fn pending(name: &str) -> bool {
    PENDING.contains(&name) || PENDING_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// What of a command is the moment's: its id and its clocks.
const COMMAND_CLOCKS: &[&str] = &[
    "id",
    "queued",
    "started",
    "ended",
    "duration",
    "stateChangeTime",
    "lastExecutionTime",
];

/// The same for a command just posted, which may or may not have started.
const COMMAND_STARTING: &[&str] = &[
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
];

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
        n if n.ends_with("-404") || n == "downloadclient-update-missing" || n == "command-get-missing" => {
            Mode::Exact(&["traceId"])
        }
        // Commands carry the original's ids and clocks; a freshly posted
        // one may already have started, or finished, when it is read back.
        "command-list" => Mode::ManualCommands(COMMAND_CLOCKS),
        "command-unknown" | "command-series-search" => Mode::Shape,
        "command-cancel-completed" | "command-cancel-missing" => Mode::Exact(&["description"]),
        "command-missing-name" => Mode::Exact(&[]),
        n if n.starts_with("command-") && n.ends_with("-get") => Mode::Exact(COMMAND_CLOCKS),
        n if n.starts_with("command-") => Mode::Exact(COMMAND_STARTING),
        // Which of the scheduled tasks ran meanwhile is the recording's.
        "system-task-after" => Mode::Shape,
        // Ages in hours and minutes carry the fraction of a second the
        // recording happened at; the original's errors carry its stack.
        n if n.starts_with("release") => Mode::Exact(&["ageHours", "ageMinutes", "description"]),
        // The queue keeps the order of the original's cache of tracked
        // downloads, a hash table's; what is left of a download counts from
        // the refresh that read it, a debounced one there.
        // A pending release counts down to the fraction of a second.
        n if n.starts_with("queue-pending") || n.starts_with("queue-details-pending") => {
            Mode::Unordered(&["estimatedCompletionTime", "lastSearchTime", "timeleft", "added"])
        }
        // A command stamps what it grabs when it runs, which in the replay
        // is on either side of the recording's pause.
        "queue-after-command" | "queue-after-pending" | "queue-after-fail" => {
            Mode::Unordered(&["estimatedCompletionTime", "lastSearchTime", "added"])
        }
        n if n.starts_with("queue") => Mode::Unordered(&["estimatedCompletionTime", "lastSearchTime"]),
        // The age a grab records is the release's at that instant; a
        // search stamps the movie when it starts, the replay when it is
        // asked.
        // The failure a removal records is stamped a moment before the
        // answer the recorder timed, which can be the second before.
        "history-after-delete" => Mode::Exact(&["date", "lastSearchTime"]),
        // Stamped by a command that ran while the recording paused.
        "blocklist-after-fail" | "blocklist-after-bulk" => Mode::Exact(&["date", "lastSearchTime"]),
        "history-after-command" | "history-after-pending" | "history-after-fail" | "history-after-bulk" => {
            Mode::Exact(&["date", "age", "ageHours", "ageMinutes", "lastSearchTime"])
        }
        // The original keeps its verdicts in a hash table.
        "health-after" => Mode::Multiset(&[]),
        n if n.starts_with("history") => Mode::Exact(&["ageHours", "ageMinutes", "lastSearchTime"]),
        // The socket error text in the detail is the runtime's (.NET's
        // "Connection refused (127.0.0.1:1)" against Rust's).
        n if n.starts_with("downloadclient-unreachable") || n == "downloadclient-testall" => {
            Mode::Exact(&["detailedDescription"])
        }
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
    // The mock indexers and clients the write goldens were recorded
    // against, on the port the recorded provider settings carry.
    let mock = unarr_mock::Mock::start(unarr_mock::fixtures_dir(), 18989)
        .await
        .expect("port 18989 free for the mock indexers (stop a stale scripts/mock-servarr.py)");
    let state = build_state();
    let goldens = load_goldens(&fixture_dir().join("golden"));
    // The free space specification reads the disk the movies would land on,
    // here the one under `/movies`: the goldens were recorded with room for
    // their eight gigabyte releases, and need the same to replay.
    let free = Path::new("/movies")
        .ancestors()
        .find_map(unarr_core::fs::free_space)
        .unwrap_or(i64::MAX);
    assert!(
        free > 9 * 1024 * 1024 * 1024,
        "the Radarr goldens need 9 GB free where /movies would be, found {} GB: \
         the releases they approve would be rejected for want of disk space",
        free / (1024 * 1024 * 1024)
    );
    state.start_runtime().await.expect("the runtime");
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;
    let mut failures = Vec::new();
    let mut checked = 0;
    // `UNARR_CONTRACT_ONLY=prefix,prefix` replays part of the list.
    let only: Vec<String> = std::env::var("UNARR_CONTRACT_ONLY")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();

    let mut ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    // What the hub would send the UI while the goldens replay.
    let mut hub = state.hub().subscribe();
    let mut broadcasts: Vec<Value> = Vec::new();
    for g in &goldens {
        while let Ok(message) = hub.try_recv() {
            broadcasts.push(message);
        }
        if g.method == "sleep" {
            // The recording's clock moved on while the original's background
            // work ran; what Unarr does meanwhile sees the same clock.
            let seconds: i64 = g.path.parse().expect("sleep seconds");
            state.set_now(Some(state.now() + chrono::Duration::seconds(seconds)));
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().expect("the runtime").wait_idle(),
            )
            .await
            .expect("commands finish within a minute");
            continue;
        }
        // `UNARR_CONTRACT_ALL=1` replays the pending ones too, to see how far
        // each is.
        if pending(&g.name) && std::env::var_os("UNARR_CONTRACT_ALL").is_none() {
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
        let mut g = g.clone();
        g.path = substitute_ids(&g.path, &ids);
        // `"{id:name}"` in a body is that id, as a number.
        if let Some(body) = &g.request_body {
            let mut body = body.clone();
            for (name, id) in &ids {
                body = body.replace(&format!("\"{{id:{name}}}\""), &id.to_string());
            }
            g.request_body = Some(body);
        }
        let g = &g;
        if g.to_mock {
            // A control call to the mock services: a download failed.
            let body = g.request_body.as_deref().unwrap_or("").as_bytes();
            let (status, _) = mock.call(&g.method, &g.path, body);
            assert!(
                status.is_success(),
                "mock {} {} answered {status}",
                g.method,
                g.path
            );
            checked -= 1;
            continue;
        }
        let (status, mut body) = send(&app, g, &session).await;
        // The root folder check sees the machine: where `/movies` cannot
        // exist (the recording made it) its warning is the machine's, not
        // the code's, as the disk space is off Linux.
        if g.name == "health-after" && !Path::new("/movies").exists() {
            if let Some(verdicts) = body.as_array_mut() {
                verdicts.retain(|v| v["source"] != "RootFolderCheck");
            }
        }
        // As the recorder: the answer's id, or its first item's.
        if let Some(id) = body
            .get("id")
            .or_else(|| body.get(0).and_then(|first| first.get("id")))
            .and_then(Value::as_i64)
        {
            ids.insert(g.name.clone(), id);
        }
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

    // The SignalR messages the UI follows a grab by: the command's states
    // with its last message, and the queue told to read itself again with
    // its new count. Nothing under a name only Sonarr's UI knows.
    if only.is_empty() && std::env::var_os("UNARR_CONTRACT_ALL").is_none() {
        while let Ok(message) = hub.try_recv() {
            broadcasts.push(message);
        }
        let named = |name: &str| -> Vec<&Value> { broadcasts.iter().filter(|m| m["name"] == name).collect() };
        let command_ended = |command: &str, message: &str| {
            named("command").iter().any(|m| {
                let resource = &m["body"]["resource"];
                m["body"]["action"] == "updated"
                    && resource["name"] == command
                    && resource["status"] == "completed"
                    && resource["message"] == message
            })
        };
        assert!(
            command_ended(
                "RssSync",
                "RSS Sync Completed. Reports found: 14, Reports grabbed: 1"
            ),
            "no SignalR message for the RSS sync's end: {:?}",
            named("command")
        );
        assert!(
            command_ended(
                "MoviesSearch",
                "Completed search for 1 movies. 1 reports downloaded."
            ),
            "no SignalR message for the search's end"
        );
        for started in ["RssSync", "MoviesSearch"] {
            assert!(
                named("command")
                    .iter()
                    .any(|m| m["body"]["resource"]["name"] == started
                        && m["body"]["resource"]["status"] == "started"),
                "no SignalR message for the start of {started}"
            );
        }
        assert!(
            named("queue").iter().any(|m| m["body"]["action"] == "sync"),
            "the queue was never told to sync"
        );
        assert!(
            named("queue/details")
                .iter()
                .any(|m| m["body"]["action"] == "sync"),
            "the queue details were never told to sync"
        );
        // Three downloads and two pending releases at the fullest.
        assert!(
            named("queue/status")
                .iter()
                .any(|m| m["body"]["action"] == "updated" && m["body"]["resource"]["totalCount"] == 5),
            "no queue status with the five items: {:?}",
            named("queue/status")
        );
        assert!(named("health").iter().any(|m| m["body"]["action"] == "sync"));
        for name in ["series", "episode", "episodefile", "calendar", "wanted/missing"] {
            assert!(named(name).is_empty(), "a {name} message from Radarr");
        }
    }

    // What Unarr asked the mock indexers, against what the real Radarr
    // asked them while the goldens were recorded (the distinct requests of
    // the Python mock's log): capabilities, the movie feed over the
    // configured categories, a search by the ids each indexer takes, and
    // the text search where the ids found nothing.
    if only.is_empty() && std::env::var_os("UNARR_CONTRACT_ALL").is_none() {
        let asked: BTreeSet<String> = mock
            .requests()
            .iter()
            .filter(|r| r.path.starts_with("/movies/") && r.path.ends_with("/api"))
            .map(|r| {
                let mut line = r.path.clone();
                for key in ["t", "cat", "offset", "limit", "tmdbid", "imdbid", "q"] {
                    if let Some(value) = r.query.get(key).filter(|v| !v.is_empty()) {
                        line.push_str(&format!(" {key}={value}"));
                    }
                }
                line
            })
            .collect();
        let page = "cat=2000,2010,2020,2030,2040,2045,2050,2060 offset=0 limit=100";
        let mut expected: BTreeSet<String> = ["empty", "newznab", "torznab"]
            .iter()
            .flat_map(|kind| {
                [
                    format!("/movies/{kind}/api t=caps"),
                    format!("/movies/{kind}/api t=movie {page}"),
                ]
            })
            .collect();
        expected.extend([
            format!("/movies/newznab/api t=movie {page} tmdbid=840430 imdbid=14849194"),
            format!("/movies/torznab/api t=movie {page} imdbid=14849194"),
            format!("/movies/newznab/api t=movie {page} tmdbid=597208 imdbid=7740496"),
            format!("/movies/torznab/api t=movie {page} imdbid=7740496"),
            format!("/movies/newznab/api t=search {page} q=Nightmare Alley 2021"),
            format!("/movies/torznab/api t=search {page} q=Nightmare Alley 2021"),
        ]);
        assert_eq!(asked, expected, "the requests to the mock indexers");
    }

    // Outside the goldens' bodies: a test that passes answers the string
    // `{}`, as plain text, or as a JSON string to a client that asks for
    // JSON (Prowlarr). Probed on the real Radarr; the recorder asks for
    // neither and decodes both to the same golden.
    if only.is_empty() {
        let test = goldens
            .iter()
            .find(|g| g.name == "indexer-test")
            .expect("indexer-test");
        for (accept, content_type, text) in [
            (None, "text/plain; charset=utf-8", "{}"),
            (
                Some("application/json"),
                "application/json; charset=utf-8",
                "\"{}\"",
            ),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v3/indexer/test")
                .header("X-Api-Key", API_KEY)
                .header("content-type", "application/json");
            if let Some(accept) = accept {
                request = request.header("accept", accept);
            }
            let body = Body::from(test.request_body.clone().expect("a body"));
            let response = app.clone().oneshot(request.body(body).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["content-type"], content_type);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(bytes, text.as_bytes(), "Accept: {accept:?}");
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
    for p in PENDING_PREFIXES {
        assert!(
            names.iter().any(|n| n.starts_with(p)),
            "PENDING_PREFIXES names no golden: {p}"
        );
    }
}
