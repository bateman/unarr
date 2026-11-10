//! Test doubles for the indexers and download clients Unarr talks to,
//! answering from `tests/fixtures/indexers/` and from an in-memory queue
//! with the rules `scripts/mock-servarr.py` documents. The Python script
//! drives the real Sonarr while goldens are recorded; this crate drives
//! Unarr in the tests. Same files, same routing, same answers, so a golden
//! recorded against one replays against the other.
//!
//! Every request is kept, so a test can assert what Unarr sent.
//!
//! Exchanges recorded with real download clients replay under `/capture/`
//! (see [`capture`]); the metadata hosts answer under `/<host>/` (see
//! [`metadata`]).

pub mod capture;
pub mod clients;
pub mod metadata;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;

pub const API_KEY: &str = "0123456789abcdef0123456789abcdef";

const ERROR_100: &str =
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"100\" description=\"Incorrect user credentials\"/>\n";
const EMPTY_RSS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" xmlns:newznab=\"http://www.newznab.com/DTD/2010/feeds/attributes/\">\n<channel><title>Fixture</title><newznab:response offset=\"0\" total=\"0\"/></channel></rss>\n";

/// One request the mock received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub user_agent: String,
    /// The fixture file served, or a note.
    pub served: String,
}

#[derive(Clone)]
struct Shared {
    fixtures: PathBuf,
    requests: Arc<Mutex<Vec<Recorded>>>,
    clients: Arc<Mutex<clients::ClientState>>,
    /// `http://127.0.0.1:port` of this mock: the fixtures name the
    /// recording port, and are rewritten to it on the way out.
    base_url: Arc<Mutex<String>>,
}

/// The address the fixture feeds carry (the Python mock's port).
const FIXTURE_URL: &str = "http://127.0.0.1:18989";

/// A running mock. Dropping it stops the server.
pub struct Mock {
    addr: SocketAddr,
    shared: Shared,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `tests/fixtures` of this repository, from any crate.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Where the mock's download clients and Plex say their files are.
#[derive(Debug, Clone, Default)]
pub struct MockDirs {
    /// The directory the download clients report and fill on `complete`;
    /// `/downloads`, the recorder's real one, when `None`.
    pub downloads: Option<PathBuf>,
    /// The TV library location Plex reports; `/tv` when `None`.
    pub tv: Option<PathBuf>,
}

impl Mock {
    /// Serve `fixtures` on `127.0.0.1:port`; port 0 picks a free one. The
    /// goldens name 18989, the port `make golden` runs the Python mock on.
    pub async fn start(fixtures: PathBuf, port: u16) -> anyhow::Result<Mock> {
        Mock::start_with(fixtures, port, MockDirs::default()).await
    }

    /// `start`, with the clients' and Plex's directories pointed at real
    /// ones, so a completed download can be imported from them.
    pub async fn start_with(fixtures: PathBuf, port: u16, dirs: MockDirs) -> anyhow::Result<Mock> {
        let mut state = clients::ClientState::default();
        state.media = fixtures.join("media");
        if let Some(d) = &dirs.downloads {
            state.downloads = d.to_string_lossy().trim_end_matches('/').to_string();
        }
        if let Some(d) = &dirs.tv {
            state.tv = d.to_string_lossy().trim_end_matches('/').to_string();
        }
        let shared = Shared {
            fixtures,
            requests: Arc::new(Mutex::new(Vec::new())),
            clients: Arc::new(Mutex::new(state)),
            base_url: Arc::new(Mutex::new(FIXTURE_URL.to_string())),
        };
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        *shared.base_url.lock().unwrap() = format!("http://{addr}");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Mock { addr, shared, task })
    }

    /// `http://127.0.0.1:port`, without a trailing slash.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.requests.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.shared.requests.lock().unwrap().clear();
    }

    /// Look at (or change) what the mock download clients hold.
    pub fn clients(&self) -> std::sync::MutexGuard<'_, clients::ClientState> {
        self.shared.clients.lock().unwrap()
    }

    /// Send a request to the mock without the network, as a `mock` line of
    /// the golden request list does during the replay: `path` carries the
    /// query. Returns the status and body.
    pub fn call(&self, method: &str, path_and_query: &str, body: &[u8]) -> (StatusCode, Vec<u8>) {
        let method = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);
        let (path, query) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));
        let query: BTreeMap<String, String> = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (clients::percent_decode(k), clients::percent_decode(v))
            })
            .collect();
        let headers = HeaderMap::new();
        let reply = clients::handle(&self.shared.clients, &method, path, &query, &headers, body)
            .unwrap_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    b"not found\n".to_vec(),
                    "text/plain",
                    "404".into(),
                )
                    .into()
            });
        (reply.status, reply.body)
    }
}

