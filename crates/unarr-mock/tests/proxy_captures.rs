//! Unarr's SABnzbd and Transmission clients against what the original
//! Sonarr 4.0.20 and Radarr 6.4.4 exchanged with the real programs
//! (SABnzbd 4.2.2, Transmission 4.0.5), recorded through
//! `scripts/downloadclient-capture.py proxy` and anonymised:
//! `tests/fixtures/downloadclients/{sabnzbd,transmission}` from Sonarr,
//! `radarr-sabnzbd` and `radarr-transmission` from Radarr. `radarr-transmission`
//! was recorded again from Radarr 7.0.3.10918 with the same Transmission;
//! there the torrent's data was put in place after the add and verified,
//! since Radarr 7 names no download folder and the torrent lands in
//! Transmission's own.
//!
//! Unlike the captures taken from a trace log, these keep whole requests,
//! so each test does what the original did at one moment of the recording
//! and checks that Unarr sent the same requests (method, path, query,
//! JSON-RPC body or multipart parts, credentials and session headers), in
//! the same order, and read the recorded answers as the original did.
//! `/capture/<name>@<seqs>` replays the exchanges of that moment only; the
//! requests are compared with the exchanges named by their `seq`.
//!
//! What happened during each recording: the client was added (its Test
//! ran twice: once from the test button, once on save); two releases were
//! pushed. For Transmission one torrent's data was already in place, so it
//! completed at once and was imported, the other stayed at 0 %; once the
//! finished one stopped at its seed ratio, the original removed it, and the
//! other was removed from the queue with "remove from client". For SABnzbd
//! the news server was a local stand-in serving one release's articles,
//! so one job completed and was imported, the other failed ("Not on your
//! server(s)"); the original removed both from the history; a third job
//! was pushed with the queue paused and removed from the queue.

use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use serde_json::{json, Value};
use unarr_core::downloadclients::{
    App, Client, ClientContext, ClientKind, DownloadClientDefinition, DownloadItemStatus, GrabRequest,
    RADARR, SONARR,
};
use unarr_core::indexers::rss::ReleaseInfo;
use unarr_core::model::{Protocol, Provider, ProviderKind};
use unarr_mock::{fixtures_dir, Mock};

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

fn http() -> unarr_core::http::HttpClient {
    unarr_core::http::build(std::time::Duration::from_secs(10))
}

fn context() -> ClientContext {
    ClientContext::new(http(), Utc.with_ymd_and_hms(2026, 11, 19, 20, 10, 0).unwrap())
}

/// The client as the original was configured, pointing at the replay of
/// `name` limited to `seqs`. The categories are the originals' defaults.
fn definition(app: App, kind: ClientKind, port: u16, name: &str, seqs: &str) -> DownloadClientDefinition {
    let movies = app == RADARR;
    let mut settings = json!({"host": "127.0.0.1", "port": port, "username": "", "password": ""});
    let prefix = format!("/capture/{name}@{seqs}");
    match kind {
        ClientKind::Transmission => {
            settings["urlBase"] = json!(format!("{prefix}/transmission/"));
            let category = if movies { "radarr" } else { "tv-sonarr" };
            settings[if movies { "movieCategory" } else { "tvCategory" }] = json!(category);
        }
        ClientKind::Sabnzbd => {
            settings["urlBase"] = json!(prefix);
            settings["apiKey"] = json!(API_KEY);
            let category = if movies { "movies" } else { "tv" };
            settings[if movies { "movieCategory" } else { "tvCategory" }] = json!(category);
        }
        _ => unreachable!(),
    }
    let provider = Provider {
        kind: ProviderKind::DownloadClient,
        id: 1,
        name: kind.display_name().into(),
        implementation: kind.implementation().into(),
        config_contract: format!("{}Settings", kind.implementation()),
        settings,
        tags: vec![],
        attributes: json!({"enable": true, "priority": 1, "removeCompletedDownloads": true, "removeFailedDownloads": true}),
    };
    DownloadClientDefinition::from_provider(app, &provider).unwrap()
}

fn release(title: &str, protocol: Protocol) -> ReleaseInfo {
    ReleaseInfo {
        guid: title.into(),
        title: title.into(),
        download_url: Some(format!("http://127.0.0.1:18999/{title}")),
        indexer_id: 1,
        indexer: "Pushed".into(),
        protocol,
        ..Default::default()
    }
}

