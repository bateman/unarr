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
    body.to_string()
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
