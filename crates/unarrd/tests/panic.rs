//! What a panic costs, on the binary as it is built: one request or one
//! command, never the process, and never the other personality.
//!
//! The daemon is started with Sonarr and Radarr on the fixtures and told to
//! panic through the panic switch, which exists only in a build with the
//! `panic-switch` feature (`make panic-test` builds one, in release mode, in
//! a target directory of its own). Radarr's `/ping` is asked throughout by a
//! thread of its own and must answer every time.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

const KEY: &str = "0123456789abcdef0123456789abcdef";

/// An answer: status, headers in lower case, body.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// One request on a connection of its own, with the API key.
fn request(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> std::io::Result<Answer> {
    request_as(addr, method, path, body, "identity")
}

/// The same, from a client that takes `encoding` (`gzip`, as a browser).
fn request_as(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
    encoding: &str,
) -> std::io::Result<Answer> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let body = body.unwrap_or("");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nX-Api-Key: {KEY}\r\nAccept-Encoding: {encoding}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("the connection closed without an answer: {text:?}"),
        )
    })?;
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let chunked = headers
        .iter()
        .any(|(n, v)| n == "transfer-encoding" && v.contains("chunked"));
    let body = if chunked { unchunk(body) } else { body.to_owned() };
    Ok(Answer {
        status,
        headers,
        body,
    })
}

fn unchunk(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 || after.len() < size {
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].trim_start_matches("\r\n");
    }
    out
}

fn get(addr: SocketAddr, path: &str) -> Answer {
    request(addr, "GET", path, None).unwrap_or_else(|e| panic!("GET {path}: {e}"))
}

