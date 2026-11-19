//! What the golden replays of every personality share: the request list
//! and the recorded answers, the comparison modes and the differences
//! reported. Each replay keeps its own fixture, state and mode table.

#![allow(dead_code)]

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

pub enum Mode {
    /// Full comparison, ignoring the named keys wherever they appear.
    Exact(&'static [&'static str]),
    /// `Exact`, with every list of identified objects compared in id order:
    /// the originals list the queue from a concurrent dictionary, so ties
    /// on the sort key come out in any order.
    Unordered(&'static [&'static str]),
    /// `Exact` over the commands a client started: the originals' own
    /// refreshes and the scheduler's commands come and go with the clock.
    ManualCommands(&'static [&'static str]),
    /// `Exact` over the commands a client or a write queued, in any
    /// order: the downloads' refresh of each minute and what it queues are
    /// the clock's, and commands that run side by side end as they may.
    QueuedCommands(&'static [&'static str]),
    /// `Exact` over a list whose order the originals do not fix: the
    /// notifications a delete leaves, where the media server's refresh
    /// runs on a background handler and lands wherever it may.
    Multiset(&'static [&'static str]),
    /// Key structure only.
    Shape,
    /// An HTML page: status and the page title.
    Html,
    /// An iCalendar feed: every line but `DTSTAMP`, which is the instant
    /// the feed was written.
    Calendar,
}

/// A list in a fixed order, for `Mode::Multiset`.
pub fn sorted_list(v: Value) -> Value {
    match v {
        Value::Array(mut items) => {
            items.sort_by_key(|i| i.to_string());
            Value::Array(items)
        }
        other => other,
    }
}

/// Drop ignored keys and normalise numbers so `100` equals `100.0`.
pub fn normalize(v: &Value, ignore: &[&str]) -> Value {
    normalize_with(v, ignore, false)
}

pub fn normalize_with(v: &Value, ignore: &[&str], sort_by_id: bool) -> Value {
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
        // A cover's `lastWrite` is the moment its file was written.
        Value::String(text) => match text.find("?lastWrite=") {
            Some(at) => Value::String(format!("{}?lastWrite=…", &text[..at])),
            None => v.clone(),
        },
        _ => v.clone(),
    }
}

