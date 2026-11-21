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

/// The same for a command the original fails, whose exception names its
/// stack.
const COMMAND_FAILING: &[&str] = &[
    "message",
    "id",
    "queued",
    "started",
    "ended",
    "duration",
    "stateChangeTime",
    "lastExecutionTime",
    "status",
    "result",
    "exception",
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
        // The notifications of milestone 4c. A target that does not
        // resolve or refuses the connection: the original lets the
        // exception escape as a 500 that carries its type and its stack.
        "notification-testall" | "notification-test-webhook-unreachable" => Mode::Shape,
        "notification-update-missing" => Mode::Exact(&["traceId"]),
        "blocklist-protocol-invalid"
        | "blocklist-protocol-comma"
        | "blocklist-protocol-empty"
        | "blocklist-movie-ids-invalid" => Mode::Exact(&["traceId"]),
        // What the notification targets received. The Discord test message
        // names the local time it was posted at.
        "notifications-after-test" | "notifications-after-testall" => Mode::Exact(&["content"]),
        // The original performs the Plex update a deletion through the
        // API queued in the background, wherever that falls among the
        // requests that follow.
        "delete-notifications" | "delete-notifications-mapped" => Mode::Multiset(&["timestamp", "dateAdded"]),
        // Discord embeds carry the moment they were built, webhook payloads
        // the moment their file was added: both fall, in the replay, on
        // the other side of the recording's pause.
        n if n.ends_with("-notifications") => Mode::Exact(&["timestamp", "dateAdded"]),
        // The import of milestone 4c: a refresh the recording posted and
        // read back after a pause runs, in the replay, during the pause,
        // so what it stamps (the file, the history row, the grab's age) is
        // on the other side of it; the queue's order is a hash table's.
        "import-moviefile-missing" => Mode::Exact(&["traceId"]),
        // The manual import of milestone 4c. A `ModelNotFoundException`
        // and the exception the original lets escape when it evaluates
        // again a file nothing can be parsed from carry their stack.
        "manualimport-reprocess" | "manualimport-reprocess-nomovie" | "moviefile-delete-missing" => {
            Mode::Exact(&["description"])
        }
        "manualimport-queue-after" => {
            Mode::Unordered(&["estimatedCompletionTime", "lastSearchTime", "added", "timeleft"])
        }
        "manualimport-history-movie-62" => Mode::Exact(&["date", "ageHours", "ageMinutes"]),
        // The items' ids hash their path, which the replay moves; the
        // file a movie shows was imported while the recording paused.
        n if n.starts_with("manualimport-") => Mode::Exact(&["id", "dateAdded"]),
        // The file the manual import brought, edited and deleted: it was
        // added, and the deletions before it were recorded, by commands
        // that ran while the recording paused; so was the search that
        // stamped the other movie.
        n if n.starts_with("moviefile-put")
            || n == "moviefile-after-put"
            || n == "moviefile-editor"
            || n == "moviefile-bulk-put" =>
        {
            Mode::Exact(&["dateAdded"])
        }
        "delete-history" => Mode::Exact(&["date"]),
        "delete-movie-70" => Mode::Exact(&["lastSearchTime"]),
        // The file of the movie a title found was imported while the
        // recording paused.
        "parse-release" => Mode::Exact(&["dateAdded"]),
        // The folder browser shows when the disk last changed. The parent
        // of the replay's stand-in for `/movies` is not the root.
        "filesystem-movies" => Mode::Exact(&["lastModified", "parent"]),
        n if n.starts_with("filesystem-") => Mode::Exact(&["lastModified"]),
        // The rename previews and commands.
        "rename-preview-missing" => Mode::Shape,
        "rename-preview-nomovie" => Mode::Exact(&["traceId"]),
        n if n.starts_with("command-rename") => {
            if n.ends_with("-get") {
                Mode::Exact(COMMAND_CLOCKS)
            } else {
                Mode::Exact(COMMAND_STARTING)
            }
        }
        n if n.starts_with("rename-") => Mode::Exact(&[
            "date",
            "dateAdded",
            "age",
            "ageHours",
            "ageMinutes",
            "lastSearchTime",
        ]),
        // The library's commands of milestone 4c, and what they leave.
        // The original's exception carries its type and its stack.
        "command-rescan-missing-get" => Mode::Exact(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
            "exception",
        ]),
        n if n.starts_with("command-rescan")
            || n.starts_with("command-scan")
            || n.starts_with("command-housekeeping")
            || n.starts_with("command-cleanup")
            || n.starts_with("command-backup") =>
        {
            if n.ends_with("-get") {
                Mode::Exact(COMMAND_CLOCKS)
            } else {
                Mode::Exact(COMMAND_STARTING)
            }
        }
        "scan-queue-after" => {
            Mode::Unordered(&["estimatedCompletionTime", "lastSearchTime", "added", "timeleft"])
        }
        // The tasks the library's commands ran, by everything but when.
        "system-task-after-library" => {
            Mode::Exact(&["lastExecution", "lastStartTime", "nextExecution", "lastDuration"])
        }
        // The backup's name carries the second it was made at.
        "system-backup-after" => Mode::Exact(&["id", "name", "path", "size", "time"]),
        n if n.starts_with("rescan-") || n.starts_with("scan-") => Mode::Exact(&[
            "date",
            "dateAdded",
            "age",
            "ageHours",
            "ageMinutes",
            "lastSearchTime",
        ]),
        n if (n.starts_with("import-") || n.starts_with("upgrade-")) && n.ends_with("-get") => {
            Mode::Exact(COMMAND_CLOCKS)
        }
        n if n.starts_with("import-refresh") || n == "upgrade-refresh" => Mode::Exact(COMMAND_STARTING),
        n if n.starts_with("import-queue") || n.starts_with("upgrade-queue") => {
            Mode::Unordered(&["estimatedCompletionTime", "lastSearchTime", "added", "timeleft"])
        }
        "import-health" | "health-after-library" => Mode::Multiset(&[]),
        "import-push-upgrade" => Mode::Exact(&["ageHours", "ageMinutes", "description"]),
        n if n.starts_with("import-") || n.starts_with("upgrade-") => Mode::Exact(&[
            "date",
            "dateAdded",
            "age",
            "ageHours",
            "ageMinutes",
            "lastSearchTime",
        ]),
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
        "history-mark-failed-missing" => Mode::Exact(&["description"]),
        "history-after-delete" => Mode::Exact(&["date", "lastSearchTime"]),
        // Stamped by a command that ran while the recording paused.
        "blocklist-after-fail" | "blocklist-after-bulk" | "blocklist-after-mark-failed" => {
            Mode::Exact(&["date", "lastSearchTime"])
        }
        "history-after-command"
        | "history-after-pending"
        | "history-after-fail"
        | "history-after-bulk"
        | "history-after-mark-failed" => {
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
        // The original lists the file written last first, which of the two
        // that is at the moment of the recording.
        "log-file" => Mode::Multiset(&["lastWriteTime", "contentsUrl", "id"]),
        _ => Mode::Exact(&[]),
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/radarr")
}

