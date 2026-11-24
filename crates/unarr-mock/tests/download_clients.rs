//! The four download clients against the mock: connection tests with their
//! failures, adding a release and watching it through the queue, remote
//! path mappings, and the service's choice of client with its back-off.

use chrono::{TimeZone, Utc};
use serde_json::json;
use unarr_core::db::Db;
use unarr_core::decision::testing::episode;
use unarr_core::decision::RemoteEpisode;
use unarr_core::downloadclients::{
    download, Client, ClientContext, ClientKind, DownloadClientDefinition, DownloadClientService,
    DownloadItemStatus,
};
use unarr_core::indexers::rss::ReleaseInfo;
use unarr_core::model::{Protocol, Provider, ProviderKind, RemotePathMapping};
use unarr_core::store;
use unarr_mock::clients::{ItemState, MOCK_PASSWORD, MOCK_USER};
use unarr_mock::{fixtures_dir, Mock, API_KEY};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 1, 12, 12, 0, 0).unwrap()
}

fn http() -> unarr_core::http::HttpClient {
    unarr_core::http::build(std::time::Duration::from_secs(10))
}

/// A stored client pointing at the mock, as the API would store it.
fn provider(id: i64, kind: ClientKind, port: u16, fields: serde_json::Value) -> Provider {
    let mut settings = match kind {
        ClientKind::Sabnzbd => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/sabnzbd", "apiKey": API_KEY, "tvCategory": "tv"})
        }
        ClientKind::Nzbget => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/nzbget", "username": MOCK_USER, "password": MOCK_PASSWORD, "tvCategory": "tv"})
        }
        ClientKind::QBittorrent => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/qbittorrent", "username": MOCK_USER, "password": MOCK_PASSWORD, "tvCategory": "tv"})
        }
        ClientKind::Transmission => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/transmission/", "username": MOCK_USER, "password": MOCK_PASSWORD, "tvCategory": "tv"})
        }
    };
    if let (Some(base), Some(extra)) = (settings.as_object_mut(), fields.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    Provider {
        kind: ProviderKind::DownloadClient,
        id,
        name: format!("Mock {}", kind.display_name()),
        implementation: kind.implementation().into(),
        config_contract: format!("{}Settings", kind.implementation()),
        settings,
        tags: vec![],
        attributes: json!({"enable": true, "priority": 1, "removeCompletedDownloads": true, "removeFailedDownloads": true}),
    }
}

fn definition(id: i64, kind: ClientKind, port: u16, fields: serde_json::Value) -> DownloadClientDefinition {
    DownloadClientDefinition::from_provider(
        unarr_core::downloadclients::SONARR,
        &provider(id, kind, port, fields),
    )
    .unwrap()
}

fn remote(mock: &Mock, title: &str, protocol: Protocol, download_url: &str) -> RemoteEpisode {
    let url = if download_url.starts_with("magnet:") {
        download_url.to_string()
    } else {
        format!("{}{}", mock.url(), download_url)
    };
    let mut remote = RemoteEpisode::new(ReleaseInfo {
        guid: title.into(),
        title: title.into(),
        size: 768_000,
        download_url: Some(url),
        indexer_id: 1,
        indexer: "Mock".into(),
        protocol,
        ..Default::default()
    });
    remote.episodes = vec![episode(1, 1, 1, 3)];
    remote
}

#[tokio::test]
async fn every_client_passes_its_connection_test() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let context = ClientContext::new(http(), now());
    for kind in [
        ClientKind::Sabnzbd,
        ClientKind::Nzbget,
        ClientKind::QBittorrent,
        ClientKind::Transmission,
    ] {
        let def = definition(1, kind, port, json!({}));
        let failures = Client::new(&def, &context).test().await;
        assert!(failures.is_empty(), "{kind:?}: {failures:?}");
        let status = Client::new(&def, &context).status().await.unwrap();
        assert!(status.is_localhost);
        assert!(!status.removes_completed_downloads);
        assert_eq!(status.output_root_folders.len(), 1, "{kind:?}: {status:?}");
    }
    // qBittorrent with the API key instead of the login form.
    let def = definition(
        1,
        ClientKind::QBittorrent,
        port,
        json!({"apiKey": API_KEY, "username": "", "password": ""}),
    );
    assert!(Client::new(&def, &context).test().await.is_empty());
}