fn grab<'a>(release: &'a ReleaseInfo, noun: &'static str) -> GrabRequest<'a> {
    GrabRequest {
        release,
        recent: false,
        seed: None,
        source: Default::default(),
        noun,
    }
}

/// One request, as much of it as both sides can be compared on: the
/// anonymiser replaced credentials, uploads and identifiers, consistently.
#[derive(Debug, PartialEq)]
struct Sent {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    body: Body,
    basic_auth: bool,
    session_id: bool,
}

#[derive(Debug, PartialEq)]
enum Body {
    None,
    /// A JSON-RPC call, the uploaded torrent left out (the recording has a
    /// placeholder for it).
    Json(Value),
    /// Multipart parts: name, file name and content type.
    Parts(Vec<(String, String, String)>),
    Text(String),
}

fn body_of(raw: &[u8], content_type: &str) -> Body {
    if raw.is_empty() {
        return Body::None;
    }
    let text = String::from_utf8_lossy(raw).into_owned();
    if let Some(boundary) = content_type.split("boundary=").nth(1) {
        let boundary = boundary.trim_matches('"');
        let parts = text
            .split(&format!("--{boundary}"))
            .filter_map(|part| {
                let head = part.split("\r\n\r\n").next()?;
                let field = |key: &str| {
                    head.split(&format!("{key}=\""))
                        .nth(1)
                        .and_then(|rest| rest.split('"').next())
                        .unwrap_or("")
                        .to_string()
                };
                let kind = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Type: "))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                head.contains("Content-Disposition")
                    .then(|| (field(" name"), field("filename"), kind))
            })
            .collect();
        return Body::Parts(parts);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(mut v) => {
            if let Some(m) = v["arguments"].get_mut("metainfo") {
                *m = json!("<torrent>");
            }
            Body::Json(v)
        }
        Err(_) => Body::Text(text),
    }
}

fn split_query(path: &str) -> (String, BTreeMap<String, String>) {
    let (p, q) = path.split_once('?').unwrap_or((path, ""));
    let query = q
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    (p.to_string(), query)
}

fn decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What the original sent in the exchanges `seqs` of capture `name`.
fn recorded(name: &str, seqs: &[u64]) -> Vec<Sent> {
    let file = fixtures_dir()
        .join("downloadclients")
        .join(name)
        .join("capture.json");
    let exchanges: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    seqs.iter()
        .map(|seq| {
            let x = exchanges
                .iter()
                .find(|x| x["seq"].as_u64() == Some(*seq))
                .unwrap_or_else(|| panic!("no exchange {seq} in {}", file.display()));
            let request = &x["request"];
            let headers: BTreeMap<String, String> = request["headers"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str().unwrap_or("").to_string()))
                .collect();
            let (path, query) = split_query(request["path"].as_str().unwrap());
            let raw = request["body"].as_str().unwrap_or("").as_bytes().to_vec();
            Sent {
                method: request["method"].as_str().unwrap().to_string(),
                path,
                query,
                body: body_of(&raw, headers.get("content-type").map_or("", String::as_str)),
                basic_auth: headers
                    .get("authorization")
                    .is_some_and(|a| a.starts_with("Basic ")),
                session_id: headers.contains_key("x-transmission-session-id"),
            }
        })
        .collect()
}

/// What Unarr sent since the last call, the capture prefix taken off.
fn sent(mock: &Mock) -> Vec<Sent> {
    let out = mock
        .requests()
        .into_iter()
        .map(|r| {
            let rest = r.path.strip_prefix("/capture/").unwrap();
            let path = format!("/{}", rest.split_once('/').map_or("", |(_, p)| p));
            Sent {
                method: r.method,
                path,
                query: r.query,
                body: body_of(&r.body, r.headers.get("content-type").map_or("", String::as_str)),
                basic_auth: r
                    .headers
                    .get("authorization")
                    .is_some_and(|a| a.starts_with("Basic ")),
                session_id: r.headers.contains_key("x-transmission-session-id"),
            }
        })
        .collect();
    mock.clear();
    out
}

#[track_caller]
fn assert_sent_as_recorded(mock: &Mock, name: &str, seqs: &[u64]) {
    let notes: Vec<String> = mock.requests().into_iter().map(|r| r.served).collect();
    assert!(
        !notes.iter().any(|n| n.ends_with("/miss")),
        "a request the capture does not have: {notes:?}"
    );
    assert_eq!(sent(mock), recorded(name, seqs), "{name} {seqs:?}");
}

