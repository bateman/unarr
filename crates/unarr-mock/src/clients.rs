//! The four download clients the Python mock stands in for, with the same
//! state and answers (see `scripts/mock-servarr.py` for the contract):
//! SABnzbd under `/sabnzbd`, NZBGet under `/nzbget`, qBittorrent under
//! `/qbittorrent`, Transmission under `/transmission`, plus the `/mock/`
//! control endpoints that complete or fail a download, and the three
//! notification targets (a Plex Media Server at `/identity` and
//! `/library/...`, a Discord webhook under `/discord/`, a generic webhook
//! at `/webhook`) that keep what they receive for `GET /mock/notifications`.
//!
//! Completing an item puts real files where the client says they are: the
//! clients report paths under [`ClientState::downloads`] and `complete`
//! copies a clip from `tests/fixtures/media/` into the item's folder
//! (`clip=`, `extras=` as the Python mock documents).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use axum::http::{HeaderMap, Method, StatusCode};
use base64::Engine;
use serde_json::{json, Value};

use crate::API_KEY;

pub const MOCK_USER: &str = "mock";
pub const MOCK_PASSWORD: &str = "mock";
pub const QBIT_SESSION: &str = "mock-session-cookie";
pub const TRANSMISSION_SESSION: &str = "mock-transmission-session-id";
pub const ITEM_SIZE: i64 = 768_000;
pub const PLEX_TOKEN: &str = "mock-plex-token";
pub const PLEX_VERSION: &str = "1.41.0.8992-8e6b099a8";
/// A plex.tv PIN the user has signed in: reading it gives [`PLEX_TOKEN`].
pub const PLEX_PIN_SIGNED_IN: i64 = 4242;
/// A plex.tv PIN created and not signed in yet: no token.
pub const PLEX_PIN_WAITING: i64 = 4243;

/// What a handler answers.
pub struct Reply {
    pub status: StatusCode,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub headers: Vec<(String, String)>,
    pub served: String,
}

impl From<(StatusCode, Vec<u8>, &'static str, String)> for Reply {
    fn from((status, body, content_type, served): (StatusCode, Vec<u8>, &'static str, String)) -> Self {
        Reply {
            status,
            body,
            content_type,
            headers: Vec::new(),
            served,
        }
    }
}

fn json_reply(status: StatusCode, value: Value, served: impl Into<String>) -> Reply {
    Reply {
        status,
        body: value.to_string().into_bytes(),
        content_type: "application/json",
        headers: Vec::new(),
        served: served.into(),
    }
}

fn text_reply(status: StatusCode, text: &str, served: impl Into<String>) -> Reply {
    Reply {
        status,
        body: text.as_bytes().to_vec(),
        content_type: "text/plain; charset=utf-8",
        headers: Vec::new(),
        served: served.into(),
    }
}

/// One download a mock client holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What the client reported when adding: nzo id, drone id, info hash.
    pub id: String,
    /// NZBGet's own numeric id, Transmission's numeric id.
    pub number: i64,
    pub title: String,
    pub category: String,
    pub labels: Vec<String>,
    pub dir: String,
    pub priority: i64,
    pub size: i64,
    pub done: i64,
    pub state: ItemState,
    pub paused: bool,
    /// The form fields or arguments the add carried, for assertions.
    pub added_with: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Downloading,
    Completed,
    Failed,
}

#[derive(Debug)]
pub struct ClientState {
    pub sabnzbd: Vec<Item>,
    pub nzbget: Vec<Item>,
    pub qbittorrent: Vec<Item>,
    pub qbittorrent_categories: BTreeMap<String, String>,
    pub transmission: Vec<Item>,
    /// What the notification targets received, oldest first.
    pub notifications: Vec<Value>,
    /// The directory the clients report their files under, no trailing
    /// slash; `/downloads` unless the mock was started on a real one.
    pub downloads: String,
    /// The directories the mock was started on, the only ones it writes to
    /// or deletes from. The default ones are names to report: `/downloads`
    /// may be a real folder of the machine the tests run on.
    pub owned: Vec<String>,
    /// The TV library location Plex reports.
    pub tv: String,
    /// The movie library location Plex reports.
    pub movies: String,
    /// The same when a test moved it away from `movies`
    /// (`POST /mock/plex/sections?movies=<path>`).
    pub plex_movies: Option<String>,
    /// `tests/fixtures/media`, the clips `complete` copies.
    pub media: PathBuf,
    seq: i64,
    initialised: bool,
}

impl Default for ClientState {
    fn default() -> Self {
        ClientState {
            sabnzbd: Vec::new(),
            nzbget: Vec::new(),
            qbittorrent: Vec::new(),
            qbittorrent_categories: BTreeMap::new(),
            transmission: Vec::new(),
            notifications: Vec::new(),
            downloads: "/downloads".into(),
            owned: Vec::new(),
            tv: "/tv".into(),
            movies: "/movies".into(),
            plex_movies: None,
            media: crate::fixtures_dir().join("media"),
            seq: 0,
            initialised: false,
        }
    }
}

impl ClientState {
    fn init(&mut self) {
        if !self.initialised {
            self.reset();
        }
    }

    /// Forget every item and notification; the directories stay.
    pub fn reset(&mut self) {
        let (downloads, owned, tv, movies, media) = (
            std::mem::take(&mut self.downloads),
            std::mem::take(&mut self.owned),
            std::mem::take(&mut self.tv),
            std::mem::take(&mut self.movies),
            std::mem::take(&mut self.media),
        );
        *self = ClientState {
            downloads,
            owned,
            tv,
            movies,
            media,
            ..ClientState::default()
        };
        self.qbittorrent_categories.insert("tv".into(), String::new());
        self.qbittorrent_categories
            .insert("movies".into(), self.dl("/movies"));
        self.initialised = true;
    }

    /// A path under the downloads directory the clients report.
    pub fn dl(&self, rest: &str) -> String {
        format!("{}{rest}", self.downloads)
    }

    fn qbit_save_path(&self, t: &Item) -> String {
        if t.category.is_empty() {
            self.dl("/")
        } else {
            self.dl(&format!("/{}/", t.category))
        }
    }

    /// The folder a client reports for a finished item, no trailing slash.
    pub fn complete_dir(&self, client: &str, item: &Item) -> String {
        match client {
            "sabnzbd" => self.dl(&format!("/complete/{}/{}", item.category, item.title)),
            "nzbget" => self.dl(&format!("/completed/{}/{}", item.category, item.title)),
            "qbittorrent" => format!("{}{}", self.qbit_save_path(item), item.title),
            _ => format!("{}/{}", item.dir.trim_end_matches('/'), item.title),
        }
    }

    /// Only the directories the mock was started on are written to or
    /// deleted from.
    pub fn inside_mock_dirs(&self, path: &str) -> bool {
        if !path.starts_with('/') || format!("{path}/").contains("/../") {
            return false;
        }
        self.owned
            .iter()
            .any(|d| path == d.as_str() || path.starts_with(&format!("{d}/")))
    }

    /// What a real client does when asked to delete an item's data.
    fn delete_data(&self, client: &str, item: &Item) {
        let folder = self.complete_dir(client, item);
        if self.inside_mock_dirs(&folder) {
            remove_path(&folder);
        }
    }