#[tokio::test]
async fn connection_tests_name_what_is_wrong() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let context = ClientContext::new(http(), now());
    let first = |failures: Vec<unarr_core::validation::ValidationFailure>| -> Vec<(String, String)> {
        failures
            .into_iter()
            .map(|f| (f.property_name, f.error_message))
            .collect()
    };
    // Wrong credentials.
    let sab = definition(1, ClientKind::Sabnzbd, port, json!({"apiKey": "wrong"}));
    let f = first(Client::new(&sab, &context).test().await);
    assert_eq!(f[0], ("Host".into(), "Unable to connect to SABnzbd".into()));
    assert_eq!(f[1], ("APIKey".into(), "API Key Incorrect".into()));
    // The next check reads the config again and aborts the test, as the
    // originals' does; the failures found so far stay.
    assert_eq!(f[2].0, "");
    assert!(f[2].1.starts_with("Test was aborted due to an error: "));
    let nzb = definition(1, ClientKind::Nzbget, port, json!({"password": "wrong"}));
    let f = first(Client::new(&nzb, &context).test().await);
    assert_eq!(f[0], ("Username".into(), "Authentication Failure".into()));
    let qbit = definition(1, ClientKind::QBittorrent, port, json!({"password": "wrong"}));
    let f = first(Client::new(&qbit, &context).test().await);
    assert_eq!(
        f,
        [("Username".to_string(), "Authentication Failure".to_string())]
    );
    let tm = definition(1, ClientKind::Transmission, port, json!({"password": "wrong"}));
    let f = first(Client::new(&tm, &context).test().await);
    assert_eq!(
        f,
        [("Username".to_string(), "Authentication Failure".to_string())]
    );
    // Nothing listening: the socket error in the originals' .NET wording.
    for kind in [ClientKind::Sabnzbd, ClientKind::Nzbget, ClientKind::QBittorrent] {
        let def = definition(1, kind, 1, json!({}));
        let f = first(Client::new(&def, &context).test().await);
        assert_eq!(f[0].0, "Host", "{kind:?}: {f:?}");
        assert_eq!(f[0].1, format!("Unable to connect to {}", kind.display_name()));
    }
    let def = definition(1, ClientKind::Transmission, 1, json!({}));
    let f = first(Client::new(&def, &context).test().await);
    assert_eq!(
        f,
        [(
            "".to_string(),
            "Unknown exception: Connection refused (127.0.0.1:1)".to_string()
        )]
    );
    let def = definition(1, ClientKind::Sabnzbd, 1, json!({}));
    let f = Client::new(&def, &context).test().await;
    assert_eq!(
        f[0].detailed_description.as_deref(),
        Some("Unable to connect to SABnzbd, Connection refused (127.0.0.1:1)")
    );
    // Categories.
    let sab = definition(1, ClientKind::Sabnzbd, port, json!({"tvCategory": "missing"}));
    assert_eq!(
        first(Client::new(&sab, &context).test().await)[0].1,
        "Category does not exist"
    );
    let sab = definition(1, ClientKind::Sabnzbd, port, json!({"tvCategory": "nojob"}));
    assert_eq!(
        first(Client::new(&sab, &context).test().await)[0].1,
        "Enable Job folders"
    );
    let sab = definition(1, ClientKind::Sabnzbd, port, json!({"tvCategory": "sorted"}));
    assert_eq!(
        first(Client::new(&sab, &context).test().await)[0].1,
        "Disable TV Sorting"
    );
    let nzb = definition(1, ClientKind::Nzbget, port, json!({"tvCategory": "missing"}));
    assert_eq!(
        first(Client::new(&nzb, &context).test().await)[0].1,
        "Category does not exist"
    );
    // qBittorrent creates a missing category and warns when none is set.
    let qbit = definition(1, ClientKind::QBittorrent, port, json!({"tvCategory": "fresh"}));
    assert!(Client::new(&qbit, &context).test().await.is_empty());
    assert!(mock.clients().qbittorrent_categories.contains_key("fresh"));
    let qbit = definition(1, ClientKind::QBittorrent, port, json!({"tvCategory": ""}));
    let f = Client::new(&qbit, &context).test().await;
    assert!(f[0].is_warning);
    assert_eq!(f[0].error_message, "Category is recommended");
}