// --- Transmission

const PITT_E01: &str = "The.Pitt.S01E01.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";
const PITT_E01_HASH: &str = "fc64326ecdef82324922c5bcc51596aa536284a8";
const PITT_E02: &str = "The.Pitt.S01E02.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";
const PITT_E02_HASH: &str = "b37d05878bb8a7e3772e2c3585d90328e4ed7455";

#[tokio::test]
async fn transmission_test_grab_and_refresh_as_sonarr() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = context();
    let def = definition(
        SONARR,
        ClientKind::Transmission,
        mock.port(),
        "transmission",
        "1-3,6-8",
    );
    let client = Client::new(&def, &context);

    // Test: the GET that earns the session id from a 409, `session-get`
    // for the version, `torrent-get`.
    assert!(client.test().await.is_empty());
    assert_sent_as_recorded(&mock, "transmission", &[1, 2, 3]);

    // The queue refresh: `torrent-get` alone, the version being cached.
    assert!(client.items().await.unwrap().is_empty());
    assert_sent_as_recorded(&mock, "transmission", &[6]);

    // The grab of a torrent file: `session-get` for the download
    // directory, then `torrent-add` with the category's folder and label.
    let release = release(PITT_E01, Protocol::Torrent);
    let hash = client
        .add_torrent(
            &grab(&release, "episode"),
            PITT_E01_HASH,
            &format!("{PITT_E01}.torrent"),
            b"d4:infod4:name1:xee".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(hash, PITT_E01_HASH);
    assert_sent_as_recorded(&mock, "transmission", &[7, 8]);

    // The status the health check asks: `session-get`.
    let status = client.status().await.unwrap();
    assert_eq!(status.output_root_folders, ["/downloads/tv-sonarr"]);
    assert_sent_as_recorded(&mock, "transmission", &[25]);
}

#[tokio::test]
async fn transmission_seeding_and_downloading_as_sonarr() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = context();
    let def = definition(
        SONARR,
        ClientKind::Transmission,
        mock.port(),
        "transmission",
        "1-2,11",
    );
    let client = Client::new(&def, &context);
    client.test().await;
    mock.clear();

    // Just after the grabs: the first torrent verified at 100 % and
    // seeding, the second at 0 %. Seeding torrents are not looked at for
    // limits, so no `session-get`.
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, "transmission", &[11]);
    let summary: Vec<(&str, DownloadItemStatus, bool)> = items
        .iter()
        .map(|i| (i.title.as_str(), i.status, i.can_be_removed))
        .collect();
    assert_eq!(
        summary,
        [
            (
                format!("{PITT_E01}.mkv").as_str(),
                DownloadItemStatus::Completed,
                false
            ),
            (
                format!("{PITT_E02}.mkv").as_str(),
                DownloadItemStatus::Downloading,
                false
            ),
        ]
    );
    assert_eq!(items[0].download_id, PITT_E01_HASH.to_ascii_uppercase());
    assert_eq!(
        items[0].output_path.as_deref(),
        Some(format!("/downloads/tv-sonarr/{PITT_E01}.mkv").as_str())
    );
    assert_eq!(items[1].remaining_size, 3_000_000);
}

#[tokio::test]
async fn transmission_stopped_at_its_ratio_and_removals_as_sonarr() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = context();
    let def = definition(
        SONARR,
        ClientKind::Transmission,
        mock.port(),
        "transmission",
        "1,18-20",
    );
    let client = Client::new(&def, &context);
    client.test().await;
    mock.clear();

    // The imported torrent stopped at the global ratio limit: the refresh
    // reads the global limits once, for it, and it can be removed.
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, "transmission", &[18, 19]);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].status, DownloadItemStatus::Completed);
    assert!(items[0].can_be_removed);
    assert!(items[0].can_move_files);
    assert!(!items[1].can_be_removed);

    // Its removal after the import, with the data, then the other one's,
    // from the queue with "remove from client".
    client.remove(&items[0], true).await.unwrap();
    assert_sent_as_recorded(&mock, "transmission", &[20]);
    client.remove(&items[1], true).await.unwrap();
    assert_sent_as_recorded(&mock, "transmission", &[22]);
    assert_eq!(items[1].download_id, PITT_E02_HASH.to_ascii_uppercase());
}