    /// Copy the fixture clips into `folder` as the download's files:
    /// `clip` names the main one (`<title>.mkv`), `extras` adds
    /// `<clip>:<relative name>` pairs with `{title}` replaced.
    fn materialise(&self, folder: &str, title: &str, query: &BTreeMap<String, String>) {
        if !self.inside_mock_dirs(folder) {
            tracing::debug!(folder, "not a directory the mock was started on; nothing written");
            return;
        }
        let target = PathBuf::from(folder);
        let mut files = vec![(
            query.get("clip").cloned().unwrap_or_else(|| "episode".into()),
            format!("{title}.mkv"),
        )];
        for extra in query
            .get("extras")
            .map(String::as_str)
            .unwrap_or("")
            .split(',')
            .filter(|e| !e.is_empty())
        {
            let (clip, name) = extra.split_once(':').unwrap_or((extra, ""));
            let name = if name.is_empty() {
                format!("{title}.{clip}.mkv")
            } else {
                name.replace("{title}", title)
            };
            files.push((clip.to_string(), name));
        }
        for (clip, name) in files {
            let source = self.media.join(format!("{clip}.mkv"));
            let dest = target.join(&name);
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::copy(&source, &dest) {
                Ok(n) => tracing::debug!(path = %dest.display(), bytes = n, "mock wrote a download file"),
                Err(e) => {
                    tracing::warn!(clip = %source.display(), error = %e, "no clip to copy; the folder stays empty")
                }
            }
        }
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    fn items_mut(&mut self, client: &str) -> Option<&mut Vec<Item>> {
        match client {
            "sabnzbd" => Some(&mut self.sabnzbd),
            "nzbget" => Some(&mut self.nzbget),
            "qbittorrent" => Some(&mut self.qbittorrent),
            "transmission" => Some(&mut self.transmission),
            _ => None,
        }
    }

    /// Move an item along, as the control endpoints do; a completed item
    /// gets the default clip as its file.
    pub fn advance(&mut self, client: &str, id: &str, state: ItemState) -> bool {
        self.advance_with(client, id, state, &BTreeMap::new())
    }

    /// Whether a control request's `id=` or `title=` names `item`.
    fn selects(item: &Item, id: &str, query: &BTreeMap<String, String>) -> bool {
        if !id.is_empty() && item.id.eq_ignore_ascii_case(id) {
            return true;
        }
        query
            .get("title")
            .is_some_and(|t| !t.is_empty() && item.title.eq_ignore_ascii_case(t))
    }

    /// `advance` with the `clip` and `extras` of a `complete` request, which
    /// may name the item by `title=` instead of its id.
    pub fn advance_with(
        &mut self,
        client: &str,
        id: &str,
        state: ItemState,
        query: &BTreeMap<String, String>,
    ) -> bool {
        let Some(items) = self.items_mut(client) else {
            return false;
        };
        let mut found = None;
        for item in items.iter_mut() {
            if ClientState::selects(item, id, query) {
                item.state = state;
                if state == ItemState::Completed {
                    item.done = item.size;
                }
                found = Some(item.clone());
                break;
            }
        }
        let Some(item) = found else {
            return false;
        };
        if state == ItemState::Completed {
            let folder = self.complete_dir(client, &item);
            self.materialise(&folder, &item.title, query);
        }
        true
    }
}

/// Delete a file or a folder tree the mock put on disk.
fn remove_path(path: &str) -> bool {
    let target = std::path::Path::new(path);
    if target.is_dir() {
        std::fs::remove_dir_all(target).is_ok()
    } else if target.is_file() {
        std::fs::remove_file(target).is_ok()
    } else {
        false
    }
}

fn new_item(id: String, number: i64, title: String, category: String, size: i64) -> Item {
    Item {
        id,
        number,
        title: title.strip_suffix(".mkv").map(String::from).unwrap_or(title),
        category,
        labels: Vec::new(),
        dir: String::new(),
        priority: 0,
        size: if size > 0 { size } else { ITEM_SIZE },
        done: 0,
        state: ItemState::Downloading,
        paused: false,
        added_with: BTreeMap::new(),
    }
}

fn basic_auth_ok(headers: &HeaderMap) -> bool {
    let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(encoded) = auth.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let decoded = String::from_utf8_lossy(&decoded);
    decoded.split_once(':') == Some((MOCK_USER, MOCK_PASSWORD))
}

/// Form parts by name: (filename, data).
fn parse_multipart(body: &[u8], content_type: &str) -> BTreeMap<String, (Option<String>, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let Some(boundary) = content_type.split("boundary=").nth(1) else {
        return out;
    };
    let boundary = boundary.split(';').next().unwrap_or("").trim().trim_matches('"');
    let marker = format!("--{boundary}").into_bytes();
    let mut rest = body;
    let mut parts = Vec::new();
    while let Some(pos) = find(rest, &marker) {
        let (before, after) = rest.split_at(pos);
        parts.push(before.to_vec());
        rest = &after[marker.len()..];
    }
    parts.push(rest.to_vec());
    for part in parts.into_iter().skip(1) {
        if part.starts_with(b"--") {
            break;
        }
        let part = strip_prefix_crlf(&part);
        let Some(split) = find(part, b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&part[..split]).into_owned();
        let mut data = &part[split + 4..];
        if data.ends_with(b"\r\n") {
            data = &data[..data.len() - 2];
        }
        let mut name = None;
        let mut filename = None;
        for line in head.split("\r\n") {
            if line.to_ascii_lowercase().starts_with("content-disposition:") {
                for piece in line.split(';').skip(1) {
                    let (k, v) = piece.trim().split_once('=').unwrap_or(("", ""));
                    let v = v.trim_matches('"').to_string();
                    match k {
                        "name" => name = Some(v),
                        "filename" => filename = Some(v),
                        _ => {}
                    }
                }
            }
        }
        if let Some(name) = name {
            out.insert(name, (filename, data.to_vec()));
        }
    }
    out
}

fn strip_prefix_crlf(part: &[u8]) -> &[u8] {
    let mut p = part;
    while p.starts_with(b"\r\n") {
        p = &p[2..];
    }
    p
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A posted form, urlencoded or multipart; file parts come back as raw bytes
/// under their field name.
fn form(headers: &HeaderMap, body: &[u8]) -> (BTreeMap<String, String>, BTreeMap<String, Vec<u8>>) {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut fields = BTreeMap::new();
    let mut files = BTreeMap::new();
    if content_type.starts_with("multipart/form-data") {
        for (name, (filename, data)) in parse_multipart(body, content_type) {
            match filename {
                Some(_) => {
                    files.insert(name, data);
                }
                None => {
                    fields.insert(name, String::from_utf8_lossy(&data).into_owned());
                }
            }
        }
    } else {
        for (k, v) in form_urlencoded(body) {
            fields.insert(k, v);
        }
    }
    (fields, files)
}

fn form_urlencoded(body: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(body);
    text.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hms(seconds: i64) -> String {
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

/// Route a request under one of the client prefixes; `None` when the path
/// belongs to no client.
pub fn handle(
    state: &Mutex<ClientState>,
    method: &Method,
    path: &str,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) -> Option<Reply> {
    let mut state = state.lock().unwrap();
    state.init();
    if path == "/sabnzbd/api" {
        return Some(sabnzbd(&mut state, query, headers, body));
    }
    if path == "/nzbget/jsonrpc" {
        return Some(nzbget(&mut state, headers, body));
    }
    if let Some(rest) = path.strip_prefix("/qbittorrent/api/v2/") {
        return Some(qbittorrent(&mut state, rest, query, headers, body));
    }
    if path == "/transmission/rpc" {
        return Some(transmission(&mut state, method, headers, body));
    }
    if let Some(rest) = path.strip_prefix("/mock/") {
        return Some(control(&mut state, rest, query));
    }
    if path == "/identity" || path.starts_with("/library/") {
        return Some(plex(&mut state, method, path, query, headers, body));
    }
    // plex.tv's ping, through `Upstreams::mock`: kept apart from the
    // notifications the goldens list, which never saw it (the originals
    // pinged the real plex.tv while they were recorded).
    if path == "/plex.tv/api/v2/ping" {
        return Some(json_reply(StatusCode::OK, json!({"pong": true}), "plex.tv/ping"));
    }
    if let Some(id) = path.strip_prefix("/plex.tv/api/v2/pins/") {
        return Some(plex_tv_pin(id, query));
    }
    if path == "/plex.tv/api/v2/resources" {
        return Some(plex_tv_resources(query));
    }
    if path.starts_with("/discord/") {
        return Some(discord(&mut state, method, path, query, headers, body));
    }
    if path == "/webhook" || path.starts_with("/webhook/") {
        return Some(webhook(&mut state, method, path, query, headers, body));
    }
    None
}

/// `GET plex.tv/api/v2/pins/{id}`, in the shapes plex.tv answered on
/// 21 November 2026: the PIN (its `authToken` null until signed in), or
/// 404 for a PIN it does not know.
fn plex_tv_pin(id: &str, query: &BTreeMap<String, String>) -> Reply {
    let token = match id.parse::<i64>() {
        Ok(PLEX_PIN_SIGNED_IN) => Value::String(PLEX_TOKEN.into()),
        Ok(PLEX_PIN_WAITING) => Value::Null,
        _ => {
            return json_reply(
                StatusCode::NOT_FOUND,
                json!({"errors": [{"code": 1020, "message": "Code not found or expired", "status": 404}]}),
                "plex.tv/pin-404",
            )
        }
    };
    json_reply(
        StatusCode::OK,
        json!({
            "id": id.parse::<i64>().unwrap_or(0),
            "code": "kfh6p48ikyiaiziy3a3exxbtz",
            "product": query.get("X-Plex-Product").cloned().unwrap_or_default(),
            "trusted": false,
            "qr": "https://plex.tv/api/v2/pins/qr/kfh6p48ikyiaiziy3a3exxbtz",
            "clientIdentifier": query.get("X-Plex-Client-Identifier").cloned().unwrap_or_default(),
            "expiresIn": 1788,
            "createdAt": "2026-11-21T08:39:09Z",
            "expiresAt": "2026-11-21T09:09:09Z",
            "authToken": token,
            "newRegistration": null,
        }),
        "plex.tv/pin",
    )
}

/// `GET plex.tv/api/v2/resources`: for [`PLEX_TOKEN`] an account with its
/// own server (a secure local connection and a remote one), a server
/// shared with it and a player; any other token is refused as plex.tv
/// refused one on 21 November 2026. The resources' shape is plex.tv's
/// documented one: no account's list was recorded.
fn plex_tv_resources(query: &BTreeMap<String, String>) -> Reply {
    if query.get("X-Plex-Token").map(String::as_str) != Some(PLEX_TOKEN) {
        return json_reply(
            StatusCode::UNAUTHORIZED,
            json!({"errors": [{"code": 1001, "message": "User could not be authenticated", "status": 401}]}),
            "plex.tv/resources-401",
        );
    }
    json_reply(
        StatusCode::OK,
        json!([
            {"name": "Mock Plex", "product": "Plex Media Server", "productVersion": PLEX_VERSION, "platform": "Linux",
             "clientIdentifier": "6d0f2a9c1b3e4d5f8a7b6c5d4e3f2a1b0c9d8e7f", "provides": "server", "owned": true,
             "accessToken": "mock-server-token", "publicAddress": "203.0.113.7", "httpsRequired": false,
             "relay": true, "presence": true, "connections": [
                {"protocol": "https", "address": "192.168.1.10", "port": 32400,
                 "uri": "https://192-168-1-10.0123456789abcdef.plex.direct:32400", "local": true, "relay": false, "IPv6": false},
                {"protocol": "http", "address": "203.0.113.7", "port": 32401,
                 "uri": "http://203.0.113.7:32401", "local": false, "relay": false, "IPv6": false}
             ]},
            {"name": "A friend's Plex", "product": "Plex Media Server", "provides": "server", "owned": false,
             "connections": [{"protocol": "https", "address": "198.51.100.2", "port": 32400,
                 "uri": "https://198-51-100-2.fedcba9876543210.plex.direct:32400", "local": false}]},
            {"name": "Living room", "product": "Plex for Android (TV)", "provides": "client,player", "owned": true,
             "connections": [{"protocol": "http", "address": "192.168.1.20", "port": 32500,
                 "uri": "http://192.168.1.20:32500", "local": true}]}
        ]),
        "plex.tv/resources",
    )
}

fn control(state: &mut ClientState, rest: &str, query: &BTreeMap<String, String>) -> Reply {
    if rest == "reset" {
        state.reset();
        return json_reply(StatusCode::OK, json!({"ok": true}), "mock/reset");
    }
    if rest == "notifications" {
        return json_reply(
            StatusCode::OK,
            Value::Array(state.notifications.clone()),
            "mock/notifications",
        );
    }
    if rest == "notifications/clear" {
        state.notifications.clear();
        return json_reply(StatusCode::OK, json!({"ok": true}), "mock/notifications/clear");
    }
    if rest == "plex/sections" {
        // Where Plex says its movie library is, when it sees the files
        // under another path than the application does.
        state.plex_movies = query
            .get("movies")
            .map(|p| p.trim_end_matches('/').to_string())
            .filter(|p| !p.is_empty());
        let movies = state.plex_movies.clone().unwrap_or_else(|| state.movies.clone());
        return json_reply(
            StatusCode::OK,
            json!({"ok": true, "movies": movies}),
            "mock/plex/sections",
        );
    }
    if rest == "fs/write" || rest == "fs/remove" {
        let path = query.get("path").cloned().unwrap_or_default();
        if !state.inside_mock_dirs(&path) {
            return json_reply(
                StatusCode::BAD_REQUEST,
                json!({"ok": false, "error": match state.owned.as_slice() {
                    [] => format!("{path}: the mock was started on no directory and writes nowhere"),
                    owned => format!("{path} is not under {}", owned.join(", ")),
                }}),
                format!("mock/{rest}"),
            );
        }
        if rest == "fs/write" {
            let clip = query.get("clip").cloned().unwrap_or_else(|| "episode".into());
            let source = state.media.join(format!("{clip}.mkv"));
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            return match std::fs::copy(&source, &path) {
                Ok(size) => json_reply(StatusCode::OK, json!({"ok": true, "size": size}), "mock/fs/write"),
                Err(e) => json_reply(
                    StatusCode::NOT_FOUND,
                    json!({"ok": false, "error": format!("no clip {}: {e}", source.display())}),
                    "mock/fs/write",
                ),
            };
        }
        let removed = remove_path(&path);
        return json_reply(
            if removed {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
            json!({"ok": removed}),
            "mock/fs/remove",
        );
    }
    let (client, action) = rest.split_once('/').unwrap_or((rest, ""));
    let target = match action {
        "complete" => ItemState::Completed,
        "fail" => ItemState::Failed,
        _ => return text_reply(StatusCode::NOT_FOUND, "not found\n", "404"),
    };
    let id = query.get("id").cloned().unwrap_or_default();
    let found = state.advance_with(client, &id, target, query);
    json_reply(
        if found {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        },
        json!({"ok": found}),
        format!("mock/{client}/{action}"),
    )
}

// --- Notification targets: Plex Media Server, Discord, a generic webhook

/// Keep what a notification target received: method, path, query, the
/// headers that matter (content type, authorization, `X-*`) and the body
/// decoded as JSON when it is.
fn notified(
    state: &mut ClientState,
    service: &str,
    method: &Method,
    path: &str,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) {
    let mut kept = serde_json::Map::new();
    for (name, value) in headers {
        let name = name.as_str().to_ascii_lowercase();
        if name == "content-type" || name == "authorization" || name.starts_with("x-") {
            kept.insert(name, Value::from(value.to_str().unwrap_or("")));
        }
    }
    let decoded = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::from(String::from_utf8_lossy(body).into_owned()))
    };
    state.notifications.push(json!({
        "service": service,
        "method": method.as_str(),
        "path": path,
        "query": query,
        "headers": kept,
        "body": decoded,
    }));
}

fn plex(
    state: &mut ClientState,
    method: &Method,
    path: &str,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) -> Reply {
    notified(state, "plex", method, path, query, headers, body);
    if query.get("X-Plex-Token").map(String::as_str) != Some(PLEX_TOKEN) {
        return json_reply(
            StatusCode::UNAUTHORIZED,
            json!({"errors": [{"code": 1001, "message": "Unauthorized", "status": 401}]}),
            "plex/401",
        );
    }
    if path == "/identity" {
        return json_reply(
            StatusCode::OK,
            json!({"MediaContainer": {"size": 0, "claimed": true, "machineIdentifier": "6d0f2a9c1b3e4d5f8a7b6c5d4e3f2a1b0c9d8e7f", "version": PLEX_VERSION}}),
            "plex/identity",
        );
    }
    if path == "/library/sections" {
        return json_reply(
            StatusCode::OK,
            json!({"MediaContainer": {"size": 2, "allowSync": false, "title1": "Plex Library", "Directory": [
                {"allowSync": true, "art": "/:/resources/show-fanart.jpg", "composite": "/library/sections/2/composite/1736478720", "filters": true, "refreshing": false, "thumb": "/:/resources/show.png", "key": "2", "type": "show", "title": "TV Shows", "agent": "tv.plex.agents.series", "scanner": "Plex TV Series", "language": "en-US", "uuid": "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0", "updatedAt": 1736478720, "createdAt": 1736478720, "scannedAt": 1736478720, "content": true, "directory": true, "contentChangedAt": 1, "hidden": 0, "Location": [{"id": 2, "path": state.tv}]},
                {"allowSync": true, "art": "/:/resources/movie-fanart.jpg", "composite": "/library/sections/1/composite/1736478720", "filters": true, "refreshing": false, "thumb": "/:/resources/movie.png", "key": "1", "type": "movie", "title": "Movies", "agent": "tv.plex.agents.movie", "scanner": "Plex Movie", "language": "en-US", "uuid": "1a2b3c4d-5e6f-7081-92a3-b4c5d6e7f809", "updatedAt": 1736478720, "createdAt": 1736478720, "scannedAt": 1736478720, "content": true, "directory": true, "contentChangedAt": 1, "hidden": 0, "Location": [{"id": 1, "path": state.plex_movies.clone().unwrap_or_else(|| state.movies.clone())}]},
            ]}}),
            "plex/sections",
        );
    }
    if let Some(section) = path
        .strip_prefix("/library/sections/")
        .and_then(|rest| rest.strip_suffix("/refresh"))
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
    {
        return Reply {
            status: StatusCode::OK,
            body: Vec::new(),
            content_type: "text/html",
            headers: Vec::new(),
            served: format!("plex/refresh-{section}"),
        };
    }
    json_reply(
        StatusCode::NOT_FOUND,
        json!({"errors": [{"code": 1002, "message": "Not Found", "status": 404}]}),
        "plex/404",
    )
}

fn discord(
    state: &mut ClientState,
    method: &Method,
    path: &str,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) -> Reply {
    notified(state, "discord", method, path, query, headers, body);
    if method != Method::POST {
        return json_reply(
            StatusCode::METHOD_NOT_ALLOWED,
            json!({"message": "405: Method Not Allowed", "code": 0}),
            "discord/405",
        );
    }
    Reply {
        status: StatusCode::NO_CONTENT,
        body: Vec::new(),
        content_type: "text/plain",
        headers: Vec::new(),
        served: "discord/webhook".into(),
    }
}

fn webhook(
    state: &mut ClientState,
    method: &Method,
    path: &str,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) -> Reply {
    notified(state, "webhook", method, path, query, headers, body);
    json_reply(StatusCode::OK, json!({"ok": true}), "webhook")
}

// --- SABnzbd

fn sabnzbd(state: &mut ClientState, q: &BTreeMap<String, String>, headers: &HeaderMap, body: &[u8]) -> Reply {
    let get = |k: &str| q.get(k).map(String::as_str).unwrap_or("");
    let ok = |v: Value, served: &str| json_reply(StatusCode::OK, v, format!("sabnzbd/{served}"));
    if let Some(key) = q.get("apikey") {
        if key != API_KEY {
            return ok(
                json!({"status": false, "error": "API Key Incorrect"}),
                "key-incorrect",
            );
        }
    } else if get("ma_username") != MOCK_USER || get("ma_password") != MOCK_PASSWORD {
        return ok(
            json!({"status": false, "error": "API Key Required"}),
            "key-required",
        );
    }
    let mode = get("mode");
    match mode {
        "version" => ok(json!({"version": "4.4.1"}), "version"),
        "get_config" => ok(
            json!({"config": {
                "misc": {
                    "complete_dir": state.dl("/complete"), "tv_categories": ["sorted"], "enable_tv_sorting": true,
                    "movie_categories": [], "enable_movie_sorting": false, "date_categories": [], "enable_date_sorting": false,
                    "pre_check": false, "history_retention": "", "history_retention_option": "all", "history_retention_number": 0,
                },
                "categories": [
                    {"name": "*", "dir": "", "priority": 0, "pp": "3", "script": "None", "newzbin": ""},
                    {"name": "tv", "dir": "tv", "priority": -100, "pp": "", "script": "Default", "newzbin": ""},
                    {"name": "movies", "dir": "movies", "priority": -100, "pp": "", "script": "Default", "newzbin": ""},
                    {"name": "nojob", "dir": "nojob*", "priority": -100, "pp": "", "script": "Default", "newzbin": ""},
                    {"name": "sorted", "dir": "sorted", "priority": -100, "pp": "", "script": "Default", "newzbin": ""},
                ],
                "servers": [],
                "sorters": [],
            }}),
            "get_config",
        ),
        "fullstatus" => ok(
            json!({"status": {"completedir": state.dl("/complete")}}),
            "fullstatus",
        ),
        "addfile" => {
            let content_type = headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let parts = parse_multipart(body, content_type);
            let Some((Some(filename), data)) = parts.get("name").cloned() else {
                return ok(
                    json!({"status": false, "error": "no nzb uploaded"}),
                    "addfile-empty",
                );
            };
            if data.is_empty() {
                return ok(
                    json!({"status": false, "error": "no nzb uploaded"}),
                    "addfile-empty",
                );
            }
            let id = format!("SABnzbd_nzo_mock{}", state.next_seq());
            let title = filename.strip_suffix(".nzb").unwrap_or(&filename).to_string();
            let mut item = new_item(id.clone(), 0, title, get("cat").to_string(), ITEM_SIZE);
            item.category = if item.category.is_empty() {
                "*".into()
            } else {
                item.category
            };
            item.priority = get("priority").parse().unwrap_or(-100);
            item.added_with = q.clone();
            state.sabnzbd.push(item);
            ok(json!({"status": true, "nzo_ids": [id]}), "addfile")
        }
        "queue" => {
            if get("name") == "delete" {
                let ids: Vec<&str> = get("value").split(',').collect();
                state
                    .sabnzbd
                    .retain(|i| !(ids.contains(&i.id.as_str()) && i.state == ItemState::Downloading));
                return ok(json!({"status": true, "nzo_ids": ids}), "queue-delete");
            }
            let category = get("category");
            let slots: Vec<Value> = state
                .sabnzbd
                .iter()
                .filter(|i| i.state == ItemState::Downloading && (category.is_empty() || i.category == category))
                .enumerate()
                .map(|(n, i)| {
                    let priority = match i.priority {
                        -2 => "Paused",
                        -1 => "Low",
                        0 => "Normal",
                        1 => "High",
                        2 => "Force",
                        _ => "Default",
                    };
                    json!({
                        "status": "Downloading", "index": n, "timeleft": hms(600),
                        "mb": format!("{:.2}", i.size as f64 / 1048576.0), "filename": i.title, "priority": priority,
                        "cat": i.category, "mbleft": format!("{:.2}", (i.size - i.done) as f64 / 1048576.0),
                        "percentage": (i.done * 100 / i.size).to_string(), "nzo_id": i.id,
                    })
                })
                .collect();
            ok(
                json!({"queue": {"my_home": state.dl(""), "paused": false, "slots": slots}}),
                "queue",
            )
        }
        "history" => {
            if get("name") == "delete" {
                let ids: Vec<&str> = get("value").split(',').collect();
                if get("del_files") == "1" {
                    for i in state
                        .sabnzbd
                        .iter()
                        .filter(|i| ids.contains(&i.id.as_str()) && i.state != ItemState::Downloading)
                    {
                        state.delete_data("sabnzbd", i);
                    }
                }
                state
                    .sabnzbd
                    .retain(|i| !(ids.contains(&i.id.as_str()) && i.state != ItemState::Downloading));
                return ok(json!({"status": true, "nzo_ids": ids}), "history-delete");
            }
            let category = get("category");
            let slots: Vec<Value> = state
                .sabnzbd
                .iter()
                .filter(|i| i.state != ItemState::Downloading && (category.is_empty() || i.category == category))
                .map(|i| {
                    let failed = i.state == ItemState::Failed;
                    json!({
                        "fail_message": if failed { "Unpacking failed, archive requires a password" } else { "" },
                        "bytes": i.size, "category": i.category, "nzb_name": format!("{}.nzb", i.title), "download_time": 60,
                        "storage": if failed { String::new() } else { state.complete_dir("sabnzbd", i) },
                        "status": if failed { "Failed" } else { "Completed" }, "nzo_id": i.id, "name": i.title,
                    })
                })
                .collect();
            ok(json!({"history": {"paused": false, "slots": slots}}), "history")
        }
        other => ok(
            json!({"status": false, "error": format!("not supported by the mock: {other}")}),
            other,
        ),
    }
}

// --- NZBGet

fn nzbget(state: &mut ClientState, headers: &HeaderMap, body: &[u8]) -> Reply {
    if !basic_auth_ok(headers) {
        let mut reply = text_reply(StatusCode::UNAUTHORIZED, "Unauthorized", "nzbget/401");
        reply
            .headers
            .push(("WWW-Authenticate".into(), "Basic realm=\"NZBGet\"".into()));
        return reply;
    }
    let call: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let method = call["method"].as_str().unwrap_or("").to_string();
    let params = call["params"].as_array().cloned().unwrap_or_default();
    let rid = call["id"].clone();
    let ok = |result: Value, served: &str| {
        json_reply(
            StatusCode::OK,
            json!({"version": "1.1", "result": result, "error": null, "id": rid}),
            format!("nzbget/{served}"),
        )
    };
    let lohi = |n: i64| (n & 0xFFFF_FFFF, n >> 32);
    match method.as_str() {
        "version" => ok(json!("24.3"), "version"),
        "config" => {
            let (main_dir, movies_dir) = (state.dl(""), state.dl("/movies"));
            let cfg = [
                ("MainDir", main_dir.as_str()),
                ("DestDir", "${MainDir}/completed"),
                ("InterDir", "${MainDir}/intermediate"),
                ("AppendCategoryDir", "yes"),
                ("KeepHistory", "7"),
                ("Category1.Name", "tv"),
                ("Category1.DestDir", ""),
                ("Category1.Unpack", "yes"),
                ("Category1.DefScript", ""),
                ("Category1.Aliases", ""),
                ("Category2.Name", "movies"),
                ("Category2.DestDir", movies_dir.as_str()),
                ("Category2.Unpack", "yes"),
                ("Category2.DefScript", ""),
                ("Category2.Aliases", ""),
            ];
            let items: Vec<Value> = cfg.iter().map(|(k, v)| json!({"Name": k, "Value": v})).collect();
            ok(Value::Array(items), "config")
        }
        "status" => {
            let remaining: i64 = state
                .nzbget
                .iter()
                .filter(|i| i.state == ItemState::Downloading)
                .map(|i| i.size - i.done)
                .sum();
            let (lo, hi) = lohi(remaining);
            ok(
                json!({"RemainingSizeLo": lo, "RemainingSizeHi": hi, "DownloadedSizeLo": 0, "DownloadedSizeHi": 0, "DownloadRate": 1048576, "AverageDownloadRate": 1048576, "DownloadLimit": 0, "DownloadPaused": false}),
                "status",
            )
        }
        "listgroups" => {
            let groups: Vec<Value> = state
                .nzbget
                .iter()
                .filter(|i| i.state == ItemState::Downloading)
                .map(|i| {
                    let (size_lo, size_hi) = lohi(i.size);
                    let (rem_lo, rem_hi) = lohi(i.size - i.done);
                    json!({"NZBID": i.number, "FirstID": i.number, "LastID": i.number, "NZBName": i.title, "Category": i.category, "FileSizeLo": size_lo, "FileSizeHi": size_hi, "RemainingSizeLo": rem_lo, "RemainingSizeHi": rem_hi, "PausedSizeLo": 0, "PausedSizeHi": 0, "MinPriority": i.priority, "MaxPriority": i.priority, "ActiveDownloads": 1, "Parameters": [{"Name": "drone", "Value": i.id}]})
                })
                .collect();
            ok(Value::Array(groups), "listgroups")
        }
        "history" => {
            let hist: Vec<Value> = state
                .nzbget
                .iter()
                .rev()
                .filter(|i| i.state != ItemState::Downloading)
                .map(|i| {
                    let (size_lo, size_hi) = lohi(i.size);
                    let failed = i.state == ItemState::Failed;
                    json!({"ID": i.number, "Name": i.title, "Category": i.category, "FileSizeLo": size_lo, "FileSizeHi": size_hi, "ParStatus": if failed { "FAILURE" } else { "SUCCESS" }, "UnpackStatus": if failed { "NONE" } else { "SUCCESS" }, "MoveStatus": if failed { "NONE" } else { "SUCCESS" }, "ScriptStatus": "NONE", "DeleteStatus": "NONE", "MarkStatus": "NONE", "DestDir": state.complete_dir("nzbget", i), "FinalDir": "", "Parameters": [{"Name": "drone", "Value": i.id}]})
                })
                .collect();
            ok(Value::Array(hist), "history")
        }
        "append" => {
            if params.len() < 10 {
                return ok(json!(0), "append-short");
            }
            let title = params[0].as_str().unwrap_or("").to_string();
            let content = params[1].as_str().unwrap_or("");
            if base64::engine::general_purpose::STANDARD.decode(content).is_err() {
                return ok(json!(0), "append-bad-content");
            }
            let category = params[2].as_str().unwrap_or("").to_string();
            let priority = params[3].as_i64().unwrap_or(0);
            let pp = params[9].as_array().cloned().unwrap_or_default();
            let drone = match (
                pp.first().and_then(Value::as_str),
                pp.get(1).and_then(Value::as_str),
            ) {
                (Some("drone"), Some(id)) => Some(id.to_string()),
                _ => None,
            };
            let number = state.next_seq();
            let title = title.strip_suffix(".nzb").unwrap_or(&title).to_string();
            let mut item = new_item(
                drone.unwrap_or_else(|| number.to_string()),
                number,
                title,
                category,
                ITEM_SIZE,
            );
            item.priority = priority;
            item.paused = params[5].as_bool().unwrap_or(false);
            state.nzbget.push(item);
            ok(json!(number), "append")
        }
        "editqueue" => {
            let command = params.first().and_then(Value::as_str).unwrap_or("").to_string();
            let ids: Vec<i64> = match params.get(3) {
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_i64).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            match command.as_str() {
                "GroupFinalDelete" | "GroupDelete" => state
                    .nzbget
                    .retain(|i| !(ids.contains(&i.number) && i.state == ItemState::Downloading)),
                "HistoryDelete" | "HistoryFinalDelete" => {
                    if command == "HistoryFinalDelete" {
                        for i in state
                            .nzbget
                            .iter()
                            .filter(|i| ids.contains(&i.number) && i.state != ItemState::Downloading)
                        {
                            state.delete_data("nzbget", i);
                        }
                    }
                    state
                        .nzbget
                        .retain(|i| !(ids.contains(&i.number) && i.state != ItemState::Downloading))
                }
                "HistoryRedownload" => {
                    for i in state.nzbget.iter_mut().filter(|i| ids.contains(&i.number)) {
                        i.state = ItemState::Downloading;
                        i.done = 0;
                    }
                }
                _ => {}
            }
            ok(json!(true), &format!("editqueue-{command}"))
        }
        other => json_reply(
            StatusCode::OK,
            json!({"version": "1.1", "result": null, "error": {"name": "JSONRPCError", "code": 1, "message": format!("Method not found: {other}")}, "id": rid}),
            format!("nzbget/{other}"),
        ),
    }
}

// --- qBittorrent (Web API v2)

fn qbittorrent(
    state: &mut ClientState,
    rest: &str,
    q: &BTreeMap<String, String>,
    headers: &HeaderMap,
    body: &[u8],
) -> Reply {
    let (fields, files) = form(headers, body);
    let field = |k: &str| fields.get(k).map(String::as_str).unwrap_or("");
    if rest == "auth/login" {
        if field("username") == MOCK_USER && field("password") == MOCK_PASSWORD {
            let mut reply = text_reply(StatusCode::OK, "Ok.", "qbittorrent/login");
            reply.headers.push((
                "Set-Cookie".into(),
                format!("SID={QBIT_SESSION}; HttpOnly; path=/"),
            ));
            return reply;
        }
        return text_reply(StatusCode::OK, "Fails.", "qbittorrent/login-fails");
    }
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("");
    let has_session = cookie
        .split(';')
        .any(|c| c.trim().strip_prefix("SID=") == Some(QBIT_SESSION));
    if auth != format!("Bearer {API_KEY}") && !has_session {
        return text_reply(StatusCode::FORBIDDEN, "Forbidden", "qbittorrent/403");
    }
    let text = |t: &str, served: &str| text_reply(StatusCode::OK, t, format!("qbittorrent/{served}"));
    let json_ok = |v: Value, served: &str| json_reply(StatusCode::OK, v, format!("qbittorrent/{served}"));
    let downloads = state.downloads.clone();
    let save_path = |t: &Item| {
        if t.category.is_empty() {
            format!("{downloads}/")
        } else {
            format!("{downloads}/{}/", t.category)
        }
    };
    match rest {
        "app/webapiVersion" => text("2.11.4", "webapiVersion"),
        "app/version" => text("v5.1.2", "version"),
        "app/preferences" => json_ok(
            json!({"save_path": format!("{downloads}/"), "max_ratio_enabled": false, "max_ratio": -1, "max_seeding_time_enabled": false, "max_seeding_time": -1, "max_inactive_seeding_time_enabled": false, "max_inactive_seeding_time": -1, "max_ratio_act": 0, "queueing_enabled": true, "dht": true}),
            "preferences",
        ),
        "torrents/categories" => {
            let map: serde_json::Map<String, Value> = state
                .qbittorrent_categories
                .iter()
                .map(|(name, path)| (name.clone(), json!({"name": name, "savePath": path})))
                .collect();
            json_ok(Value::Object(map), "categories")
        }
        "torrents/createCategory" => {
            state
                .qbittorrent_categories
                .entry(field("category").to_string())
                .or_insert_with(|| field("savePath").to_string());
            text("", "createCategory")
        }
        "torrents/info" => {
            let category = q.get("category").map(String::as_str).unwrap_or("");
            let out: Vec<Value> = state
                .qbittorrent
                .iter()
                .filter(|t| category.is_empty() || t.category == category)
                .map(|t| {
                    let progress = t.done as f64 / t.size as f64;
                    let state_name = match t.state {
                        ItemState::Downloading => "downloading",
                        ItemState::Completed => "stoppedUP",
                        ItemState::Failed => "error",
                    };
                    let save = save_path(t);
                    json!({"hash": t.id.to_ascii_lowercase(), "name": t.title, "size": t.size, "progress": progress, "eta": if progress >= 1.0 { 8640000 } else { 600 }, "state": state_name, "category": t.category, "save_path": save, "content_path": format!("{save}{}", t.title), "ratio": if progress < 1.0 { 0.0 } else { 1.0 }, "ratio_limit": -2, "seeding_time_limit": -2, "inactive_seeding_time_limit": -2, "last_activity": 1736478720, "added_on": 1736478720})
                })
                .collect();
            json_ok(Value::Array(out), "info")
        }
        "torrents/properties" => {
            let hash = q.get("hash").map(String::as_str).unwrap_or("");
            match state.qbittorrent.iter().find(|t| t.id.eq_ignore_ascii_case(hash)) {
                Some(t) => json_ok(
                    json!({"hash": t.id.to_ascii_lowercase(), "save_path": save_path(t), "seeding_time": 0}),
                    "properties",
                ),
                None => text_reply(StatusCode::NOT_FOUND, "Not Found", "qbittorrent/properties-404"),
            }
        }
        "torrents/files" => {
            let hash = q.get("hash").map(String::as_str).unwrap_or("");
            match state.qbittorrent.iter().find(|t| t.id.eq_ignore_ascii_case(hash)) {
                Some(t) => json_ok(
                    json!([{"name": format!("{0}/{0}.mkv", t.title), "size": t.size, "progress": t.done as f64 / t.size as f64, "priority": 1}]),
                    "files",
                ),
                None => text_reply(StatusCode::NOT_FOUND, "Not Found", "qbittorrent/files-404"),
            }
        }
        "torrents/add" => {
            let summary = if let Some(url) = fields.get("urls") {
                if url.starts_with("magnet:") {
                    unarr_core::torrent::magnet_info_hash(url).map(|hash| {
                        let name =
                            unarr_core::torrent::magnet_display_name(url).unwrap_or_else(|| hash.clone());
                        (hash, name, ITEM_SIZE)
                    })
                } else {
                    Some((
                        sha1_hex(url.as_bytes()),
                        url.rsplit('/').next().unwrap_or(url).to_string(),
                        ITEM_SIZE,
                    ))
                }
            } else {
                files.get("torrents").and_then(|data| {
                    let hash = unarr_core::torrent::info_hash(data)?;
                    let (name, size) = unarr_core::torrent::summary(data)?;
                    Some((hash, name, size))
                })
            };
            let Some((hash, name, size)) = summary else {
                return text("Fails.", "add-fails");
            };
            if state.qbittorrent.iter().any(|t| t.id == hash) {
                return text("Ok.", "add-duplicate");
            }
            let mut item = new_item(hash, 0, name, field("category").to_string(), size);
            item.paused = matches!(
                fields.get("stopped").or(fields.get("paused")).map(String::as_str),
                Some("true")
            );
            item.added_with = fields.clone();
            state.qbittorrent.push(item);
            text("Ok.", "add")
        }
        "torrents/delete" => {
            let hashes: Vec<String> = field("hashes")
                .split('|')
                .map(|h| h.to_ascii_lowercase())
                .collect();
            if field("deleteFiles") == "true" {
                for t in state
                    .qbittorrent
                    .iter()
                    .filter(|t| hashes.contains(&t.id.to_ascii_lowercase()))
                {
                    state.delete_data("qbittorrent", t);
                }
            }
            state
                .qbittorrent
                .retain(|t| !hashes.contains(&t.id.to_ascii_lowercase()));
            text("", "delete")
        }
        "torrents/setCategory" => {
            let hashes: Vec<String> = field("hashes")
                .split('|')
                .map(|h| h.to_ascii_lowercase())
                .collect();
            let category = field("category").to_string();
            for t in state.qbittorrent.iter_mut() {
                if hashes.contains(&t.id.to_ascii_lowercase()) {
                    t.category = category.clone();
                }
            }
            text("", "setCategory")
        }
        "torrents/setShareLimits"
        | "torrents/topPrio"
        | "torrents/setForceStart"
        | "torrents/pause"
        | "torrents/stop"
        | "torrents/resume"
        | "torrents/start" => {
            let hashes: Vec<String> = field("hashes")
                .split('|')
                .map(|h| h.to_ascii_lowercase())
                .collect();
            for t in state.qbittorrent.iter_mut() {
                if hashes.contains(&t.id.to_ascii_lowercase()) {
                    for (k, v) in &fields {
                        t.added_with.insert(
                            format!("{}:{k}", rest.rsplit('/').next().unwrap_or(rest)),
                            v.clone(),
                        );
                    }
                }
            }
            text("", rest.rsplit('/').next().unwrap_or(rest))
        }
        other => text_reply(
            StatusCode::NOT_FOUND,
            "Not Found",
            format!("qbittorrent/{other}-404"),
        ),
    }
}

fn sha1_hex(data: &[u8]) -> String {
    crate::sha1(data).iter().map(|b| format!("{b:02X}")).collect()
}

// --- Transmission

fn transmission(state: &mut ClientState, _method: &Method, headers: &HeaderMap, body: &[u8]) -> Reply {
    if !basic_auth_ok(headers) {
        let mut reply = text_reply(StatusCode::UNAUTHORIZED, "401: Unauthorized", "transmission/401");
        reply
            .headers
            .push(("WWW-Authenticate".into(), "Basic realm=\"Transmission\"".into()));
        return reply;
    }
    let session = headers
        .get("x-transmission-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if session != TRANSMISSION_SESSION {
        let mut reply = text_reply(StatusCode::CONFLICT, "409: Conflict", "transmission/409");
        reply
            .headers
            .push(("X-Transmission-Session-Id".into(), TRANSMISSION_SESSION.into()));
        return reply;
    }
    let call: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let method = call["method"].as_str().unwrap_or("").to_string();
    let args = call["arguments"].clone();
    let ok = |arguments: Value, served: &str| {
        json_reply(
            StatusCode::OK,
            json!({"result": "success", "arguments": arguments}),
            format!("transmission/{served}"),
        )
    };
    let torrent_json = |t: &Item| json!({"id": t.number, "hashString": t.id.to_ascii_lowercase(), "name": t.title, "downloadDir": t.dir, "totalSize": t.size, "leftUntilDone": t.size - t.done, "isFinished": t.state == ItemState::Completed, "eta": if t.state == ItemState::Downloading { 600 } else { -1 }, "status": match t.state { ItemState::Downloading => 4, ItemState::Completed => 6, ItemState::Failed => 0 }, "secondsDownloading": 60, "secondsSeeding": 0, "errorString": if t.state == ItemState::Failed { "Tracker gave an error: unregistered torrent" } else { "" }, "downloadedEver": t.done, "uploadedEver": 0, "seedRatioLimit": 2.0, "seedRatioMode": 0, "seedIdleLimit": 30, "seedIdleMode": 0, "file-count": 1, "labels": t.labels});
    let selected = |state: &ClientState| -> Vec<Item> {
        match &args["ids"] {
            Value::Null => state.transmission.clone(),
            ids => {
                let wanted: Vec<String> = match ids {
                    Value::Array(a) => a
                        .iter()
                        .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
                        .collect(),
                    v => vec![v.as_str().map(String::from).unwrap_or_else(|| v.to_string())],
                };
                state
                    .transmission
                    .iter()
                    .filter(|t| {
                        wanted
                            .iter()
                            .any(|w| w.eq_ignore_ascii_case(&t.id) || *w == t.number.to_string())
                    })
                    .cloned()
                    .collect()
            }
        }
    };
    match method.as_str() {
        "session-get" => ok(
            json!({"rpc-version": 17, "version": "4.0.6 (38c164933e)", "download-dir": state.dl(""), "seedRatioLimit": 2.0, "seedRatioLimited": false, "idle-seeding-limit": 30, "idle-seeding-limit-enabled": false}),
            "session-get",
        ),
        "torrent-get" => {
            let torrents: Vec<Value> = selected(state).iter().map(torrent_json).collect();
            ok(json!({"torrents": torrents}), "torrent-get")
        }
        "torrent-add" => {
            let summary = if let Some(meta) = args["metainfo"].as_str() {
                base64::engine::general_purpose::STANDARD
                    .decode(meta)
                    .ok()
                    .and_then(|data| {
                        let hash = unarr_core::torrent::info_hash(&data)?;
                        let (name, size) = unarr_core::torrent::summary(&data)?;
                        Some((hash, name, size))
                    })
            } else {
                let filename = args["filename"].as_str().unwrap_or("");
                if filename.starts_with("magnet:") {
                    unarr_core::torrent::magnet_info_hash(filename).map(|hash| {
                        let name = unarr_core::torrent::magnet_display_name(filename)
                            .unwrap_or_else(|| hash.clone());
                        (hash, name, ITEM_SIZE)
                    })
                } else {
                    Some((
                        sha1_hex(filename.as_bytes()),
                        filename.rsplit('/').next().unwrap_or(filename).to_string(),
                        ITEM_SIZE,
                    ))
                }
            };
            let Some((hash, name, size)) = summary else {
                return json_reply(
                    StatusCode::OK,
                    json!({"result": "invalid or corrupt torrent file", "arguments": {}}),
                    "transmission/add-invalid",
                );
            };
            if let Some(t) = state.transmission.iter().find(|t| t.id == hash) {
                return ok(
                    json!({"torrent-duplicate": {"id": t.number, "hashString": hash.to_ascii_lowercase(), "name": t.title}}),
                    "torrent-add-duplicate",
                );
            }
            let number = state.next_seq();
            let mut item = new_item(hash.clone(), number, name, String::new(), size);
            item.dir = args["download-dir"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| state.dl(""));
            item.labels = args["labels"]
                .as_array()
                .map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect())
                .unwrap_or_default();
            item.paused = args["paused"].as_bool().unwrap_or(false);
            if let Value::Object(m) = &args {
                for (k, v) in m {
                    if k != "metainfo" {
                        item.added_with.insert(
                            k.clone(),
                            v.as_str().map(String::from).unwrap_or_else(|| v.to_string()),
                        );
                    }
                }
            }
            let title = item.title.clone();
            state.transmission.push(item);
            ok(
                json!({"torrent-added": {"id": number, "hashString": hash.to_ascii_lowercase(), "name": title}}),
                "torrent-add",
            )
        }
        "torrent-set" => {
            let chosen: Vec<String> = selected(state).iter().map(|t| t.id.clone()).collect();
            for t in state.transmission.iter_mut().filter(|t| chosen.contains(&t.id)) {
                if let Some(labels) = args["labels"].as_array() {
                    t.labels = labels
                        .iter()
                        .filter_map(|l| l.as_str().map(String::from))
                        .collect();
                }
                for key in ["seedRatioLimit", "seedRatioMode", "seedIdleLimit", "seedIdleMode"] {
                    if !args[key].is_null() {
                        t.added_with.insert(format!("set:{key}"), args[key].to_string());
                    }
                }
            }
            ok(json!({}), "torrent-set")
        }
        "torrent-remove" => {
            if args["delete-local-data"].as_bool() == Some(true) {
                for t in selected(state) {
                    state.delete_data("transmission", &t);
                }
            }
            let gone: Vec<String> = selected(state).iter().map(|t| t.id.clone()).collect();
            state.transmission.retain(|t| !gone.contains(&t.id));
            ok(json!({}), "torrent-remove")
        }
        "queue-move-top" | "torrent-start" | "torrent-stop" | "torrent-start-now" => {
            let chosen: Vec<String> = selected(state).iter().map(|t| t.id.clone()).collect();
            for t in state.transmission.iter_mut().filter(|t| chosen.contains(&t.id)) {
                t.added_with.insert(format!("call:{method}"), "1".into());
            }
            ok(json!({}), &method)
        }
        other => json_reply(
            StatusCode::OK,
            json!({"result": format!("method name not recognized: {other}"), "arguments": {}}),
            format!("transmission/{other}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multipart_and_forms() {
        let body = b"--XX\r\nContent-Disposition: form-data; name=\"category\"\r\n\r\ntv\r\n--XX\r\nContent-Disposition: form-data; name=\"torrents\"; filename=\"a.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\nd4:infod4:name1:aee\r\n--XX--\r\n";
        let parts = parse_multipart(body, "multipart/form-data; boundary=XX");
        assert_eq!(parts["category"], (None, b"tv".to_vec()));
        assert_eq!(parts["torrents"].0.as_deref(), Some("a.torrent"));
        assert_eq!(parts["torrents"].1, b"d4:infod4:name1:aee".to_vec());
        let fields = form_urlencoded(b"urls=magnet%3A%3Fxt%3Dx&stopped=false");
        assert_eq!(fields[0], ("urls".to_string(), "magnet:?xt=x".to_string()));
        assert_eq!(hms(3725), "1:02:05");
    }
}

#[cfg(test)]
mod owned_dirs {
    use super::*;

    #[test]
    fn the_default_directories_are_names_and_never_written_to() {
        let state = ClientState::default();
        assert_eq!(state.downloads, "/downloads");
        assert!(!state.inside_mock_dirs("/downloads/complete/tv/Some.Release"));
        assert!(!state.inside_mock_dirs("/tv/Some Series"));
        assert!(!state.inside_mock_dirs("/movies/Some Movie (2018)"));
    }

    #[test]
    fn a_directory_the_mock_was_started_on_is_its_own() {
        let mut state = ClientState {
            downloads: "/tmp/mock-downloads".into(),
            owned: vec!["/tmp/mock-downloads".into()],
            ..ClientState::default()
        };
        assert!(state.inside_mock_dirs("/tmp/mock-downloads/complete/tv/Some.Release"));
        assert!(!state.inside_mock_dirs("/tmp/mock-downloads/../elsewhere"));
        assert!(!state.inside_mock_dirs("/tv/Some Series"));
        state.reset();
        assert!(state.inside_mock_dirs("/tmp/mock-downloads/complete"));
    }
}
