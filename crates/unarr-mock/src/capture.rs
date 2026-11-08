//! Replays of exchanges recorded with real download clients.
//!
//! `scripts/downloadclient-capture.py` writes what Sonarr asked a real
//! SABnzbd, NZBGet, qBittorrent or Transmission and what it answered to
//! `tests/fixtures/downloadclients/<name>/capture.json`. A request to
//! `/capture/<name>/<path>` on the mock is answered with the recorded
//! response to the same request (method, path, query and, for JSON-RPC, the
//! method called), so Unarr's clients run against what the real programs
//! said rather than against what the mock thinks they say. A request nothing
//! recorded answers 404, which a test sees as a client call the capture did
//! not cover.

use std::collections::BTreeMap;
use std::path::Path;

use axum::http::{Method, StatusCode};
use serde_json::Value;

use crate::clients::{percent_decode, Reply};

/// The capture directories under `fixtures/downloadclients`, by name.
pub fn names(fixtures: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(fixtures.join("downloadclients")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join("capture.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

pub fn handle(
    fixtures: &Path,
    method: &Method,
    path: &str,
    query: &BTreeMap<String, String>,
    body: &[u8],
) -> Option<Reply> {
    let rest = path.strip_prefix("/capture/")?;
    let (name, rest) = rest.split_once('/').map_or((rest, ""), |(n, r)| (n, r));
    let wanted = format!("/{rest}");
    let file = fixtures.join("downloadclients").join(name).join("capture.json");
    let miss = |note: &str| Reply {
        status: StatusCode::NOT_FOUND,
        body: format!("not in the capture: {note}\n").into_bytes(),
        content_type: "text/plain; charset=utf-8",
        headers: Vec::new(),
        served: format!("capture/{name}/miss"),
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Some(miss(&format!("no capture named {name}")));
    };
    let exchanges: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
    // A JSON-RPC call names its method in the body; the capture keeps
    // Sonarr's summary of it, `method(params)`.
    let rpc_method = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["method"].as_str().map(String::from));
    let found = exchanges.iter().find(|x| {
        let request = &x["request"];
        if request["method"].as_str() != Some(method.as_str()) {
            return false;
        }
        let (recorded_path, recorded_query) = split_query(request["path"].as_str().unwrap_or(""));
        if recorded_path != wanted || recorded_query != *query {
            return false;
        }
        match (&rpc_method, request["body"].as_str()) {
            (Some(m), Some(summary)) => summary == m || summary.starts_with(&format!("{m}(")),
            _ => true,
        }
    });
    let Some(exchange) = found else {
        let call = rpc_method.map(|m| format!(" {m}()")).unwrap_or_default();
        return Some(miss(&format!("{method} {wanted}{call}")));
    };
    let response = &exchange["response"];
    let status = response["status"]
        .as_u64()
        .and_then(|s| StatusCode::from_u16(s as u16).ok())
        .unwrap_or(StatusCode::OK);
    let content_type = response["headers"]
        .as_object()
        .and_then(|h| h.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    let content_type: &'static str = if content_type.contains("json") {
        "application/json"
    } else if content_type.contains("xml") {
        "application/xml"
    } else if content_type.contains("html") {
        "text/html; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    };
    let body = match &response["body"] {
        Value::String(s) => s.as_bytes().to_vec(),
        Value::Object(o) => o["base64"]
            .as_str()
            .and_then(|b| {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.decode(b).ok()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let what = exchange["request"]["body"]
        .as_str()
        .map(|s| s.split('(').next().unwrap_or(s).to_string())
        .unwrap_or_else(|| wanted.trim_start_matches('/').to_string());
    Some(Reply {
        status,
        body,
        content_type,
        headers: Vec::new(),
        served: format!("capture/{name}/{what}"),
    })
}

fn split_query(path: &str) -> (String, BTreeMap<String, String>) {
    let (p, q) = path.split_once('?').unwrap_or((path, ""));
    let query = q
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    (p.to_string(), query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_paths_and_queries() {
        let (p, q) = split_query("/api/v2/torrents/info?category=tv&filter=a%20b");
        assert_eq!(p, "/api/v2/torrents/info");
        assert_eq!(q.get("category").map(String::as_str), Some("tv"));
        assert_eq!(q.get("filter").map(String::as_str), Some("a b"));
        assert_eq!(split_query("/jsonrpc").1.len(), 0);
    }

    #[test]
    fn unknown_capture_is_a_miss() {
        let reply = handle(
            Path::new("/nonexistent"),
            &Method::GET,
            "/capture/none/api",
            &BTreeMap::new(),
            b"",
        )
        .unwrap();
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        assert_eq!(reply.served, "capture/none/miss");
    }
}