const BACKCOUNTRY: &str = "Backcountry.2015.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";
const BACKCOUNTRY_HASH: &str = "8b1592979f5a1477c1321b9fddcada738bf5c324";

#[tokio::test]
async fn transmission_as_radarr() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = context();
    let name = "radarr-transmission";
    let def = definition(RADARR, ClientKind::Transmission, mock.port(), name, "1-3,5,8");
    let client = Client::new(&def, &context);
    assert!(client.test().await.is_empty());
    assert_sent_as_recorded(&mock, name, &[1, 2, 3]);
    assert!(client.items().await.unwrap().is_empty());
    assert_sent_as_recorded(&mock, name, &[5]);
    let release = release(BACKCOUNTRY, Protocol::Torrent);
    client
        .add_torrent(
            &grab(&release, "movie"),
            BACKCOUNTRY_HASH,
            &format!("{BACKCOUNTRY}.torrent"),
            b"d4:infod4:name1:xee".to_vec(),
        )
        .await
        .unwrap();
    // Radarr 7 (Radarr/Radarr@1b7edb3240): a Transmission with labels
    // takes the category as a label alone, so the add asks no `session-get`
    // for the download folder and names none.
    assert_sent_as_recorded(&mock, name, &[8]);

    // Seeding with no ratio limit of its own (mode 2), the other at 0 %;
    // the second removed from the queue.
    let def = definition(RADARR, ClientKind::Transmission, mock.port(), name, "1-2,11,13");
    let client = Client::new(&def, &context);
    client.test().await;
    mock.clear();
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, name, &[11]);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].status, DownloadItemStatus::Completed);
    assert!(!items[0].can_be_removed);
    assert_eq!(items[0].category.as_deref(), Some("radarr"));
    client.remove(&items[1], true).await.unwrap();
    assert_sent_as_recorded(&mock, name, &[13]);

    // Stopped at the global ratio: one `session-get`, then its removal.
    let def = definition(RADARR, ClientKind::Transmission, mock.port(), name, "1,16-18");
    let client = Client::new(&def, &context);
    client.test().await;
    mock.clear();
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, name, &[16, 17]);
    assert_eq!(items.len(), 1);
    assert!(items[0].can_be_removed);
    client.remove(&items[0], true).await.unwrap();
    assert_sent_as_recorded(&mock, name, &[18]);
}

// --- SABnzbd

const PITT_E03: &str = "The.Pitt.S01E03.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";
const PITT_E04: &str = "The.Pitt.S01E04.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";
const PITT_E05: &str = "The.Pitt.S01E05.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";

#[tokio::test]
async fn sabnzbd_test_grab_and_refresh_as_sonarr() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = context();
    let def = definition(SONARR, ClientKind::Sabnzbd, mock.port(), "sabnzbd", "1-2,10-12");
    let client = Client::new(&def, &context);

    // Test: `version`, then `get_config` for the global settings, again
    // for the category, again for the sorting.
    assert!(client.test().await.is_empty());
    assert_sent_as_recorded(&mock, "sabnzbd", &[1, 2, 3, 4]);

    // The health check's status: `get_config`.
    let status = client.status().await.unwrap();
    assert!(status.is_localhost);
    assert_sent_as_recorded(&mock, "sabnzbd", &[9]);

    // The grab: the NZB uploaded with `addfile`, category and priority in
    // the query.
    let release = release(PITT_E03, Protocol::Usenet);
    let id = client
        .add_nzb(
            &grab(&release, "episode"),
            &format!("{PITT_E03}.nzb"),
            b"<nzb/>".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(id, "SABnzbd_nzo_3ceb1eaa");
    assert_sent_as_recorded(&mock, "sabnzbd", &[10]);

    // The refresh right after: the job already in the history, waiting
    // for post-processing ("Queued" there), which the originals show as
    // downloading.
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, "sabnzbd", &[11, 12]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, PITT_E03);
    assert_eq!(items[0].download_id, "SABnzbd_nzo_3ceb1eaa");
    assert_eq!(items[0].status, DownloadItemStatus::Downloading);
}