pub(crate) fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// The fixture file a query maps to; the Python mock applies the same rules.
pub fn result_file(q: &BTreeMap<String, String>) -> String {
    let get = |k: &str| q.get(k).map(String::as_str).filter(|v| !v.is_empty());
    if get("t") == Some("caps") {
        return "caps.xml".into();
    }
    let mut suffix = String::new();
    if let Some(season) = get("season") {
        suffix.push_str(&format!("-S{season}"));
        if let Some(ep) = get("ep") {
            suffix.push_str(&format!("E{ep}"));
        }
    }
    let mut name = None;
    for key in ["tvdbid", "rid", "tvmazeid", "imdbid"] {
        if let Some(v) = get(key) {
            name = Some(format!("tvsearch-{key}{v}{suffix}"));
            break;
        }
    }
    if name.is_none() {
        if let Some(text) = get("q") {
            name = Some(format!("search-{}{suffix}", slug(text)));
        }
    }
    let mut name = name.unwrap_or_else(|| {
        if get("t") == Some("tvsearch") {
            "rss"
        } else {
            "empty"
        }
        .to_string()
    });
    if let Some(offset) = get("offset").filter(|o| *o != "0") {
        name.push_str(&format!("-offset{offset}"));
    }
    name + ".xml"
}

fn synth_nzb(guid: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE nzb PUBLIC \"-//newzBin//DTD NZB 1.1//EN\" \"http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd\">\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n<file poster=\"fixture@fixture.invalid\" date=\"1736478720\" subject=\"{guid} (1/1)\">\n<groups><group>alt.binaries.fixture</group></groups>\n<segments><segment bytes=\"768000\" number=\"1\">{guid}-1@fixture.invalid</segment></segments>\n</file>\n</nzb>\n"
    )
    .into_bytes()
}

fn bencode_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(format!("{}:", s.len()).as_bytes());
    out.extend_from_slice(s);
}

/// A single-file torrent whose info hash is stable per guid (keys sorted).
fn synth_torrent(guid: &str) -> Vec<u8> {
    // SHA-1 of the guid as the one piece, as the Python mock does.
    let pieces = sha1(guid.as_bytes());
    let mut out = Vec::new();
    out.push(b'd');
    bencode_str(&mut out, b"announce");
    bencode_str(&mut out, b"udp://tracker.invalid:6969/announce");
    bencode_str(&mut out, b"info");
    out.push(b'd');
    bencode_str(&mut out, b"length");
    out.extend_from_slice(b"i768000e");
    bencode_str(&mut out, b"name");
    bencode_str(&mut out, format!("{guid}.mkv").as_bytes());
    bencode_str(&mut out, b"piece length");
    out.extend_from_slice(b"i1048576e");
    bencode_str(&mut out, b"pieces");
    bencode_str(&mut out, &pieces);
    bencode_str(&mut out, b"private");
    out.extend_from_slice(b"i1e");
    out.push(b'e');
    out.push(b'e');
    out
}

/// SHA-1, for the synthetic torrent only. Twenty lines beat a dependency
/// in a test crate.
pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (hi, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *hi = hi.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

async fn handle(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let reply: clients::Reply = 'route: {
        for kind in ["newznab", "torznab"] {
            if path == format!("/{kind}/api") {
                break 'route indexer(&shared, kind, &query).into();
            }
            if let Some(file) = path.strip_prefix(&format!("/{kind}/download/")) {
                break 'route download(&shared, kind, file).into();
            }
        }
        if let Some(reply) = metadata::handle(&shared.fixtures, &path, uri.query().unwrap_or("")) {
            break 'route reply;
        }
        if let Some(reply) = capture::handle(&shared.fixtures, &method, &path, &query, &body) {
            break 'route reply;
        }
        // `/big/<n>`: n bytes, for the tests of the response size ceiling.
        if let Some(n) = path.strip_prefix("/big/").and_then(|n| n.parse::<usize>().ok()) {
            break 'route (StatusCode::OK, vec![b'x'; n], "text/plain", format!("big/{n}")).into();
        }
        if let Some(reply) = clients::handle(&shared.clients, &method, &path, &query, &headers, &body) {
            break 'route reply;
        }
        (
            StatusCode::NOT_FOUND,
            b"not found\n".to_vec(),
            "text/plain",
            "404".into(),
        )
            .into()
    };
    tracing::debug!(%method, %path, ?query, served = reply.served, "mock request");
    shared.requests.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path,
        query,
        user_agent,
        served: reply.served.clone(),
    });
    let mut response = (
        reply.status,
        [(header::CONTENT_TYPE, reply.content_type)],
        reply.body,
    )
        .into_response();
    for (name, value) in reply.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            header::HeaderValue::from_str(&value),
        ) {
            response.headers_mut().append(name, value);
        }
    }
    response
}

