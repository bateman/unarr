//! The minimal SignalR surface the official UI needs: `negotiate`, then the
//! JSON hub protocol over a WebSocket. Only WebSockets are offered as a
//! working transport; the negotiate response still lists the others as the
//! originals do, and the client picks WebSockets first.
//!
//! Hub protocol (version 1, JSON): messages are JSON documents terminated by
//! `\x1e`. The client opens with `{"protocol":"json","version":1}`, the
//! server answers `{}`. Type 1 is an invocation (`target`, `arguments`), type
//! 6 a ping, type 7 a close. The originals send one invocation of
//! `receiveMessage` per event, with `{name, body}`; on connect that is the
//! `version` message.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::auth::{key_matches, presented_key};
use crate::compat;
use crate::query::Params;
use crate::state::AppState;

const RECORD_SEPARATOR: char = '\u{1e}';

/// Fan-out of hub messages to every connected UI.
#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<Value>,
}

impl Default for Hub {
    fn default() -> Self {
        Hub::new()
    }
}

impl Hub {
    pub fn new() -> Hub {
        let (tx, _) = broadcast::channel(256);
        Hub { tx }
    }

    /// Send `{name, body}` to every connected client as a `receiveMessage`.
    pub fn broadcast(&self, name: &str, body: Value) {
        let _ = self.tx.send(json!({"name": name, "body": body}));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }
}

/// `access_token` query parameter, or the usual header.
fn authorized(state: &AppState, uri: &Uri, headers: &HeaderMap) -> bool {
    let settings = state.settings();
    let expected = &settings.host.api_key;
    let params = Params::from_uri(uri);
    if let Some(token) = params.get("access_token") {
        return key_matches(expected, token);
    }
    presented_key(uri, headers).is_some_and(|k| key_matches(expected, &k))
}

/// 22 characters, the length of ASP.NET connection ids. The server never
/// reads the id back (the UI connects without it), so a counter will do.
fn connection_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{n:022x}")
}

pub async fn negotiate(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    if !authorized(&state, &uri, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "negotiateVersion": 0,
        "connectionId": connection_id(),
        "availableTransports": [
            {"transport": "WebSockets", "transferFormats": ["Text", "Binary"]},
            {"transport": "ServerSentEvents", "transferFormats": ["Text"]},
            {"transport": "LongPolling", "transferFormats": ["Text", "Binary"]},
        ],
    }))
    .into_response()
}

pub async fn connect(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !authorized(&state, &uri, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| serve(socket, state))
}

fn frame(v: &Value) -> Message {
    Message::Text(format!("{v}{RECORD_SEPARATOR}").into())
}

/// Split a text frame into its `\x1e`-terminated JSON records.
fn records(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.split(RECORD_SEPARATOR)
        .filter(|r| !r.trim().is_empty())
        .filter_map(|r| serde_json::from_str(r).ok())
}

async fn serve(mut socket: WebSocket, state: AppState) {
    // Handshake: the first record names the protocol.
    let handshake_ok = loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                let mut ok = false;
                for r in records(&text) {
                    if r.get("protocol").is_some() {
                        ok = r["protocol"] == "json" && r["version"].as_i64().unwrap_or(1) == 1;
                    }
                }
                break ok;
            }
            Some(Ok(_)) => continue,
            _ => break false,
        }
    };
    if !handshake_ok {
        let _ = socket
            .send(frame(
                &json!({"error": "Unarr only supports the JSON hub protocol, version 1"}),
            ))
            .await;
        return;
    }
    if socket.send(frame(&json!({}))).await.is_err() {
        return;
    }
    tracing::debug!("signalr client connected");

    let mut rx = state.hub().subscribe();
    let version = json!({
        "name": "version",
        "body": {"version": compat::emulated_version(state.personality())},
    });
    if socket
        .send(frame(
            &json!({"type": 1, "target": "receiveMessage", "arguments": [version]}),
        ))
        .await
        .is_err()
    {
        return;
    }

    let mut ping = tokio::time::interval(Duration::from_secs(15));
    ping.tick().await;
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        for r in records(&text) {
                            match r["type"].as_i64() {
                                Some(7) => return,
                                // An invocation we do not implement: complete it with an error.
                                Some(1) => {
                                    if let Some(id) = r.get("invocationId") {
                                        let reply = json!({"type": 3, "invocationId": id, "error": "not supported"});
                                        if socket.send(frame(&reply)).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => {}
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(message) => {
                        let v = json!({"type": 1, "target": "receiveMessage", "arguments": [message]});
                        if socket.send(frame(&v)).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = ping.tick() => {
                if socket.send(frame(&json!({"type": 6}))).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_ids_look_right_and_records_split() {
        let id = connection_id();
        assert_eq!(id.len(), 22);
        assert_ne!(id, connection_id());
        let recs: Vec<Value> =
            records("{\"protocol\":\"json\",\"version\":1}\u{1e}{\"type\":6}\u{1e}").collect();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[1]["type"], 6);
    }
}
