//! Unarr's download clients against exchanges recorded from the real
//! programs (`tests/fixtures/downloadclients/<name>/capture.json`, replayed
//! by the mock under `/capture/<name>/`). The captures come from a running
//! Sonarr's trace log: what it asked its clients during normal use and
//! what they answered, anonymised. Queues were empty when they were taken,
//! so what they verify is the configuration side: versions, settings,
//! categories, the directories Unarr derives from them.

use chrono::{TimeZone, Utc};
use serde_json::json;
use unarr_core::downloadclients::{Client, ClientContext, ClientKind, DownloadClientDefinition};
use unarr_core::model::{Provider, ProviderKind};
use unarr_mock::{capture, fixtures_dir, Mock};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 7, 21, 0, 0).unwrap()
}

fn http() -> unarr_core::http::HttpClient {
    unarr_core::http::build(std::time::Duration::from_secs(10))
}

/// A client pointing at the replay of `name`, with `base` as the program's
/// own URL base. No credentials: Sonarr sent none in the recordings (NZBGet
/// is called with basic auth only when a username is set, qBittorrent had
/// authentication bypassed for the local network).
fn definition(kind: ClientKind, port: u16, name: &str, base: &str) -> DownloadClientDefinition {
    let provider = Provider {
        kind: ProviderKind::DownloadClient,
        id: 1,
        name: format!("Captured {}", kind.display_name()),
        implementation: kind.implementation().into(),
        config_contract: format!("{}Settings", kind.implementation()),
        settings: json!({
            "host": "127.0.0.1", "port": port, "urlBase": format!("/capture/{name}{base}"),
            "username": "", "password": "", "tvCategory": "tv",
        }),
        tags: vec![],
        attributes: json!({"enable": true, "priority": 1, "removeCompletedDownloads": true, "removeFailedDownloads": true}),
    };
    DownloadClientDefinition::from_provider(&provider).unwrap()
}

/// What the mock served, by note, in order.
fn served(mock: &Mock) -> Vec<String> {
    mock.requests().into_iter().map(|r| r.served).collect()
}

#[test]
fn the_captures_are_present() {
    let names = capture::names(&fixtures_dir());
    for expected in ["nzbget", "qbittorrent", "qbittorrent-compat"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} missing from {names:?}"
        );
    }
}

#[tokio::test]
async fn nzbget_as_recorded() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = ClientContext::new(http(), now());
    let def = definition(ClientKind::Nzbget, mock.port(), "nzbget", "");
    let client = Client::new(&def, &context);

    // `status`: the tv category's own DestDir from the real `config`.
    let status = client.status().await.unwrap();
    assert_eq!(status.output_root_folders, ["/downloads/tv"]);
    assert!(status.is_localhost);
    assert!(served(&mock).contains(&"capture/nzbget/config".to_string()));

    // `items`: `listgroups` and `history` as NZBGet 24 answers when idle.
    assert!(client.items().await.unwrap().is_empty());
    let notes = served(&mock);
    assert!(
        notes.contains(&"capture/nzbget/listgroups".to_string()),
        "{notes:?}"
    );
    assert!(notes.contains(&"capture/nzbget/history".to_string()), "{notes:?}");

    // The connection test asks `version`, which Sonarr calls only from its
    // own Test button, outside the recording; the category and KeepHistory
    // checks run on the real config and pass.
    let failures: Vec<(String, String)> = client
        .test()
        .await
        .into_iter()
        .map(|f| (f.property_name, f.error_message))
        .collect();
    assert_eq!(
        failures,
        [("Host".to_string(), "Unable to connect to NZBGet".to_string())]
    );
    assert!(served(&mock).contains(&"capture/nzbget/miss".to_string()));
}

#[tokio::test]
async fn qbittorrent_as_recorded() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = ClientContext::new(http(), now());
    let def = definition(ClientKind::QBittorrent, mock.port(), "qbittorrent", "");
    let client = Client::new(&def, &context);

    // Version, preferences, categories and the torrent list as qBittorrent
    // 5 (API 2.16) answers: every check of the connection test passes.
    let failures = client.test().await;
    assert!(failures.is_empty(), "{failures:?}");
    let notes = served(&mock);
    for expected in [
        "app/webapiVersion",
        "app/preferences",
        "torrents/categories",
        "torrents/info",
    ] {
        let note = format!("capture/qbittorrent/api/v2/{expected}");
        assert!(notes.contains(&note), "{expected} not served: {notes:?}");
    }
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");

    // The tv category's relative save path under the global one.
    let status = client.status().await.unwrap();
    assert_eq!(status.output_root_folders, ["/downloads/torrents/tv"]);
    assert!(!status.removes_completed_downloads);
    assert!(client.items().await.unwrap().is_empty());
}

#[tokio::test]
async fn qbittorrent_compatible_api_as_recorded() {
    // A program that speaks qBittorrent's API under a URL base of its own,
    // with a reduced `preferences` and absolute category paths.
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = ClientContext::new(http(), now());
    let def = definition(
        ClientKind::QBittorrent,
        mock.port(),
        "qbittorrent-compat",
        "/api/as-qbittorrent",
    );
    let client = Client::new(&def, &context);
    let failures = client.test().await;
    assert!(failures.is_empty(), "{failures:?}");
    let status = client.status().await.unwrap();
    assert_eq!(status.output_root_folders, ["/downloads/emule"]);
    assert!(!status.removes_completed_downloads);
    assert!(client.items().await.unwrap().is_empty());
    let notes = served(&mock);
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");
}