#[tokio::test]
async fn usenet_clients_take_an_nzb_and_report_it_through_the_queue() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let mut context = ClientContext::new(http(), now());
    context.mappings = vec![RemotePathMapping {
        id: 1,
        host: "127.0.0.1".into(),
        remote_path: "/downloads/".into(),
        local_path: "/mnt/downloads/".into(),
    }];
    for (kind, client_name, completed) in [
        (
            ClientKind::Sabnzbd,
            "sabnzbd",
            "/mnt/downloads/complete/tv/The.Pitt.S01E03",
        ),
        (
            ClientKind::Nzbget,
            "nzbget",
            "/mnt/downloads/completed/tv/The.Pitt.S01E03",
        ),
    ] {
        let def = definition(1, kind, port, json!({}));
        let client = Client::new(&def, &context);
        let remote = remote(
            &mock,
            "The.Pitt.S01E03",
            Protocol::Usenet,
            "/newznab/download/The.Pitt.S01E03.nzb",
        );
        let id = download(&client, &remote, None, &[]).await.unwrap();
        let items = client.items().await.unwrap();
        assert_eq!(items.len(), 1, "{kind:?}");
        let item = &items[0];
        assert_eq!(item.download_id, id);
        assert_eq!(item.title, "The.Pitt.S01E03");
        assert_eq!(item.category.as_deref(), Some("tv"));
        assert_eq!(item.status, DownloadItemStatus::Downloading);
        // SABnzbd reports megabytes with two decimals, so its sizes are near.
        assert!(
            (item.total_size - 768_000).abs() < 4_000,
            "{kind:?}: {}",
            item.total_size
        );
        assert!((item.remaining_size - 768_000).abs() < 4_000);
        assert!(item.remaining_time.is_some());
        assert_eq!(item.client.id, 1);
        assert!(mock.clients().advance(client_name, &id, ItemState::Completed));
        let items = client.items().await.unwrap();
        assert_eq!(items[0].status, DownloadItemStatus::Completed, "{kind:?}");
        assert_eq!(items[0].output_path.as_deref(), Some(completed));
        assert_eq!(items[0].remaining_size, 0);
        if kind == ClientKind::Nzbget {
            // Each status fills its own token, the script's included: Radarr 7
            // stopped leaving `{scriptStatus}` in the message.
            let message = items[0].message.as_deref().unwrap_or_default();
            assert!(message.contains("Script Status: NONE"), "{message}");
            assert!(!message.contains('{'), "{message}");
        }
        client.remove(&items[0], false).await.unwrap();
        assert!(client.items().await.unwrap().is_empty());
        // A failed download shows as failed.
        let id = download(&client, &remote, None, &[]).await.unwrap();
        assert!(mock.clients().advance(client_name, &id, ItemState::Failed));
        let items = client.items().await.unwrap();
        assert_eq!(items[0].status, DownloadItemStatus::Failed, "{kind:?}");
        assert!(items[0].message.as_deref().is_some_and(|m| !m.is_empty()));
    }
    // The nzb that never existed.
    let def = definition(1, ClientKind::Sabnzbd, port, json!({}));
    let client = Client::new(&def, &context);
    let mut gone = remote(&mock, "Gone", Protocol::Usenet, "/missing.nzb");
    gone.release.download_url = Some(format!("{}/nothing/here.nzb", mock.url()));
    let err = download(&client, &gone, None, &[]).await.unwrap_err();
    assert!(
        matches!(
            err,
            unarr_core::downloadclients::DownloadError::ReleaseUnavailable(_)
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn torrent_clients_take_files_and_magnets() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let context = ClientContext::new(http(), now());
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Daredevil.S01E01&tr=udp%3A%2F%2Ftracker.invalid%3A6969";
    for (kind, client_name) in [
        (ClientKind::QBittorrent, "qbittorrent"),
        (ClientKind::Transmission, "transmission"),
    ] {
        let def = definition(1, kind, port, json!({}));
        let client = Client::new(&def, &context);
        // A .torrent file: the hash the client reports is the file's.
        let remote_file = remote(
            &mock,
            "The.Pitt.S01E03",
            Protocol::Torrent,
            "/torznab/download/The.Pitt.S01E03.torrent",
        );
        let hash = download(&client, &remote_file, None, &[]).await.unwrap();
        assert_eq!(hash.len(), 40);
        assert_eq!(hash, hash.to_ascii_uppercase());
        // A magnet link.
        let remote_magnet = remote(&mock, "Daredevil.S01E01", Protocol::Torrent, magnet);
        let magnet_hash = download(&client, &remote_magnet, None, &[]).await.unwrap();
        assert_eq!(magnet_hash, "0123456789ABCDEF0123456789ABCDEF01234567");
        let items = client.items().await.unwrap();
        assert_eq!(items.len(), 2, "{kind:?}: {items:?}");
        let file_item = items.iter().find(|i| i.download_id == hash).unwrap();
        assert_eq!(file_item.title, "The.Pitt.S01E03");
        assert_eq!(file_item.status, DownloadItemStatus::Downloading);
        assert_eq!(file_item.category.as_deref(), Some("tv"));
        assert!(
            file_item.output_path.is_none()
                || file_item
                    .output_path
                    .as_deref()
                    .is_some_and(|p| p.contains("The.Pitt.S01E03"))
        );
        assert!(!file_item.can_be_removed);
        assert!(mock.clients().advance(client_name, &hash, ItemState::Completed));
        let items = client.items().await.unwrap();
        let done = items.iter().find(|i| i.download_id == hash).unwrap();
        assert_eq!(done.status, DownloadItemStatus::Completed, "{kind:?}");
        assert_eq!(done.remaining_size, 0);
        assert!(
            done.output_path
                .as_deref()
                .is_some_and(|p| p.ends_with("The.Pitt.S01E03")),
            "{done:?}"
        );
        client.remove(done, true).await.unwrap();
        assert_eq!(client.items().await.unwrap().len(), 1);
        assert!(mock
            .clients()
            .advance(client_name, &magnet_hash, ItemState::Failed));
        let items = client.items().await.unwrap();
        assert_eq!(
            items[0].status,
            DownloadItemStatus::Warning,
            "{kind:?}: {items:?}"
        );
        assert!(items[0].message.is_some());
    }
    // What the clients were asked for.
    let qbit_added = mock.clients().qbittorrent.clone();
    assert!(qbit_added
        .iter()
        .all(|t| t.added_with.get("category").map(String::as_str) == Some("tv")));
    assert!(qbit_added
        .iter()
        .all(|t| t.added_with.get("stopped").map(String::as_str) == Some("false")));
    let tm_added = mock.clients().transmission.clone();
    assert!(tm_added
        .iter()
        .all(|t| t.dir == "/downloads/tv" && t.labels == ["tv"]));
    // A blocklisted hash is refused when the indexer asks for the check.
    let def = definition(1, ClientKind::QBittorrent, port, json!({}));
    let client = Client::new(&def, &context);
    let remote_magnet = remote(&mock, "Daredevil.S01E01", Protocol::Torrent, magnet);
    let mut indexer = unarr_core::indexers::IndexerDefinition::from_provider(&Provider {
        kind: ProviderKind::Indexer,
        id: 1,
        name: "Mock Torznab".into(),
        implementation: "Torznab".into(),
        config_contract: "TorznabSettings".into(),
        settings: json!({"baseUrl": mock.url(), "apiPath": "/api", "apiKey": API_KEY, "rejectBlocklistedTorrentHashesWhileGrabbing": true}),
        tags: vec![],
        attributes: json!({"enableRss": true}),
    })
    .unwrap();
    indexer.settings.reject_blocklisted_torrent_hashes_while_grabbing = true;
    let err = download(
        &client,
        &remote_magnet,
        Some(&indexer),
        &["0123456789abcdef0123456789abcdef01234567".into()],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, unarr_core::downloadclients::DownloadError::ReleaseBlocked(_)),
        "{err:?}"
    );
}