#[tokio::test]
async fn sabnzbd_completed_failed_and_removed_as_sonarr() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = context();

    // One job downloading, one completed; the completed one removed after
    // the import: the queue is asked first, then the history entry goes,
    // archived.
    let def = definition(SONARR, ClientKind::Sabnzbd, mock.port(), "sabnzbd", "14-17");
    let client = Client::new(&def, &context);
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, "sabnzbd", &[14, 15]);
    let summary: Vec<(&str, DownloadItemStatus)> =
        items.iter().map(|i| (i.title.as_str(), i.status)).collect();
    assert_eq!(
        summary,
        [
            (PITT_E04, DownloadItemStatus::Downloading),
            (PITT_E03, DownloadItemStatus::Completed)
        ]
    );
    // The output is the job's folder, named like the job, not the file
    // SABnzbd's `storage` names.
    assert_eq!(
        items[1].output_path.as_deref(),
        Some(format!("/downloads/{PITT_E03}").as_str())
    );
    client.remove(&items[1], true).await.unwrap();
    assert_sent_as_recorded(&mock, "sabnzbd", &[16, 17]);

    // The failed job (its message as SABnzbd wrote it, the help link
    // anonymised), removed from the history without archiving.
    let def = definition(SONARR, ClientKind::Sabnzbd, mock.port(), "sabnzbd", "18-21");
    let client = Client::new(&def, &context);
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, "sabnzbd", &[18, 19]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, PITT_E04);
    assert_eq!(items[0].status, DownloadItemStatus::Failed);
    assert_eq!(
        items[0].message.as_deref(),
        Some("Download failed - Not on your server(s) - http://host.invalid/c5a3ecb8ce1f")
    );
    client.remove(&items[0], true).await.unwrap();
    assert_sent_as_recorded(&mock, "sabnzbd", &[20, 21]);

    // A job in the paused queue, removed from it.
    let def = definition(SONARR, ClientKind::Sabnzbd, mock.port(), "sabnzbd", "33-36");
    let client = Client::new(&def, &context);
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, "sabnzbd", &[33, 34]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, PITT_E05);
    assert_eq!(items[0].status, DownloadItemStatus::Paused);
    client.remove(&items[0], true).await.unwrap();
    assert_sent_as_recorded(&mock, "sabnzbd", &[35, 36]);
}

const BRAINDEAD: &str = "Braindead.1992.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";
const CANDYMAN: &str = "Candyman.1992.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";
const CANDY_LAND: &str = "Candy.Land.2023.ITA.ENG.1080p.WEB-DL.DDP5.1.H.264-UNARR";

#[tokio::test]
async fn sabnzbd_as_radarr() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = context();
    let name = "radarr-sabnzbd";
    let def = definition(RADARR, ClientKind::Sabnzbd, mock.port(), name, "1-2,11-15");
    let client = Client::new(&def, &context);
    assert!(client.test().await.is_empty());
    assert_sent_as_recorded(&mock, name, &[1, 2, 3, 4]);
    client.status().await.unwrap();
    assert_sent_as_recorded(&mock, name, &[9]);
    let release = release(BRAINDEAD, Protocol::Usenet);
    client
        .add_nzb(
            &grab(&release, "movie"),
            &format!("{BRAINDEAD}.nzb"),
            b"<nzb/>".to_vec(),
        )
        .await
        .unwrap();
    assert_sent_as_recorded(&mock, name, &[11]);
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, name, &[12, 13]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, BRAINDEAD);
    assert_eq!(items[0].status, DownloadItemStatus::Completed);
    assert_eq!(items[0].category.as_deref(), Some("movies"));
    client.remove(&items[0], true).await.unwrap();
    assert_sent_as_recorded(&mock, name, &[14, 15]);

    let def = definition(RADARR, ClientKind::Sabnzbd, mock.port(), name, "17-20");
    let client = Client::new(&def, &context);
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, name, &[17, 18]);
    assert_eq!(items[0].title, CANDYMAN);
    assert_eq!(items[0].status, DownloadItemStatus::Failed);
    client.remove(&items[0], true).await.unwrap();
    assert_sent_as_recorded(&mock, name, &[19, 20]);

    let def = definition(RADARR, ClientKind::Sabnzbd, mock.port(), name, "34-37");
    let client = Client::new(&def, &context);
    let items = client.items().await.unwrap();
    assert_sent_as_recorded(&mock, name, &[34, 35]);
    assert_eq!(items[0].title, CANDY_LAND);
    assert_eq!(items[0].status, DownloadItemStatus::Paused);
    client.remove(&items[0], true).await.unwrap();
    assert_sent_as_recorded(&mock, name, &[36, 37]);
}