/// Key paths of a JSON tree, arrays reduced to their first element.
pub fn shape(v: &Value, prefix: &str, out: &mut BTreeSet<String>) {
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
pub fn diff(path: &str, want: &Value, got: &Value, out: &mut Vec<String>) {
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

pub fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 {
        format!("{}...", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}

#[derive(Clone)]
pub struct Golden {
    pub name: String,
    /// `sleep` for a pause in the recording: the replay waits for the
    /// command queue instead.
    pub method: String,
    /// A `mock` line: sent to the mock services, not to Unarr.
    pub to_mock: bool,
    pub path: String,
    /// JSON request body, from the file the fourth column names.
    pub request_body: Option<String>,
    pub status: u16,
    pub body: Value,
    /// When the real application answered; Unarr's clock is pinned to it.
    pub recorded_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A `{"$from": name, "$set": {...}}` body: the body the golden `name`
/// recorded, its top-level keys replaced (`scripts/record-golden.py`).
pub fn derive_body(text: &str, golden_dir: &std::path::Path) -> String {
    let Ok(Value::Object(spec)) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    let Some(from) = spec.get("$from").and_then(Value::as_str) else {
        return text.to_string();
    };
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(golden_dir.join(format!("{from}.json")))
            .unwrap_or_else(|e| panic!("body derived from {from}: {e}")),
    )
    .unwrap();
    let mut body = match &recorded["body"] {
        Value::Array(items) => items.first().cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    if let (Value::Object(target), Some(Value::Object(set))) = (&mut body, spec.get("$set")) {
        for (k, v) in set {
            target.insert(k.clone(), v.clone());
        }
    }
    python_dumps(&body)
}

/// `json.dumps` as the recorder writes a derived body: `, ` and `: `
/// between items, non-ASCII escaped. A model binding error names the byte
/// it stopped at, so the replay sends the same bytes.
pub fn python_dumps(v: &Value) -> String {
    fn string(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7f => {
                    let mut buf = [0u16; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    walk(item, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    string(k, out);
                    out.push_str(": ");
                    walk(item, out);
                }
                out.push('}');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    out
}

/// The request list of `golden_dir` with what the original answered.
pub fn load_goldens(golden_dir: &std::path::Path) -> Vec<Golden> {
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
            let text = std::fs::read_to_string(golden_dir.join(body_file.trim()))
                .unwrap_or_else(|e| panic!("body file {body_file} for {name}: {e}"));
            derive_body(&text, golden_dir)
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
pub async fn login_cookie(app: &axum::Router) -> String {
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
pub fn substitute_ids(path: &str, ids: &std::collections::HashMap<String, i64>) -> String {
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

/// The differences between what the original answered and what Unarr did,
/// under a comparison mode.
pub fn compare(mode: Mode, want_body: &Value, got_body: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    match mode {
        Mode::Exact(ignore) => {
            let want = normalize(want_body, ignore);
            let got = normalize(got_body, ignore);
            diff("", &want, &got, &mut problems);
        }
        Mode::Unordered(ignore) => {
            let want = normalize_with(want_body, ignore, true);
            let got = normalize_with(got_body, ignore, true);
            diff("", &want, &got, &mut problems);
        }
        Mode::Multiset(ignore) => {
            let want = sorted_list(normalize(want_body, ignore));
            let got = sorted_list(normalize(got_body, ignore));
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
            let want = normalize(&manual(want_body), ignore);
            let got = normalize(&manual(got_body), ignore);
            diff("", &want, &got, &mut problems);
        }
        Mode::QueuedCommands(ignore) => {
            let queued = |v: &Value| -> Value {
                Value::Array(
                    v.as_array()
                        .map(|a| {
                            a.iter()
                                .filter(|c| {
                                    c["trigger"] != "scheduled"
                                        && c["name"] != "RefreshMonitoredDownloads"
                                        && c["name"] != "ProcessMonitoredDownloads"
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default(),
                )
            };
            let want = sorted_list(normalize(&queued(want_body), ignore));
            let got = sorted_list(normalize(&queued(got_body), ignore));
            diff("", &want, &got, &mut problems);
        }
        Mode::Calendar => {
            let lines = |v: &Value| -> Vec<String> {
                v.as_str()
                    .unwrap_or("")
                    .lines()
                    .filter(|l| !l.starts_with("DTSTAMP:"))
                    .map(str::to_string)
                    .collect()
            };
            let (want, got) = (lines(want_body), lines(got_body));
            if want != got {
                let at = want
                    .iter()
                    .zip(&got)
                    .position(|(w, g)| w != g)
                    .unwrap_or(want.len().min(got.len()));
                problems.push(format!(
                    "calendar: {} lines expected, {} got; first difference at line {}: expected {:?}, got {:?}",
                    want.len(),
                    got.len(),
                    at + 1,
                    want.get(at),
                    got.get(at)
                ));
            }
        }
        Mode::Html => {
            let title = |v: &Value| {
                v.as_str()
                    .and_then(|s| s.split("<title>").nth(1))
                    .and_then(|s| s.split("</title>").next())
                    .map(str::to_string)
            };
            if title(want_body).is_none() || title(want_body) != title(got_body) {
                problems.push(format!(
                    "html title: expected {:?}, got {:?}",
                    title(want_body),
                    title(got_body)
                ));
            }
        }
        Mode::Shape => {
            let (mut want, mut got) = (BTreeSet::new(), BTreeSet::new());
            shape(want_body, "", &mut want);
            shape(got_body, "", &mut got);
            for k in want.difference(&got) {
                problems.push(format!("{k}: missing"));
            }
            for k in got.difference(&want) {
                problems.push(format!("{k}: unexpected key"));
            }
        }
    }
    problems
}

/// Every string in `v` with the replay directories mapped back to the
/// recorded ones.
pub fn unmap_paths(v: &Value, mappings: &[(String, &str)]) -> Value {
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

/// The recording's directories (`/tv`, `/movies`, `/downloads`) in a request's path or body
/// become the replay directories: after `=` or `"`, up to the next `/`,
/// `"`, `&` or the end, so that `/downloads/tv/...` maps once.
pub fn map_paths(text: &str, mappings: &[(String, &str)]) -> String {
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

/// `golden-housekeeping`: the database as its `setup.sh` leaves the
/// original's (a second user, the health check's last run a day ahead),
/// the `Housekeeping` command posted, and what is left compared with the
/// `db-after.json` written from the original's database: the user it
/// kept, and no task's last run still ahead.
pub async fn housekeeping_repairs_as_the_original(
    state: &unarr_api::AppState,
    golden_dir: &std::path::Path,
    api_key: &str,
) {
    let after: Value =
        serde_json::from_str(&std::fs::read_to_string(golden_dir.join("db-after.json")).unwrap()).unwrap();
    let ahead = state.now() + chrono::Duration::days(1);
    state
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO users (identifier, username, password) VALUES ('5f0c6a3e-0000-4000-8000-000000000002', 'second', 'x')",
                [],
            )?;
            let task = unarr_core::store::providers::list_scheduled_tasks(conn)?
                .into_iter()
                .find(|t| t.type_name.ends_with(".CheckHealthCommand"))
                .expect("the health check's task");
            unarr_core::store::providers::set_scheduled_task_execution(conn, task.id, ahead, ahead)?;
            Ok(())
        })
        .await
        .unwrap();
    state.start_runtime().await.expect("the command runtime");
    let app = unarr_api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/command")
                .header("X-Api-Key", api_key)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name": "Housekeeping"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        state.commands().unwrap().wait_idle(),
    )
    .await
    .expect("the housekeeping finishes within a minute");
    let now = state.now();
    let (users, ahead): (Vec<String>, Vec<String>) = state
        .db()
        .call(move |conn| {
            let users = unarr_core::store::providers::list_users(conn)?
                .into_iter()
                .map(|u| u.username)
                .collect();
            let ahead = unarr_core::store::providers::list_scheduled_tasks(conn)?
                .into_iter()
                .filter(|t| t.last_execution.is_some_and(|last| last > now))
                .map(|t| t.type_name)
                .collect();
            Ok((users, ahead))
        })
        .await
        .unwrap();
    assert_eq!(serde_json::json!(users), after["users"]);
    let recorded_ahead: Vec<&Value> = after["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["inTheFuture"] == true)
        .collect();
    assert!(
        recorded_ahead.is_empty(),
        "the recording left {recorded_ahead:?} ahead"
    );
    assert!(ahead.is_empty(), "still ahead after the housekeeping: {ahead:?}");
}