async fn db_with(providers: Vec<Provider>) -> Db {
    let db = Db::open_in_memory().unwrap();
    db.call(move |conn| {
        for p in &providers {
            store::providers::insert_provider(conn, p)?;
        }
        Ok(())
    })
    .await
    .unwrap();
    db
}

#[tokio::test]
async fn the_service_picks_clients_by_protocol_priority_tags_and_health() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let mut tagged = provider(3, ClientKind::QBittorrent, port, json!({}));
    tagged.tags = vec![7];
    let mut low = provider(4, ClientKind::Transmission, port, json!({}));
    low.attributes["priority"] = json!(5);
    let unreachable = provider(5, ClientKind::Nzbget, 1, json!({}));
    let db = db_with(vec![
        provider(1, ClientKind::Sabnzbd, port, json!({})),
        provider(2, ClientKind::Nzbget, port, json!({})),
        tagged,
        low,
        unreachable,
    ])
    .await;
    let service = DownloadClientService::with_client(db, http(), unarr_core::downloadclients::SONARR);
    assert_eq!(service.definitions().await.unwrap().len(), 5);
    // Usenet: round robin between the two healthy ones, the unreachable
    // one (not yet failed) included in the rotation.
    let pick = |p: Protocol, tags: &'static [i64]| {
        let service = &service;
        async move {
            service
                .get_download_client(p, 0, true, tags, now())
                .await
                .unwrap()
                .map(|d| d.map(|d| d.id))
        }
    };
    assert_eq!(pick(Protocol::Usenet, &[]).await, Ok(Some(1)));
    assert_eq!(pick(Protocol::Usenet, &[]).await, Ok(Some(2)));
    assert_eq!(pick(Protocol::Usenet, &[]).await, Ok(Some(5)));
    assert_eq!(pick(Protocol::Usenet, &[]).await, Ok(Some(1)));
    // Torrent: the untagged, lower-priority-number client wins; a series
    // with the tag gets the tagged one.
    assert_eq!(pick(Protocol::Torrent, &[]).await, Ok(Some(4)));
    assert_eq!(pick(Protocol::Torrent, &[7]).await, Ok(Some(3)));
    assert_eq!(pick(Protocol::Unknown, &[]).await, Ok(None));
    // A failed test counts against the client, but as in the originals a
    // client is not backed off within five minutes of its first failure;
    // the next failure after that blocks it and the rotation skips it.
    let unreachable = service.find(5).await.unwrap().unwrap();
    let failures = service.test(&unreachable, now()).await.unwrap();
    assert!(!failures.is_empty());
    assert!(!service.blocked(now()).await.contains(&5));
    let later = now() + chrono::Duration::minutes(6);
    assert!(!service.test(&unreachable, later).await.unwrap().is_empty());
    assert!(service.blocked(later).await.contains(&5));
    assert!(
        service.blocked(now()).await.contains(&5),
        "blocked for a minute after the failure"
    );
    assert_eq!(pick(Protocol::Usenet, &[]).await, Ok(Some(2)));
    assert_eq!(pick(Protocol::Usenet, &[]).await, Ok(Some(1)));
    let healthy = service.download_handling_enabled(true, now()).await.unwrap();
    assert_eq!(healthy.iter().map(|d| d.id).collect::<Vec<_>>(), [1, 2, 3, 4]);
    // A passing test lifts the block one level at a time.
    let good = service.find(1).await.unwrap().unwrap();
    assert!(service.test(&good, now()).await.unwrap().is_empty());
    assert!(!service.blocked(now()).await.contains(&1));
    // An indexer pinned to a client gets it, or an error when it is gone.
    service
        .db()
        .call(|conn| {
            store::providers::insert_provider(
                conn,
                &Provider {
                    kind: ProviderKind::Indexer,
                    id: 9,
                    name: "Pinned".into(),
                    implementation: "Newznab".into(),
                    config_contract: "NewznabSettings".into(),
                    settings: json!({"baseUrl": "http://127.0.0.1:1", "apiPath": "/api"}),
                    tags: vec![],
                    attributes: json!({"enableRss": true, "downloadClientId": 2}),
                },
            )
        })
        .await
        .unwrap();
    let pinned = service
        .get_download_client(Protocol::Usenet, 9, true, &[], now())
        .await
        .unwrap();
    assert_eq!(pinned.map(|d| d.map(|d| d.id)), Ok(Some(2)));
    let wrong = service
        .get_download_client(Protocol::Torrent, 9, true, &[], now())
        .await
        .unwrap();
    assert_eq!(
        wrong.unwrap_err().0,
        "Indexer specified download client does not exist for Pinned"
    );
}