/// `GET` as a browser asks: an answer that could be compressed would be.
fn get_gzip(addr: SocketAddr, path: &str) -> Answer {
    request_as(addr, "GET", path, None, "gzip").unwrap_or_else(|e| panic!("GET {path}: {e}"))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// A running daemon on a data directory of its own; stopped and cleaned
/// up when dropped.
struct Daemon {
    child: Child,
    data: PathBuf,
    sonarr: SocketAddr,
    radarr: SocketAddr,
}

impl Daemon {
    /// A data directory with the fixtures of `apps`, as `make run-both`
    /// lays them out.
    fn data(test: &str, apps: &[&str]) -> PathBuf {
        let data = std::env::temp_dir().join(format!("unarr-panic-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        for app in apps {
            let dir = data.join(app);
            std::fs::create_dir_all(&dir).unwrap();
            for file in ["config.xml".to_owned(), format!("{app}.db")] {
                std::fs::copy(fixtures().join(app).join(&file), dir.join(&file)).unwrap();
            }
        }
        data
    }

    fn start(data: PathBuf, personalities: &str, env: &[(&str, &str)]) -> Daemon {
        let (sonarr, radarr) = (free_port(), free_port());
        let mut command = Command::new(env!("CARGO_BIN_EXE_unarrd"));
        command
            .args(["--data", data.to_str().unwrap()])
            .args(["--personalities", personalities, "--bind", "127.0.0.1"])
            .env("SONARR__SERVER__PORT", sonarr.to_string())
            .env("RADARR__SERVER__PORT", radarr.to_string())
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Nothing here needs the network, and nothing should wait for it.
        for proxy in [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            command.env_remove(proxy);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        Daemon {
            child: command.spawn().expect("starting unarrd"),
            data,
            sonarr: SocketAddr::from(([127, 0, 0, 1], sonarr)),
            radarr: SocketAddr::from(([127, 0, 0, 1], radarr)),
        }
    }

    /// Wait until `addr` answers `/ping`.
    fn wait_for(&mut self, addr: SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if request(addr, "GET", "/ping", None).is_ok_and(|a| a.status == 200) {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("unarrd exited while starting: {status}");
            }
            assert!(Instant::now() < deadline, "{addr} never answered /ping");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    /// The last lines of a personality's log file.
    fn log(&self, app: &str) -> String {
        std::fs::read_to_string(self.data.join(app).join("logs").join(format!("{app}.txt")))
            .unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

/// Asks `/ping` again and again until told to stop: how many times, and
/// how many of them did not answer 200 `{"status":"OK"}`.
struct Pinger {
    stop: Arc<AtomicBool>,
    asked: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Pinger {
    fn start(addr: SocketAddr) -> Pinger {
        let stop = Arc::new(AtomicBool::new(false));
        let asked = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicUsize::new(0));
        let (s, a, f) = (stop.clone(), asked.clone(), failed.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                let ok = request(addr, "GET", "/ping", None)
                    .is_ok_and(|answer| answer.status == 200 && answer.json()["status"] == "OK");
                a.fetch_add(1, Ordering::SeqCst);
                if !ok {
                    f.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Pinger {
            stop,
            asked,
            failed,
            thread: Some(thread),
        }
    }

    /// Stop, and say how many pings were asked and how many failed.
    fn finish(mut self) -> (usize, usize) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
        (
            self.asked.load(Ordering::SeqCst),
            self.failed.load(Ordering::SeqCst),
        )
    }
}

/// The 500 both originals give for an exception nothing caught, as
/// recorded from Sonarr 4.0.20 and Radarr 6.4.4.
fn assert_the_originals_500(answer: &Answer, message: &str) {
    assert_eq!(answer.status, 500, "{}", answer.body);
    assert_eq!(answer.header("content-type"), Some("application/json"));
    assert_eq!(answer.header("cache-control"), Some("no-cache,no-store"));
    assert_eq!(answer.header("pragma"), Some("no-cache"));
    assert_eq!(answer.header("expires"), Some("-1"));
    assert_eq!(answer.header("x-application-version"), None);
    assert_eq!(answer.header("content-encoding"), None);
    let body = answer.json();
    let said = body["message"].as_str().unwrap();
    assert!(said.ends_with(message), "{said}");
    let description = body["description"].as_str().unwrap();
    assert!(
        description.starts_with("panicked at ") && description.ends_with(message),
        "{description}"
    );
}

/// Queue a command and wait for its end.
fn run_command(addr: SocketAddr, name: &str) -> Value {
    let body = format!(r#"{{"name":"{name}"}}"#);
    let queued = request(addr, "POST", "/api/v3/command", Some(&body)).unwrap();
    assert_eq!(queued.status, 201, "{}", queued.body);
    let id = queued.json()["id"].as_i64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let command = get(addr, &format!("/api/v3/command/{id}")).json();
        if command["ended"].is_string() {
            return command;
        }
        assert!(Instant::now() < deadline, "{name} never ended: {command}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_panic_costs_one_request_or_one_command() {
    let mut daemon = Daemon::start(
        Daemon::data("requests", &["sonarr", "radarr"]),
        "sonarr,radarr",
        &[],
    );
    let (sonarr, radarr) = (daemon.sonarr, daemon.radarr);
    daemon.wait_for(sonarr);
    daemon.wait_for(radarr);
    let pings = Pinger::start(radarr);
    let status = |addr| get(addr, "/api/v3/system/status");
    assert_eq!(status(sonarr).status, 200);

    // A request, wherever its work runs: the answer is the originals' 500
    // and the next request is answered.
    for (place, message) in [
        ("handler", "the panic switch was set for a handler"),
        ("database", "the panic switch was set for the database thread"),
        ("blocking", "the panic switch was set for the blocking pool"),
        ("lock", "the panic switch was set for a holder of the settings"),
    ] {
        let answer = get_gzip(sonarr, &format!("/api/v3/system/panic?in={place}"));
        assert_the_originals_500(&answer, message);
        // The key is read from the settings and the status from the
        // database: the lock a panic poisoned and the thread it happened
        // on both still serve.
        let next = status(sonarr);
        assert_eq!(next.status, 200, "after a panic in {place}: {}", next.body);
        assert_eq!(next.json()["appName"], "Sonarr");
        let tags = get(sonarr, "/api/v3/tag");
        assert_eq!(tags.status, 200, "after a panic in {place}: {}", tags.body);
    }
    // Outside the API's pipeline, where the pages are.
    let answer = get_gzip(sonarr, "/panic");
    assert_the_originals_500(&answer, "the panic switch was set for a page");
    assert_eq!(get(sonarr, "/ping").status, 200);
    // And it is in the log of the personality it happened in, with where.
    let log = daemon.log("sonarr");
    assert!(
        log.contains("Request Failed. GET /api/v3/system/panic: panicked at ")
            && log.contains("the panic switch was set for a handler"),
        "{log}"
    );
    assert!(!daemon.log("radarr").contains("panicked"));

    // A command: failed with the panic for its exception, and the same
    // command runs to its end the next time. More times than there are
    // workers, so that none of them is lost on the way.
    for place in ["command", "record", "command", "record", "command"] {
        let armed = get(
            sonarr,
            &format!("/api/v3/system/panic?in={place}&name=MessagingCleanup"),
        );
        assert_eq!(armed.status, 200, "{}", armed.body);
        let failed = run_command(sonarr, "MessagingCleanup");
        assert_eq!(failed["status"], "failed", "{failed}");
        assert_eq!(failed["message"], "Failed", "{failed}");
        let exception = failed["exception"].as_str().unwrap();
        assert!(
            exception.starts_with("panicked at ")
                && exception.ends_with(&format!(
                    "the panic switch was set for {place}:sonarr:MessagingCleanup"
                )),
            "{exception}"
        );
    }
    let done = run_command(sonarr, "MessagingCleanup");
    assert_eq!(done["status"], "completed", "{done}");
    // Another command, and a request, after all of it.
    let done = run_command(sonarr, "RefreshMonitoredDownloads");
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(status(sonarr).status, 200);

    // The other way round, once: Radarr's panic is Radarr's.
    let answer = get_gzip(radarr, "/api/v3/system/panic?in=handler");
    assert_the_originals_500(&answer, "the panic switch was set for a handler");
    assert_eq!(status(radarr).json()["appName"], "Radarr");
    assert_eq!(status(sonarr).status, 200);

    // Radarr answered `/ping` every time it was asked meanwhile, and the
    // process is the one that was started.
    let (asked, failed) = pings.finish();
    assert!(asked > 20, "only {asked} pings");
    assert_eq!(failed, 0, "{failed} of {asked} pings of Radarr failed");
    assert!(daemon.alive());
}

/// `unarrd healthcheck` on `data`: whether healthy, and what it said.
fn healthcheck(data: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_unarrd"))
        .args(["healthcheck", "--data", data.to_str().unwrap()])
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_personality_that_cannot_start_is_left_down_and_the_others_start() {
    // Radarr's database is not one: an error at its start.
    let data = Daemon::data("broken-database", &["sonarr"]);
    std::fs::create_dir_all(data.join("radarr")).unwrap();
    std::fs::write(data.join("radarr/unarr-radarr.db"), "not a database").unwrap();
    let mut daemon = Daemon::start(data, "sonarr,radarr", &[]);
    let (sonarr, radarr) = (daemon.sonarr, daemon.radarr);
    daemon.wait_for(sonarr);
    assert_eq!(get(sonarr, "/api/v3/system/status").status, 200);
    assert!(request(radarr, "GET", "/ping", None).is_err());
    assert!(daemon.alive());
    // Reported: in the log each personality keeps, and by the health check.
    let log = daemon.log("sonarr");
    assert!(
        log.contains("Radarr did not start and is left down until the next start"),
        "{log}"
    );
    let (healthy, said) = healthcheck(&daemon.data);
    assert!(!healthy, "{said}");
    assert!(
        said.contains("sonarr: ok") && said.contains("radarr: did not start"),
        "{said}"
    );
    drop(daemon);

    // The same for a panic at the start, of the first of the two this
    // time: the second still starts.
    let mut daemon = Daemon::start(
        Daemon::data("panic-at-start", &["sonarr", "radarr"]),
        "sonarr,radarr",
        &[("UNARR_PANIC_AT_START", "sonarr")],
    );
    let (sonarr, radarr) = (daemon.sonarr, daemon.radarr);
    daemon.wait_for(radarr);
    assert_eq!(get(radarr, "/api/v3/system/status").status, 200);
    assert!(request(sonarr, "GET", "/ping", None).is_err());
    let log = daemon.log("radarr");
    assert!(
        log.contains("Sonarr did not start and is left down until the next start: panicked at "),
        "{log}"
    );
    let (healthy, said) = healthcheck(&daemon.data);
    assert!(!healthy, "{said}");
    assert!(
        said.contains("radarr: ok") && said.contains("sonarr: did not start"),
        "{said}"
    );
    drop(daemon);

    // Started again without the fault, both serve and the check is clean.
    let mut daemon = Daemon::start(
        Daemon::data("both-start", &["sonarr", "radarr"]),
        "sonarr,radarr",
        &[],
    );
    let (sonarr, radarr) = (daemon.sonarr, daemon.radarr);
    daemon.wait_for(sonarr);
    daemon.wait_for(radarr);
    let (healthy, said) = healthcheck(&daemon.data);
    assert!(healthy, "{said}");
    drop(daemon);

    // With no personality left to serve, the daemon ends and says so.
    let mut daemon = Daemon::start(
        Daemon::data("none-starts", &["sonarr"]),
        "sonarr",
        &[("UNARR_PANIC_AT_START", "sonarr")],
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "unarrd kept running with nothing to serve"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success());
}