fn indexer(
    shared: &Shared,
    kind: &str,
    q: &BTreeMap<String, String>,
) -> (StatusCode, Vec<u8>, &'static str, String) {
    if q.get("apikey").map(String::as_str) != Some(API_KEY) {
        return (
            StatusCode::OK,
            ERROR_100.as_bytes().to_vec(),
            "application/xml",
            "error-100".into(),
        );
    }
    let name = result_file(q);
    let dir = shared.fixtures.join("indexers").join(kind);
    if let Ok(text) = std::fs::read_to_string(dir.join(&name)) {
        // Links in the fixtures point at the recording port; on any other
        // port they point here.
        let base = shared.base_url.lock().unwrap().clone();
        return (
            StatusCode::OK,
            text.replace(FIXTURE_URL, &base).into_bytes(),
            "application/rss+xml",
            format!("{kind}/{name}"),
        );
    }
    let body = std::fs::read(dir.join("empty.xml")).unwrap_or_else(|_| EMPTY_RSS.as_bytes().to_vec());
    (
        StatusCode::OK,
        body,
        "application/rss+xml",
        format!("{kind}/{name} (missing: empty)"),
    )
}

fn download(shared: &Shared, kind: &str, file: &str) -> (StatusCode, Vec<u8>, &'static str, String) {
    let (guid, ext) = file.rsplit_once('.').unwrap_or((file, ""));
    let (sub, content_type) = if ext == "nzb" {
        ("nzb", "application/x-nzb")
    } else {
        ("torrent", "application/x-bittorrent")
    };
    let path = shared.fixtures.join("indexers").join(kind).join(sub).join(file);
    match std::fs::read(&path) {
        Ok(bytes) => (
            StatusCode::OK,
            bytes,
            content_type,
            format!("{kind}/{sub}/{file}"),
        ),
        Err(_) => {
            let body = if sub == "nzb" {
                synth_nzb(guid)
            } else {
                synth_torrent(guid)
            };
            (
                StatusCode::OK,
                body,
                content_type,
                format!("{kind}/{sub}/{file} (synthesised)"),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn routing_matches_the_python_mock() {
        assert_eq!(result_file(&q(&[("t", "caps")])), "caps.xml");
        assert_eq!(
            result_file(&q(&[
                ("t", "tvsearch"),
                ("tvdbid", "448176"),
                ("season", "1"),
                ("ep", "1")
            ])),
            "tvsearch-tvdbid448176-S1E1.xml"
        );
        assert_eq!(
            result_file(&q(&[("t", "tvsearch"), ("tvdbid", "448176"), ("season", "1")])),
            "tvsearch-tvdbid448176-S1.xml"
        );
        assert_eq!(
            result_file(&q(&[("t", "search"), ("q", "The Pitt S01E01")])),
            "search-the-pitt-s01e01.xml"
        );
        assert_eq!(
            result_file(&q(&[
                ("t", "tvsearch"),
                ("q", "The Pitt"),
                ("season", "1"),
                ("ep", "2")
            ])),
            "search-the-pitt-S1E2.xml"
        );
        assert_eq!(result_file(&q(&[("t", "tvsearch"), ("cat", "5030")])), "rss.xml");
        assert_eq!(
            result_file(&q(&[("t", "tvsearch"), ("offset", "100")])),
            "rss-offset100.xml"
        );
        assert_eq!(result_file(&q(&[("t", "search")])), "empty.xml");
    }

    #[test]
    fn sha1_is_the_real_one() {
        let hex: String = sha1(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
        let t = synth_torrent("x");
        assert!(t.starts_with(b"d8:announce"));
        assert!(t.ends_with(b"ee"));
    }
}