/// Radarr 7 with a Transmission that has labels (the mock reports 4.0.6):
/// the category is a label and no subfolder, a category and a directory
/// together pass the test, and the client's folder is the default one.
/// Sonarr's pin still puts the torrent in the category's subfolder.
#[tokio::test]
async fn radarr_takes_transmissions_category_as_a_label_only() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let context = ClientContext::new(http(), now());
    let radarr = |fields: serde_json::Value| {
        let mut p = provider(1, ClientKind::Transmission, port, fields);
        p.settings.as_object_mut().unwrap().remove("tvCategory");
        DownloadClientDefinition::from_provider(unarr_core::downloadclients::RADARR, &p).unwrap()
    };
    let def = radarr(json!({"movieCategory": "radarr"}));
    let client = Client::new(&def, &context);
    let remote_file = remote(
        &mock,
        "The.Pitt.S01E03",
        Protocol::Torrent,
        "/torznab/download/The.Pitt.S01E03.torrent",
    );
    let hash = download(&client, &remote_file, None, &[]).await.unwrap();
    let added = mock.clients().transmission.clone();
    let downloads = added[0].dir.clone();
    assert!(!downloads.ends_with("/radarr"), "{downloads}");
    assert_eq!(added[0].labels, ["radarr"]);
    let items = client.items().await.unwrap();
    assert!(items.iter().any(|i| i.download_id == hash), "{items:?}");
    let status = client.status().await.unwrap();
    assert_eq!(status.output_root_folders.len(), 1);
    assert!(!status.output_root_folders[0].ends_with("radarr"), "{status:?}");
    // A category with a directory: refused by the settings for Sonarr,
    // accepted by the test for Radarr when the client has labels.
    let both = radarr(json!({"movieCategory": "radarr", "movieDirectory": "/downloads/films"}));
    let failures = Client::new(&both, &context).test().await;
    assert!(
        !failures.iter().any(|f| f.error_message == "Cannot use Category and Directory"),
        "{failures:?}"
    );
}