/// The goldens were recorded with the ffprobe the pinned Radarr ships,
/// which `make ui` leaves under `.cache`; the replay probes with the same
/// build when it is there, else with `UNARR_FFPROBE` or the `PATH`'s.
fn use_bundled_ffprobe() {
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/radarr/Radarr/ffprobe");
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

/// The directories the replay stands in for the recording's `/movies` and
/// `/downloads`, under one temporary root.
struct ReplayDirs {
    root: PathBuf,
    movies: PathBuf,
    downloads: PathBuf,
}

impl ReplayDirs {
    fn create() -> ReplayDirs {
        ReplayDirs::create_named("replay")
    }

    /// Tests run side by side in one process: each takes its own name.
    fn create_named(name: &str) -> ReplayDirs {
        let root = std::env::temp_dir().join(format!("unarr_contract_radarr_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let movies = root.join("movies");
        let downloads = root.join("downloads");
        std::fs::create_dir_all(&movies).expect("temp movies dir");
        std::fs::create_dir_all(&downloads).expect("temp downloads dir");
        ReplayDirs {
            root,
            movies,
            downloads,
        }
    }

    /// `(replay path, recorded path)` pairs.
    fn mappings(&self) -> Vec<(String, &'static str)> {
        vec![
            (self.downloads.to_string_lossy().into_owned(), "/downloads"),
            (self.movies.to_string_lossy().into_owned(), "/movies"),
        ]
    }
}

impl Drop for ReplayDirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The fixture imported into an in-memory database with Radarr's schema,
/// its library moved under `dirs`.
fn build_state(dirs: &ReplayDirs) -> AppState {
    use_bundled_ffprobe();
    let dir = fixture_dir();
    let db = Db::open_in_memory_for(Personality::Radarr).expect("in-memory db");
    unarr_import::radarr::import(&dir.join("radarr.db"), &db).expect("import fixture");
    let movies = dirs.movies.to_string_lossy().into_owned();
    db.call_blocking(move |conn| {
        conn.execute(
            "UPDATE root_folders SET path = ?1 WHERE path = '/movies'",
            [&movies],
        )?;
        conn.execute(
            "UPDATE movies SET path = ?1 || substr(path, 8) WHERE path LIKE '/movies/%'",
            [&movies],
        )?;
        conn.execute(
            "UPDATE collections SET root_folder_path = ?1 || substr(root_folder_path, 8)
              WHERE root_folder_path = '/movies' OR root_folder_path LIKE '/movies/%'",
            [&movies],
        )?;
        // The import list's root folder names the same path.
        conn.execute(
            "UPDATE providers SET settings = replace(settings, '\"/movies\"', '\"' || ?1 || '\"'),
                attributes = replace(attributes, '\"/movies\"', '\"' || ?1 || '\"')",
            [&movies],
        )?;
        Ok(())
    })
    .expect("move the library under the replay directory");
    let host = unarr_import::config_xml::read(&dir.join("config.xml"), Personality::Radarr.default_port())
        .expect("config.xml");
    let settings = Settings::from_host_config(host, Personality::Radarr);
    // An application data folder of its own, with the log files the
    // original had written by `log-file`: written at one instant, so the
    // list (last written first) keeps the order of their names.
    let app_data = dirs.root.join("appdata");
    let logs_dir = app_data.join("logs");
    std::fs::create_dir_all(&logs_dir).expect("app data");
    // The backup the replay makes takes the configuration file with it.
    std::fs::copy(dir.join("config.xml"), app_data.join("config.xml")).expect("config.xml copy");
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
    runtime.media_cover_dir = dirs.root.join("MediaCover");
    let logs = LogBuffer::new();
    logs.push("info", "unarr", "contract test".into(), None);
    AppState::new(Personality::Radarr, db, settings, runtime, logs)
}

async fn send(
    app: &axum::Router,
    g: &Golden,
    session: &str,
    mappings: &[(String, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(g.method.as_str())
        .uri(map_paths(&g.path, mappings));
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

#[tokio::test]
async fn radarr_golden_responses_match() {
    // The mock indexers and clients the write goldens were recorded
    // against, on the port the recorded provider settings carry.
    let dirs = ReplayDirs::create();
    let mappings = dirs.mappings();
    let mock = unarr_mock::Mock::start_with(
        unarr_mock::fixtures_dir(),
        18989,
        unarr_mock::MockDirs {
            downloads: Some(dirs.downloads.clone()),
            tv: None,
            movies: Some(dirs.movies.clone()),
        },
    )
    .await
    .expect("port 18989 free for the mock indexers (stop a stale scripts/mock-servarr.py)");
    // Radarr's Discord embeds are signed with the instance's name, not the
    // machine's as Sonarr's are: nothing of the recording machine to pin.
    let state = build_state(&dirs);
    let goldens = load_goldens(&fixture_dir().join("golden"));
    // The free space specification reads the disk the movies would land on,
    // here the replay's stand-in for `/movies`: the goldens were recorded
    // with room for their eight gigabyte releases, and need the same to
    // replay.
    let free = dirs
        .movies
        .ancestors()
        .find_map(unarr_core::fs::free_space)
        .unwrap_or(i64::MAX);
    assert!(
        free > 9 * 1024 * 1024 * 1024,
        "the Radarr goldens need 9 GB free in the temporary directory, found {} GB: \
         the releases they approve would be rejected for want of disk space",
        free / (1024 * 1024 * 1024)
    );
    // The recorder marks every scheduled task as just run before it starts
    // (scripts/radarr-fixture.sh), so that no health check, RSS sync or
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
    // The same messages by stretch of the list: a stretch ends at each
    // `sleep`, where the recording waited for the original's background
    // work and the replay waits for Unarr's.
    let mut stretches: Vec<Vec<Value>> = vec![Vec::new()];
    for g in &goldens {
        while let Ok(message) = hub.try_recv() {
            stretches.last_mut().unwrap().push(message.clone());
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
            // What the wait produced belongs to the stretch it ends. The
            // relay to the hub is a task of its own, and a scan of the whole
            // library hands it hundreds of events: wait until it is quiet.
            for message in drain_hub(&mut hub).await {
                stretches.last_mut().unwrap().push(message.clone());
                broadcasts.push(message);
            }
            stretches.push(Vec::new());
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
            // A control call to the mock services (a download failed or
            // completed, a file put on disk), naming the recording's
            // directories; or, under a name, what the mock holds, compared
            // as a golden: what the notification targets received.
            let mut path = g.path.clone();
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
                checked -= 1;
                continue;
            }
            failures.extend(mock_differences(mode(&g.name), g, status, &bytes, &mappings));
            continue;
        }
        let (status, body) = send(&app, g, &session, &mappings).await;
        let body = unmap_paths(&body, &mappings);
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
        // The message of a folder that cannot be written names the user
        // the process runs as: root where the recording was taken,
        // another on a CI runner. The rest of the answer is compared.
        let expected = if g.name == "config-mediamanagement-put-bin-missing" {
            as_current_user(&g.body)
        } else {
            g.body.clone()
        };
        let problems = compare(mode(&g.name), &expected, &body);
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
        for message in drain_hub(&mut hub).await {
            stretches.last_mut().unwrap().push(message.clone());
            broadcasts.push(message);
        }
        let problems = signalr_differences(&fixture_dir().join("golden"), &goldens, &stretches);
        assert!(
            problems.is_empty(),
            "the hub differs from the original's (golden/signalr.json):\n  {}",
            problems.join("\n  ")
        );
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
        // `wanted/missing` is a name Radarr's hub uses too, for the movie
        // a grab or an import touched: the recording shows it.
        for name in ["series", "episode", "episodefile", "calendar"] {
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
            // The search a history row marked as failed is followed by.
            format!("/movies/newznab/api t=movie {page} tmdbid=206487 imdbid=2397535"),
            format!("/movies/torznab/api t=movie {page} imdbid=2397535"),
            format!("/movies/newznab/api t=search {page} q=Predestination 2014"),
            format!("/movies/torznab/api t=search {page} q=Predestination 2014"),
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

/// Radarr's hub and command list around what the long list does not show,
/// against a recording of its own (`tests/fixtures/radarr/golden-hub`):
/// a scan that imports a file asks for the downloads to be refreshed, as
/// a grab does, and `command` sync follows the clean-up of the old
/// commands.
#[tokio::test]
async fn the_hub_and_the_refresh_after_a_scan_are_the_originals() {
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
            tv: None,
            movies: Some(dirs.movies.clone()),
        },
    )
    .await
    .expect("a mock on a free port");
    let state = build_state(&dirs);
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
    state.start_runtime().await.expect("the runtime");
    let mut hub = state.hub().subscribe();
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;
    let mut stretches: Vec<Vec<Value>> = vec![Vec::new()];
    let mut ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut answers: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for g in &goldens {
        if g.method == "sleep" {
            let seconds: i64 = g.path.parse().expect("sleep seconds");
            state.set_now(Some(state.now() + chrono::Duration::seconds(seconds)));
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().expect("the runtime").wait_idle(),
            )
            .await
            .expect("commands finish within a minute");
            let heard = drain_hub(&mut hub).await;
            stretches.last_mut().unwrap().extend(heard);
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
        let sent = Golden { path, ..g.clone() };
        let (status, body) = send(&app, &sent, &session, &mappings).await;
        assert_eq!(status.as_u16(), g.status, "{}", g.name);
        if let Some(id) = body.get("id").and_then(Value::as_i64) {
            ids.insert(g.name.clone(), id);
        }
        answers.insert(g.name.clone(), body);
    }

    // Each scan imported the file dropped for it, and the import asked for
    // the downloads to be refreshed: in the original's list of commands
    // and in Unarr's, after the scan and queued by nobody (`unspecified`,
    // where the scheduler's own refresh would say `scheduled`).
    for (list_name, scan) in [
        ("command-after-rescan", "RescanMovie"),
        ("command-after-scan", "DownloadedMoviesScan"),
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

    // The hub, as the long list's is compared.
    let problems = signalr_differences(&golden_dir, &goldens, &stretches);
    assert!(
        problems.is_empty(),
        "the hub differs from the original's (golden-hub/signalr.json):\n  {}",
        problems.join("\n  ")
    );
    drop(mock);
    drop(dirs);
}

/// Milestone 4d, the library: movies looked up, refreshed, added, edited,
/// moved and deleted, the settings written, against a recording of its
/// own (`tests/fixtures/radarr/golden-library`), so that the long list was
/// not recorded again. The original reached its metadata server through
/// `scripts/mock-servarr.py`, which answered from
/// `tests/fixtures/metadata`; Unarr asks the Rust mock for the same files.
#[tokio::test]
async fn the_library_goldens_match() {
    replay_list("golden-library", library_mode, &[]).await;
}

/// Milestone 4d, the routes the audit of `docs/audit/` found declared by
/// Radarr or called by its UI and missing, against
/// `tests/fixtures/radarr/golden-audit`.
#[tokio::test]
async fn the_audit_goldens_match() {
    replay_list("golden-audit", audit_mode, &["queue sync", "queue/details sync"]).await;
}

/// The review of milestone 4d: the path rules of a movie and of a root
/// folder against the recycling bin and the system folders, against
/// `tests/fixtures/radarr/golden-review`.
#[tokio::test]
async fn the_review_goldens_match() {
    // `health sync`: the checks a settings write asks for run some
    // seconds later on either side, and the list does not wait for them.
    replay_list(
        "golden-review",
        review_mode,
        &["queue sync", "queue/details sync", "health sync"],
    )
    .await;
}

/// The fixture library served with the mock's metadata, for the cases no
/// golden covers: the dirs and the mock live as long as the app.
async fn live_app(name: &str) -> (ReplayDirs, unarr_mock::Mock, AppState, axum::Router) {
    let dirs = ReplayDirs::create_named(name);
    let mock = unarr_mock::Mock::start_with(
        unarr_mock::fixtures_dir(),
        0,
        unarr_mock::MockDirs {
            downloads: Some(dirs.downloads.clone()),
            tv: None,
            movies: Some(dirs.movies.clone()),
        },
    )
    .await
    .expect("a mock on a free port");
    let state = build_state(&dirs);
    state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock(&mock.url()));
    state.start_runtime().await.expect("the runtime");
    let app = unarr_api::router(state.clone());
    (dirs, mock, state, app)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("X-Api-Key", API_KEY);
    let body = match body {
        Some(json) => {
            req = req.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// `history/since` is read and sent a thousand rows at a time: across
/// pages it is still one JSON array, of every row from the date on, oldest
/// first, as the original answers in one piece.
#[tokio::test]
async fn history_since_is_one_array_across_pages() {
    let (_dirs, _mock, state, app) = live_app("history_since_pages").await;
    let expected: Vec<i64> = state
        .db()
        .call(|conn| {
            let movie: i64 = conn.query_row("SELECT id FROM movies ORDER BY id LIMIT 1", [], |r| r.get(0))?;
            let first: i64 =
                conn.query_row("SELECT coalesce(max(id), 0) + 1 FROM history", [], |r| r.get(0))?;
            let start = unarr_core::time::parse_utc("2030-01-01T00:00:00Z").unwrap();
            // Two and a half pages, some sharing an instant.
            for n in 0..2500 {
                unarr_core::movies::store::insert_history(
                    conn,
                    &unarr_core::movies::model::MovieHistoryEntry {
                        id: first + n,
                        movie_id: movie,
                        source_title: Some(format!("Some.Movie.2030.1080p.WEB-DL.x264-G{n}")),
                        date: start + chrono::Duration::seconds(n / 3),
                        event_type: unarr_core::movies::model::MovieHistoryEvent::Grabbed,
                        quality: unarr_core::model::QualityModel {
                            quality_id: 3,
                            ..Default::default()
                        },
                        languages: vec![1],
                        data: serde_json::json!({}),
                        download_id: None,
                    },
                )?;
            }
            unarr_core::movies::store::history_since_ids(conn, chrono::DateTime::<chrono::Utc>::MIN_UTC, None)
        })
        .await
        .unwrap();
    assert!(expected.len() > 2500, "{} rows", expected.len());
    let (status, body) = call(
        &app,
        "GET",
        "/api/v3/history/since?date=1900-01-01T00:00:00Z",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Vec<i64> = body
        .as_array()
        .expect("one JSON array")
        .iter()
        .map(|h| h["id"].as_i64().unwrap())
        .collect();
    assert_eq!(got, expected);
    // Nothing from the date on: an empty array.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v3/history/since?date=2100-01-01T00:00:00Z",
        None,
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, serde_json::json!([])));
}

/// The Discover add leaves out a movie that does not validate, so the
/// movies it answers with no longer pair with the request by position:
/// each takes its own entry's root folder, found by its TMDB id.
#[tokio::test]
async fn the_discover_add_answers_each_movie_with_its_own_entry() {
    let (dirs, _mock, _state, app) = live_app("discover_pairs").await;
    let movies = dirs.movies.to_string_lossy().into_owned();
    let entry = |tmdb: i64, title: &str, root: &str| {
        serde_json::json!({
            "tmdbId": tmdb, "title": title, "year": 2014, "qualityProfileId": 4,
            "monitored": true, "minimumAvailability": "released", "rootFolderPath": root,
            "addOptions": {"monitor": "movieOnly", "searchForMovie": false}
        })
    };
    // The metadata server knows no movie 999999999.
    let body = serde_json::json!([
        entry(999999999, "Nothing", "/nowhere"),
        entry(157336, "Interstellar", &movies),
    ]);
    let (status, answer) = call(&app, "POST", "/api/v3/importlist/movie", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let added = answer.as_array().expect("an array");
    assert_eq!(added.len(), 1, "{answer}");
    assert_eq!(added[0]["tmdbId"], 157336);
    assert_eq!(added[0]["rootFolderPath"], serde_json::json!(movies));
}

/// The single list edit stores `minimumAvailability` as the movies take
/// it: a number is the enum's value, a name is matched however it is
/// written.
#[tokio::test]
async fn a_list_edit_stores_the_minimum_availability_it_was_sent() {
    let (_dirs, _mock, _state, app) = live_app("list_availability").await;
    let (status, lists) = call(&app, "GET", "/api/v3/importlist", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = lists.as_array().and_then(|l| l.first()).cloned().expect("a list");
    let id = list["id"].as_i64().unwrap();
    for (sent, stored) in [
        (serde_json::json!(1), "announced"),
        (serde_json::json!("InCinemas"), "inCinemas"),
    ] {
        let mut edit = list.clone();
        edit["minimumAvailability"] = sent.clone();
        // The fixture's list id was redacted; the edit needs one that validates.
        for field in edit["fields"].as_array_mut().into_iter().flatten() {
            if field["name"] == "listId" {
                field["value"] = serde_json::json!("ur12345678");
            }
        }
        let (status, answer) = call(
            &app,
            "PUT",
            &format!("/api/v3/importlist/{id}?forceSave=true"),
            Some(edit),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{sent}: {answer}");
        let (_, read) = call(&app, "GET", &format!("/api/v3/importlist/{id}"), None).await;
        assert_eq!(read["minimumAvailability"], stored, "{sent}");
    }
}

fn review_mode(name: &str) -> Mode {
    match name {
        // An exception the original lets escape carries its stack.
        "movie-import-no-root" => Mode::Exact(&["description"]),
        // A model binding error names the request it refused.
        "blocklist-movie-ids-empty" | "blocklist-movie-ids-space" | "blocklist-protocols-space" => {
            Mode::Exact(&["traceId"])
        }
        // The insert the unique index refused, with the SQLite stack.
        "importlist-movie-add-one-unknown" => Mode::Exact(&["description"]),
        // The movie added, with the moment it was.
        "movie-add-path-dotted-system-folder" | "movie-added-from-discover" => Mode::Exact(&["added"]),
        // The disk the recording ran on.
        n if n.starts_with("rootfolder-") => Mode::Exact(&["freeSpace"]),
        _ => Mode::Exact(&[]),
    }
}

/// Milestone 4d, the backups and the log files, against
/// `tests/fixtures/radarr/golden-backup`.
#[tokio::test]
async fn the_backup_goldens_match() {
    replay_list(
        "golden-backup",
        backup_mode,
        &["queue sync", "queue/details sync"],
    )
    .await;
}

/// What `tests/fixtures/radarr/golden-events` recorded on the original:
/// the console's level saved with the host settings and read back, and
/// the deprecated `CheckForFinishedDownload` run as the refresh of the
/// downloads. The warning it logs is the series side's code, whose test
/// reads it back from the log.
#[tokio::test]
async fn the_events_goldens_match() {
    replay_list(
        "golden-events",
        events_mode,
        &["queue sync", "queue/details sync"],
    )
    .await;
}

/// How each answer of the events recording is compared.
fn events_mode(name: &str) -> Mode {
    match name {
        n if n.starts_with("config-host") => Mode::Exact(&["logLevel"]),
        n if n.starts_with("command-") => Mode::Exact(COMMAND_STARTING),
        // The page; its lines are what each program logged since it
        // started, and this replay keeps none (the series side's test
        // reads the warning back).
        "log-warnings" => Mode::Exact(&["records", "totalRecords"]),
        _ => Mode::Exact(&[]),
    }
}

/// Milestone 4d, the import lists: the IMDb watchlist written, tested and
/// synced, against `tests/fixtures/radarr/golden-list`.
#[tokio::test]
async fn the_import_list_goldens_match() {
    // The recording spans seventy seconds, in which the original's
    // scheduler refreshed the download clients once and told the UI to
    // read the queue again. The replay moves its clock in jumps and the
    // scheduler looks every thirty seconds of its own time, which this
    // short list does not reach: the queue it would announce is the same
    // empty one, and the long list next door checks that announcement.
    replay_list("golden-list", list_mode, &["queue sync", "queue/details sync"]).await;
}

/// What a movie added or deleted tells Discord, the webhook and Plex,
/// against `tests/fixtures/radarr/golden-notifications`: a movie added
/// with and without its options (the webhook fails on the second, as the
/// original's does), one deleted with its files (Plex refreshes its
/// folder) and one without.
#[tokio::test]
async fn the_notification_goldens_match() {
    replay_list(
        "golden-notifications",
        notifications_mode,
        &["queue sync", "queue/details sync"],
    )
    .await;
}

/// How each answer of the notifications recording is compared.
fn notifications_mode(name: &str) -> Mode {
    match name {
        // The original's Plex update runs on a background handler, after
        // the deletion, wherever that lands among the other posts.
        "delete-notifications" => Mode::Multiset(&[]),
        n if n.ends_with("-notifications") => Mode::Exact(&[]),
        // The movie added, with the moment it was: the original stamps it
        // before the lookup of its metadata, the recording when it
        // answered.
        "movie-add" | "movie-add-bare" => Mode::Exact(&["added"]),
        other => library_mode(other),
    }
}

/// A recorded answer with the user the recording ran as (`root`) replaced
/// by the one this process runs as, found the way the server finds it, so
/// that both say the same name wherever the tests run.
fn as_current_user(recorded: &Value) -> Value {
    let user = unarr_api::routes::settings_writes::current_user();
    let text = recorded
        .to_string()
        .replace("by user 'root'", &format!("by user '{user}'"))
        .replace("\"user\":\"root\"", &format!("\"user\":\"{user}\""));
    serde_json::from_str(&text).expect("the answer is still JSON")
}

/// A golden the mock answered under a name: what the notification targets
/// received, compared as `mode` says (exactly, or in any order).
fn mock_differences(
    mode: Mode,
    g: &Golden,
    status: StatusCode,
    bytes: &[u8],
    mappings: &[(String, &str)],
) -> Option<String> {
    let body: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let body = unmap_paths(&body, mappings);
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
        return Some(format!(
            "{}: status {} expected, {} got",
            g.name, g.status, status
        ));
    }
    let mut problems = Vec::new();
    let (want, got) = match mode {
        Mode::Exact(ignore) => (normalize(&g.body, ignore), normalize(&body, ignore)),
        Mode::Multiset(ignore) => (
            sorted_list(normalize(&g.body, ignore)),
            sorted_list(normalize(&body, ignore)),
        ),
        _ => panic!("mock goldens compare exactly"),
    };
    diff("", &want, &got, &mut problems);
    (!problems.is_empty()).then(|| {
        format!(
            "{} (mock {} {}):\n    {}",
            g.name,
            g.method,
            g.path,
            problems.join("\n    ")
        )
    })
}

/// The port the recordings name for the mock services: the Python mock's.
const RECORDED_MOCK_PORT: u16 = 18989;

/// Where a connection names the mock, the recording's port in place of
/// `from` and `to` the other: an address (`127.0.0.1:<port>` in a URL)
/// and a provider field called `port` (Plex's). The proxy's port is
/// another setting and is left alone.
fn swap_mock_port(v: &mut Value, from: u16, to: u16) {
    match v {
        Value::String(s) => {
            let address = format!("127.0.0.1:{from}");
            if s.contains(&address) {
                *s = s.replace(&address, &format!("127.0.0.1:{to}"));
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| swap_mock_port(i, from, to)),
        Value::Object(map) => {
            if map.get("name").and_then(Value::as_str) == Some("port")
                && map.get("value").and_then(Value::as_u64) == Some(u64::from(from))
            {
                map.insert("value".into(), Value::from(to));
            }
            map.values_mut().for_each(|x| swap_mock_port(x, from, to));
        }
        _ => {}
    }
}

/// One recorded list replayed: the fixture restored into a directory of
/// its own, the mock on a free port, the clock moved by the recording.
/// The connections a list creates name the recording's port: the replay
/// sends its mock's instead, and compares the answers with it put back.
async fn replay_list(list: &'static str, mode: fn(&str) -> Mode, excused_hub: &[&str]) {
    let golden_dir = fixture_dir().join(list);
    let dirs = ReplayDirs::create_named(list);
    let mappings = dirs.mappings();
    let mock = unarr_mock::Mock::start_with(
        unarr_mock::fixtures_dir(),
        0,
        unarr_mock::MockDirs {
            downloads: Some(dirs.downloads.clone()),
            tv: None,
            movies: Some(dirs.movies.clone()),
        },
    )
    .await
    .expect("a mock on a free port");
    let state = build_state(&dirs);
    state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock(&mock.url()));
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
    // The recording's `/movies` holds the marker of the script that made
    // it, so the root folder is not empty and a movie's scan is not
    // skipped: the same here.
    std::fs::write(dirs.movies.join(".unarr-fixture"), "").expect("the marker");
    state.start_runtime().await.expect("the runtime");
    // The checks of the start, which the original ran five seconds in,
    // before the list reads the health: now, not five seconds from now.
    let runtime = state.commands().expect("the runtime").clone();
    runtime.take_pending_health_checks();
    runtime.check_health(unarr_core::health::CheckSet::All).await;
    let mut hub = state.hub().subscribe();
    let app = unarr_api::router(state.clone());
    let session = login_cookie(&app).await;
    let only: Vec<String> = std::env::var("UNARR_CONTRACT_ONLY")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let dump: Vec<String> = std::env::var("UNARR_CONTRACT_DUMP")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let mut stretches: Vec<Vec<Value>> = vec![Vec::new()];
    let mut ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut failures = Vec::new();
    let mut checked = 0;
    for g in &goldens {
        while let Ok(message) = hub.try_recv() {
            stretches.last_mut().unwrap().push(message);
        }
        if g.method == "sleep" {
            let seconds: i64 = g.path.parse().expect("sleep seconds");
            state.set_now(Some(state.now() + chrono::Duration::seconds(seconds)));
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state.commands().expect("the runtime").wait_idle(),
            )
            .await
            .expect("commands finish within a minute");
            let heard = drain_hub(&mut hub).await;
            stretches.last_mut().unwrap().extend(heard);
            stretches.push(Vec::new());
            continue;
        }
        state.set_now(g.recorded_at);
        let mut sent = g.clone();
        sent.path = substitute_ids(&g.path, &ids);
        if let Some(body) = &g.request_body {
            let mut body = body.clone();
            for (name, id) in &ids {
                body = body.replace(&format!("\"{{id:{name}}}\""), &id.to_string());
            }
            // Written again only where the port was: a model binding
            // error names the byte it stopped at.
            if let Ok(mut json) = serde_json::from_str::<Value>(&body) {
                let recorded = json.clone();
                swap_mock_port(&mut json, RECORDED_MOCK_PORT, mock.port());
                if json != recorded {
                    body = json.to_string();
                }
            }
            sent.request_body = Some(body);
        }
        if g.to_mock {
            let mut path = sent.path.clone();
            for (replay, recorded) in &mappings {
                path = path.replace(&format!("path={recorded}"), &format!("path={replay}"));
            }
            let body = sent.request_body.as_deref().unwrap_or("").as_bytes();
            let (status, bytes) = mock.call(&g.method, &path, body);
            if g.name == "-" {
                assert!(status.is_success(), "mock {} {path}: {status}", g.method);
                continue;
            }
            // Under a name, what the mock holds is a golden too.
            if !only.is_empty() && !only.iter().any(|p| g.name.starts_with(p.as_str())) {
                continue;
            }
            checked += 1;
            failures.extend(mock_differences(mode(&g.name), g, status, &bytes, &mappings));
            continue;
        }
        let (status, body) = send(&app, &sent, &session, &mappings).await;
        let mut body = unmap_paths(&body, &mappings);
        swap_mock_port(&mut body, mock.port(), RECORDED_MOCK_PORT);
        if let Some(id) = body
            .get("id")
            .or_else(|| body.get(0).and_then(|first| first.get("id")))
            .and_then(Value::as_i64)
        {
            ids.insert(g.name.clone(), id);
        }
        if dump.contains(&g.name) {
            eprintln!(
                "--- {} answered {status}:\n{}",
                g.name,
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );
        }
        if pending(&g.name) && std::env::var_os("UNARR_CONTRACT_ALL").is_none() {
            continue;
        }
        if !only.is_empty() && !only.iter().any(|p| g.name.starts_with(p.as_str())) {
            continue;
        }
        checked += 1;
        if status.as_u16() != g.status {
            failures.push(format!(
                "{}: status {} expected, {} got: {}",
                g.name,
                g.status,
                status,
                short(&body)
            ));
            continue;
        }
        // The message of a folder that cannot be written names the user
        // the process runs as: root where the recording was taken,
        // another on a CI runner. The rest of the answer is compared.
        let expected = if g.name == "config-mediamanagement-put-bin-missing" {
            as_current_user(&g.body)
        } else {
            g.body.clone()
        };
        let problems = compare(mode(&g.name), &expected, &body);
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
    if only.is_empty() {
        let heard = drain_hub(&mut hub).await;
        stretches.last_mut().unwrap().extend(heard);
        // A command's message names the folders it moved.
        let stretches: Vec<Vec<Value>> = stretches
            .iter()
            .map(|stretch| stretch.iter().map(|m| unmap_paths(m, &mappings)).collect())
            .collect();
        let problems: Vec<String> = signalr_differences(&golden_dir, &goldens, &stretches)
            .into_iter()
            .filter(|p| !excused_hub.iter().any(|e| p.ends_with(e)))
            .collect();
        if !problems.is_empty() {
            failures.push(format!(
                "the hub differs from the original's ({list}/signalr.json):\n  {}",
                problems.join("\n  ")
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

/// How each answer of the audit recording is compared.
fn audit_mode(name: &str) -> Mode {
    match name {
        // A command the original takes and no service of its runs: the
        // exception carries the type it looked for and its stack.
        // Two commands the original fails: one no service of its runs,
        // one for want of an update log folder, which Unarr, updated by
        // replacing its image, never writes. Each exception names the
        // type it looked for and carries its stack.
        "command-rename-movie-folder"
        | "command-rename-movie-folder-done"
        | "command-delete-update-log-files" => Mode::Exact(COMMAND_FAILING),
        n if n.starts_with("command-after-") => Mode::QueuedCommands(COMMAND_CLOCKS),
        n if n.starts_with("command-") => Mode::Exact(COMMAND_STARTING),
        n if n.starts_with("health") => Mode::Multiset(&[]),
        // The movie the Discover page added, with the moment it was.
        "importlist-movie-add" | "movie-added-from-discover" => Mode::Exact(&["added"]),
        "movie-folder-missing" => Mode::Exact(&["description"]),
        // The exception the original's schema builder throws, with its stack.
        "metadata-test-unknown" => Mode::Exact(&["description"]),
        // The blocklist's own rows.
        n if n.starts_with("blocklist") => Mode::Exact(&["ageHours", "ageMinutes", "date", "id"]),
        _ => Mode::Exact(&[]),
    }
}

/// How each answer of the backup recording is compared.
fn backup_mode(name: &str) -> Mode {
    match name {
        // The zip the recording made: its name, size and moment are its
        // own, and so is the id they hash to.
        n if n.starts_with("system-backup") => Mode::Exact(&["id", "name", "path", "size", "time"]),
        n if n.starts_with("command-") => Mode::Exact(COMMAND_STARTING),
        // The files the original wrote, with the moments it wrote them.
        n if n.starts_with("log-file") => Mode::Exact(&[
            "id",
            "lastWriteTime",
            "filename",
            "contentsUrl",
            "downloadUrl",
            "traceId",
        ]),
        _ => Mode::Exact(&[]),
    }
}

/// How each answer of the import list's recording is compared.
fn list_mode(name: &str) -> Mode {
    match name {
        // The commands the writes and the syncs queued, with the
        // original's own refresh of the downloads coming and going.
        // A command that failed carries the exception's stack.
        n if n.starts_with("command-after-") => Mode::QueuedCommands(&[
            "id",
            "queued",
            "started",
            "ended",
            "duration",
            "stateChangeTime",
            "lastExecutionTime",
            "exception",
        ]),
        n if n.starts_with("command-") => Mode::Exact(COMMAND_STARTING),
        n if n.starts_with("health") => Mode::Multiset(&[]),
        // A movie added by a list carries the moment it was added and,
        // for the one the sync searched for, when that happened.
        // The two movies a sync adds are refreshed side by side, and
        // their alternative titles get their ids in the order that
        // happens to be.
        "movie-prestige" | "movie-gravity" | "movie-after-sync" => {
            Mode::Exact(&["added", "lastSearchTime", "id"])
        }
        "history-after-sync" => Mode::Exact(&["date", "id", "movieId", "ageHours", "ageMinutes"]),
        "movie-delete-excluded" => Mode::Exact(&["traceId"]),
        _ => Mode::Exact(&[]),
    }
}

/// How each answer of the library list is compared.
fn library_mode(name: &str) -> Mode {
    match name {
        // An exception the original lets escape carries its stack.
        "movie-lookup-empty"
        | "movie-lookup-no-term"
        | "movie-lookup-route-tmdb-missing"
        | "movie-lookup-route-imdb-missing"
        | "movie-add-no-tmdb"
        | "config-mediamanagement-put-bin-relative"
        | "tag-create-taken"
        | "tag-update-missing"
        | "tag-delete-missing"
        | "tag-delete-used"
        | "remotepathmapping-create-duplicate"
        | "customformat-create-bad-regex"
        | "customformat-delete-missing"
        | "qualityprofile-create-taken"
        | "qualityprofile-delete-used"
        | "exclusion-update-missing"
        | "collection-put-missing" => Mode::Exact(&["description"]),
        // A model binding error names the request it refused.
        "config-metadata-put-country" => Mode::Exact(&["traceId"]),
        // The disk the recording ran on.
        n if n.starts_with("rootfolder") => Mode::Exact(&["freeSpace"]),
        // `QualityDefinitionController.UpdateMany` answers from the
        // service's five-second cache, which the read just before it
        // filled: the definitions as they were. What it wrote is read
        // back by the next golden.
        "qualitydefinition-update-many" => Mode::Exact(&["title", "minSize", "maxSize", "preferredSize"]),
        // The commands the list or a write queued; the original's own
        // refresh of the downloads, each minute, comes and goes with the
        // clock.
        "command-after-add"
        | "command-after-add-collection"
        | "command-after-move"
        | "command-after-editor-move"
        | "command-after-import"
        | "command-after-collection-put" => Mode::QueuedCommands(COMMAND_CLOCKS),
        n if n.starts_with("command-") && n.ends_with("-get") => Mode::Exact(COMMAND_CLOCKS),
        n if n.starts_with("command-") => Mode::Exact(COMMAND_STARTING),
        // The verdicts are a hash table's.
        n if n.starts_with("health") => Mode::Multiset(&[]),
        // A search the post-add actions queued ran, in the recording,
        // before the pause the replay moves its clock by. The movie's
        // metadata row is written by its own refresh and by the refresh
        // of the collection it brought, two commands the original runs
        // side by side: which of them has the last word differs from one
        // recording to the next (its popularity and votes, a minute
        // apart). Unarr lets the movie's own be the last.
        "movie-added-collection" => Mode::Exact(&["lastSearchTime", "added", "popularity", "ratings"]),
        "collection-after-add" => Mode::Exact(&["ratings"]),
        "movie-brought-by-collection" => Mode::Exact(&["lastSearchTime", "added"]),
        // The file a rescan found was added while the recording paused.
        "movie-after-rescan"
        | "movie-put-move"
        | "movie-after-move"
        | "movie-editor"
        | "movie-editor-replace-tags"
        | "movie-editor-remove-tags"
        | "movie-editor-move"
        | "movie-after-editor-move" => Mode::Exact(&["dateAdded"]),
        // The folder browser shows when the disk last changed; the parent
        // of the replay's stand-in for `/movies` is not the root.
        n if n.starts_with("filesystem-") => Mode::Exact(&["lastModified", "parent"]),
        "movie-deleted" => Mode::Exact(&["traceId"]),
        "movie-delete-missing" | "movie-import-existing" => Mode::Exact(&["description"]),
        // The two movies of an import are refreshed side by side, and
        // their titles get their ids in the order that happens to be.
        "movie-imported" | "movie-imported-second" => Mode::Exact(&["id"]),
        _ => Mode::Exact(&[]),
    }
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

/// Everything the hub has sent, once it has been quiet for a moment.
async fn drain_hub(hub: &mut tokio::sync::broadcast::Receiver<Value>) -> Vec<Value> {
    use tokio::sync::broadcast::error::TryRecvError;
    let mut out = Vec::new();
    let mut quiet = 0;
    while quiet < 4 {
        match hub.try_recv() {
            Ok(message) => {
                out.push(message);
                quiet = 0;
            }
            Err(TryRecvError::Lagged(missed)) => panic!("the test fell {missed} hub messages behind"),
            Err(_) => {
                quiet += 1;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    out
}

/// What identifies a hub message that carries a resource: the name it was
/// sent under, the action, the resource's id and, for a movie, the two
/// fields a grab and an import change. A command the list posted counts
/// once it has ended, by its name and last message: the states before are debounced by the
/// original and arrive or not as the clock has it. `None` for a message
/// without a resource (`sync`), and for the queue's counts, which are
/// debounced too.
fn hub_key(message: &Value) -> Option<String> {
    let name = message["name"].as_str()?;
    let body = &message["body"];
    let action = body["action"].as_str().unwrap_or("");
    let resource = body["resource"].as_object()?;
    let field = |k: &str| resource.get(k).map(|v| v.to_string()).unwrap_or_default();
    match name {
        "queue/status" | "version" => None,
        // A scheduled run is the clock's doing, not the list's.
        "command" => (resource.get("status")? == "completed" && resource.get("trigger")? != "scheduled")
            .then(|| format!("command {} ended: {}", field("name"), field("message"))),
        "movie" | "wanted/missing" | "wanted/cutoff" => Some(format!(
            "{name} {action} id={} grabbed={} hasFile={}",
            field("id"),
            field("grabbed"),
            field("hasFile")
        )),
        _ => Some(format!("{name} {action} id={}", field("id"))),
    }
}

/// A message without a resource, by name and action.
fn hub_sync_key(message: &Value) -> Option<String> {
    let body = &message["body"];
    if body["resource"].is_object() || message["name"] == "version" {
        return None;
    }
    Some(format!(
        "{} {}",
        message["name"].as_str()?,
        body["action"].as_str().unwrap_or("")
    ))
}

/// The messages about movies and their files, in the order sent, each as
/// a word: `movie+root` is the movie as `MovieController` maps it, with
/// its root folder; `movie` the one the shared handler sends without.
fn movie_words(messages: &[&Value]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(|m| {
            let name = m["name"].as_str()?;
            let body = &m["body"];
            let resource = body["resource"].as_object()?;
            let id = resource.get("id").map(|v| v.to_string()).unwrap_or_default();
            let word = match name {
                "movie" if resource.contains_key("rootFolderPath") => "movie+root".to_string(),
                "movie" | "wanted/cutoff" | "wanted/missing" => name.to_string(),
                "moviefile" => format!("moviefile {}", body["action"].as_str().unwrap_or("")),
                _ => return None,
            };
            Some((word, id))
        })
        .collect()
}

/// What surrounds each movie file message, against the order the original
/// announces an import (the file, the two wanted lists, the movie) and a
/// deletion (the movie with its root folder, the two wanted lists, the
/// file, the movie again) in.
fn movie_file_order_problems(messages: &[&Value]) -> Vec<String> {
    let words = movie_words(messages);
    let word = |i: usize| words.get(i).map(|(w, _)| w.as_str()).unwrap_or("(nothing)");
    let mut problems = Vec::new();
    for (i, (name, id)) in words.iter().enumerate() {
        match name.as_str() {
            "moviefile updated" => {
                let after = [word(i + 1), word(i + 2), word(i + 3)];
                if after != ["wanted/cutoff", "wanted/missing", "movie"] {
                    problems.push(format!("movie file {id} imported, then {after:?}"));
                }
            }
            "moviefile deleted" => {
                let before = [
                    i.checked_sub(3).map(word).unwrap_or("(nothing)"),
                    i.checked_sub(2).map(word).unwrap_or("(nothing)"),
                    i.checked_sub(1).map(word).unwrap_or("(nothing)"),
                ];
                if before != ["movie+root", "wanted/cutoff", "wanted/missing"] || word(i + 1) != "movie" {
                    problems.push(format!(
                        "movie file {id} deleted after {before:?} and before {:?}",
                        word(i + 1)
                    ));
                }
            }
            _ => {}
        }
    }
    problems
}

/// Hub messages Unarr is known to send differently, each with its reason;
/// a line is a key of `hub_key` or `hub_sync_key`.
const PENDING_HUB: &[&str] = &[];

/// The hub against the original's (`signalr.json`, recorded with the
/// goldens by `record-golden.py --signalr`). Messages that carry a resource
/// are compared stretch by stretch, as sets: which resources the UI was
/// told about between two pauses of the list. Messages that only say
/// "read this again" are compared over the whole list, by name: when and
/// how often they are sent follows the original's debounce timers. The
/// order of the messages around each movie file is checked apart, on the
/// recording as on the replay.
fn signalr_differences(
    golden_dir: &std::path::Path,
    goldens: &[Golden],
    replayed: &[Vec<Value>],
) -> Vec<String> {
    use std::collections::{BTreeMap, BTreeSet};
    let Ok(text) = std::fs::read_to_string(golden_dir.join("signalr.json")) else {
        return vec!["signalr.json is missing: record it with `make golden-radarr`".into()];
    };
    let recorded: Vec<Value> = serde_json::from_str(&text).expect("signalr.json");
    // The stretch each step of the list is in. The recorder names a step by
    // the request's name, or by its position when it has none.
    let mut stretch_of: BTreeMap<String, usize> = BTreeMap::new();
    stretch_of.insert("(start)".into(), 0);
    let mut stretch = 0;
    for (index, g) in goldens.iter().enumerate() {
        if g.method == "sleep" {
            stretch += 1;
            continue;
        }
        let step = if g.name == "-" {
            format!("#{}", index + 1)
        } else {
            g.name.clone()
        };
        stretch_of.insert(step, stretch);
    }
    let mut problems = Vec::new();
    let mut original: Vec<BTreeSet<String>> = vec![BTreeSet::new(); stretch + 1];
    let mut original_sync: BTreeSet<String> = BTreeSet::new();
    let mut first_step: BTreeMap<usize, String> = BTreeMap::new();
    for step in &recorded {
        let after = step["after"].as_str().unwrap_or("");
        let Some(&at) = stretch_of.get(after) else {
            problems.push(format!(
                "signalr.json names a step the list does not have: {after}"
            ));
            continue;
        };
        first_step.entry(at).or_insert_with(|| after.to_string());
        for message in step["messages"].as_array().into_iter().flatten() {
            if let Some(key) = hub_key(message) {
                original[at].insert(key);
            } else if let Some(key) = hub_sync_key(message) {
                original_sync.insert(key);
            }
        }
    }
    let mut ours_sync: BTreeSet<String> = BTreeSet::new();
    let pending = |key: &String| PENDING_HUB.contains(&key.as_str());
    for (at, wanted) in original.iter().enumerate() {
        let empty = Vec::new();
        let messages = replayed.get(at).unwrap_or(&empty);
        let ours: BTreeSet<String> = messages.iter().filter_map(hub_key).collect();
        ours_sync.extend(messages.iter().filter_map(hub_sync_key));
        let place = first_step
            .get(&at)
            .map(|s| format!("stretch {at} (from {s})"))
            .unwrap_or_else(|| format!("stretch {at}"));
        for key in wanted.difference(&ours).filter(|k| !pending(k)) {
            problems.push(format!("{place}: the original sent, Unarr did not: {key}"));
        }
        for key in ours.difference(wanted).filter(|k| !pending(k)) {
            problems.push(format!("{place}: Unarr sent, the original did not: {key}"));
        }
    }
    // The order around a movie file, which the sets above cannot see.
    let recorded_messages: Vec<&Value> = recorded
        .iter()
        .flat_map(|step| step["messages"].as_array().into_iter().flatten())
        .collect();
    let replayed_messages: Vec<&Value> = replayed.iter().flatten().collect();
    for (who, messages) in [("the original", recorded_messages), ("Unarr", replayed_messages)] {
        problems.extend(
            movie_file_order_problems(&messages)
                .into_iter()
                .map(|p| format!("{who}: {p}")),
        );
    }
    for key in original_sync.difference(&ours_sync).filter(|k| !pending(k)) {
        problems.push(format!("the original sent, Unarr never did: {key}"));
    }
    for key in ours_sync.difference(&original_sync).filter(|k| !pending(k)) {
        problems.push(format!("Unarr sent, the original never did: {key}"));
    }
    problems
}

#[test]
fn the_order_check_sees_a_file_announced_out_of_turn() {
    let message = |name: &str, action: &str, resource: Value| serde_json::json!({"name": name, "body": {"action": action, "resource": resource}});
    let movie = serde_json::json!({"id": 7});
    let rooted = serde_json::json!({"id": 7, "rootFolderPath": "/movies"});
    let file = serde_json::json!({"id": 40});
    let right = [
        message("movie", "updated", rooted.clone()),
        message("wanted/cutoff", "updated", movie.clone()),
        message("wanted/missing", "updated", movie.clone()),
        message("moviefile", "deleted", file.clone()),
        message("movie", "updated", movie.clone()),
        message("queue", "sync", Value::Null),
        message("moviefile", "updated", file.clone()),
        message("wanted/cutoff", "updated", movie.clone()),
        message("wanted/missing", "updated", movie.clone()),
        message("movie", "updated", movie.clone()),
    ];
    assert!(movie_file_order_problems(&right.iter().collect::<Vec<_>>()).is_empty());
    // The file first, the movie after: the sets are the same.
    let wrong = [
        message("moviefile", "deleted", file.clone()),
        message("movie", "updated", rooted),
        message("wanted/cutoff", "updated", movie.clone()),
        message("wanted/missing", "updated", movie.clone()),
        message("movie", "updated", movie),
    ];
    assert_eq!(
        movie_file_order_problems(&wrong.iter().collect::<Vec<_>>()).len(),
        1
    );
}

/// `CleanupAdditionalUsers` and `FixFutureRunScheduledTasks`, against
/// what the original's database held after the same housekeeping
/// (`tests/fixtures/radarr/golden-housekeeping`).
#[tokio::test]
async fn housekeeping_repairs_users_and_task_runs_as_the_original() {
    let dirs = ReplayDirs::create_named("housekeeping");
    let state = build_state(&dirs);
    housekeeping_repairs_as_the_original(&state, &fixture_dir().join("golden-housekeeping"), API_KEY).await;
}

/// What Radarr puts in a database it creates at its first start, against a
/// recording of the original started without one
/// (`tests/fixtures/radarr/golden-blank`): Unarr on a blank database, with
/// what its start ensures (`unarrd`'s `defaults::ensure_movies` and
/// `metadata_writes::insert_missing_consumers`). The hub
/// is not compared: it told the UI the version and a health sync only.
#[tokio::test]
async fn a_blank_start_has_the_originals_defaults() {
    let dir = fixture_dir();
    let db = Db::open_in_memory_for(Personality::Radarr).expect("in-memory db");
    db.call_blocking(|conn| {
        unarr_core::defaults::ensure_movies(conn)?;
        unarr_api::routes::metadata_writes::insert_missing_consumers(conn, Personality::Radarr)?;
        Ok(())
    })
    .expect("the defaults");
    let host = unarr_import::config_xml::read(&dir.join("config.xml"), Personality::Radarr.default_port())
        .expect("config.xml");
    let settings = Settings::from_host_config(host, Personality::Radarr);
    let app_data = std::env::temp_dir().join(format!("unarr_contract_{}_blank", std::process::id()));
    std::fs::create_dir_all(&app_data).expect("app data");
    let state = AppState::new(
        Personality::Radarr,
        db,
        settings,
        RuntimeInfo::detect(app_data.clone()),
        LogBuffer::new(),
    );
    let app = unarr_api::router(state);
    let mut failures = Vec::new();
    let goldens = load_goldens(&dir.join("golden-blank"));
    for g in &goldens {
        let (status, body) = send(&app, g, "", &[]).await;
        if status.as_u16() != g.status {
            failures.push(format!("{}: status {} expected, {status} got", g.name, g.status));
            continue;
        }
        let problems = compare(Mode::Exact(&[]), &g.body, &body);
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", g.name, problems.join("\n    ")));
        }
    }
    let _ = std::fs::remove_dir_all(&app_data);
    assert_eq!(goldens.len(), 5, "every answer of the recording");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The Plex connection's sign-in and server list (issue #16), against the
/// mock's plex.tv: Radarr's names and version.
#[tokio::test]
async fn the_plex_actions_answer_as_radarrs() {
    let (_dirs, _mock, _state, app) = live_app("plex_actions").await;
    plex_actions_answer_as_the_originals(&app, "Radarr", "6.4.4.10685").await;
}
