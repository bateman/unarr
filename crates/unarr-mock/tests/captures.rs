//! Unarr's download clients against exchanges recorded from the real
//! programs (`tests/fixtures/downloadclients/<name>/capture.json`, replayed
//! by the mock under `/capture/<name>/`). The captures come from a running
//! Sonarr's trace log: what it asked its clients during normal use and
//! what they answered, anonymised. The first three were taken with empty
//! queues and verify the configuration side: versions, settings,
//! categories, the directories Unarr derives from them. `nzbget-import`
//! was taken while NZBGet 26 downloaded a season and Sonarr imported the
//! finished episodes: one refresh cycle with five items in the queue, one
//! in the history and the deletion that followed its import. The two
//! `*-import` torrent captures were taken the same day: qBittorrent 5 with
//! a finished torrent stopped at its inactive-seeding limit next to a
//! season pack still downloading, and the qBittorrent-compatible program
//! (API 2.0) with five single-file items, one finished; each ends with the
//! removal Sonarr sent after importing.
//!
//! The `radarr-*` captures come from a running Radarr's trace log, the
//! clients being the same three programs asked for the `movies` category:
//! one torrent on qBittorrent 5 from the grab to its removal after the
//! import (`radarr-qbittorrent-downloading` is a refresh at 27 %,
//! `radarr-qbittorrent-import` the grab, the refresh that found it stopped
//! and the removal), the same on the qBittorrent-compatible program, and
//! NZBGet while it downloaded a season for Sonarr and nothing for Radarr.

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
    DownloadClientDefinition::from_provider(unarr_core::downloadclients::SONARR, &provider).unwrap()
}

/// The same client as Radarr configures it: the `movies` category.
fn radarr_definition(kind: ClientKind, port: u16, name: &str, base: &str) -> DownloadClientDefinition {
    let provider = Provider {
        kind: ProviderKind::DownloadClient,
        id: 1,
        name: format!("Captured {}", kind.display_name()),
        implementation: kind.implementation().into(),
        config_contract: format!("{}Settings", kind.implementation()),
        settings: json!({
            "host": "127.0.0.1", "port": port, "urlBase": format!("/capture/{name}{base}"),
            "username": "", "password": "", "movieCategory": "movies",
        }),
        tags: vec![],
        attributes: json!({"enable": true, "priority": 1, "removeCompletedDownloads": true, "removeFailedDownloads": true}),
    };
    DownloadClientDefinition::from_provider(unarr_core::downloadclients::RADARR, &provider).unwrap()
}

/// A magnet release grabbed for a movie.
fn movie_release(title: &str, magnet: &str) -> unarr_core::indexers::rss::ReleaseInfo {
    unarr_core::indexers::rss::ReleaseInfo {
        guid: title.into(),
        title: title.into(),
        download_url: Some(magnet.into()),
        indexer_id: 1,
        indexer: "Captured".into(),
        protocol: unarr_core::model::Protocol::Torrent,
        ..Default::default()
    }
}

fn grab_of(release: &unarr_core::indexers::rss::ReleaseInfo) -> unarr_core::downloadclients::GrabRequest<'_> {
    unarr_core::downloadclients::GrabRequest {
        release,
        recent: false,
        seed: None,
        source: Default::default(),
        noun: "movie",
    }
}

/// What the mock served, by note, in order.
fn served(mock: &Mock) -> Vec<String> {
    mock.requests().into_iter().map(|r| r.served).collect()
}

#[test]
fn the_captures_are_present() {
    let names = capture::names(&fixtures_dir());
    for expected in [
        "nzbget",
        "nzbget-import",
        "qbittorrent",
        "qbittorrent-compat",
        "qbittorrent-compat-import",
        "qbittorrent-import",
        "radarr-nzbget-other-category",
        "radarr-qbittorrent-compat-import",
        "radarr-qbittorrent-downloading",
        "radarr-qbittorrent-import",
    ] {
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
async fn nzbget_with_downloads_in_flight_as_recorded() {
    use unarr_core::downloadclients::DownloadItemStatus;
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let context = ClientContext::new(http(), now());
    let def = definition(ClientKind::Nzbget, mock.port(), "nzbget-import", "");
    let client = Client::new(&def, &context);

    // `items`: the queue's five groups and the history's one, in the
    // queue's order, identified by the drone id Sonarr gave each NZB.
    let items = client.items().await.unwrap();
    let summary: Vec<(String, DownloadItemStatus)> =
        items.iter().map(|i| (i.title.clone(), i.status)).collect();
    assert_eq!(
        summary,
        [
            (
                "Ted.S01E03.1080p.BluRay.x264-BORDURE".to_string(),
                DownloadItemStatus::Downloading
            ),
            (
                "Ted.S01E04.1080p.BluRay.x264-BORDURE".to_string(),
                DownloadItemStatus::Queued
            ),
            (
                "Ted.S01E05.1080p.BluRay.x264-BORDURE".to_string(),
                DownloadItemStatus::Queued
            ),
            (
                "Ted.S01E06.1080p.BluRay.x264-BORDURE".to_string(),
                DownloadItemStatus::Queued
            ),
            (
                "Ted.S01E07.1080p.BluRay.x264-BORDURE".to_string(),
                DownloadItemStatus::Queued
            ),
            (
                "Ted.S01E02.1080p.BluRay.x264-BORDURE".to_string(),
                DownloadItemStatus::Completed
            ),
        ]
    );
    let downloading = &items[0];
    assert_eq!(downloading.download_id, "ca48b4646ff349d284a2c485cd48f277");
    // `FileSizeHi` and `FileSizeLo` make one 64-bit size.
    assert_eq!(downloading.total_size, (1 << 32) + 1033326036);
    assert!(downloading.remaining_size > 0 && downloading.remaining_size < downloading.total_size);
    let completed = &items[5];
    assert_eq!(completed.download_id, "6d6ab4258cc340afa6cbeaa139b67732");
    assert_eq!(
        completed.output_path.as_deref(),
        Some("/downloads/tv/Ted.S01E02.1080p.BluRay.x264-BORDURE")
    );
    assert!(completed.can_be_removed);
    assert!(completed.can_move_files);
    assert_eq!(completed.remaining_size, 0);

    // After the import Sonarr removed the finished item from the history:
    // `editqueue("HistoryDelete", ...)` with the NZB id the drone id maps to.
    client.remove(completed, false).await.unwrap();
    let notes = served(&mock);
    assert!(
        notes.contains(&"capture/nzbget-import/editqueue".to_string()),
        "{notes:?}"
    );
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

#[tokio::test]
async fn qbittorrent_with_downloads_in_flight_as_recorded() {
    use unarr_core::downloadclients::DownloadItemStatus;
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    // The refresh was recorded at 12:50:35 local time (CET): the inactive
    // seeding limit below is measured against this clock.
    let recorded = Utc.with_ymd_and_hms(2026, 11, 9, 11, 50, 35).unwrap();
    let context = ClientContext::new(http(), recorded);
    let def = definition(ClientKind::QBittorrent, mock.port(), "qbittorrent-import", "");
    let client = Client::new(&def, &context);

    // `items`: the two torrents of the tv category as qBittorrent 5 (API
    // 2.16) listed them, in its order.
    let items = client.items().await.unwrap();
    let summary: Vec<(String, DownloadItemStatus)> =
        items.iter().map(|i| (i.title.clone(), i.status)).collect();
    assert_eq!(
        summary,
        [
            (
                "www.UIndex.org    -    Ted.S02E02.1080p.WEB.h264-ETHEL".to_string(),
                DownloadItemStatus::Completed
            ),
            (
                "Below.S01E01-06.1080p.NF.WEB-DL.ITA.ENG.DDP5.1.AV1-G66".to_string(),
                DownloadItemStatus::Downloading
            ),
        ]
    );

    // The finished torrent is `stoppedUP`: qBittorrent stopped it when the
    // global inactive seeding limit (one minute) passed, 88 seconds after
    // its last activity, so Unarr may remove it and move its files. Its
    // output path is the `content_path` the listing carries on this API
    // level; the four spaces in the name are the torrent's own.
    let completed = &items[0];
    assert_eq!(completed.download_id, "E274EBCD3F2B606CDA26D89AE084FB7E5FEA776D");
    assert_eq!(
        completed.output_path.as_deref(),
        Some("/downloads/torrents/tv/www.UIndex.org    -    Ted.S02E02.1080p.WEB.h264-ETHEL")
    );
    assert!(completed.can_be_removed);
    assert!(completed.can_move_files);
    assert_eq!(completed.remaining_size, 0);
    assert_eq!(completed.remaining_time, Some(chrono::Duration::zero()));
    assert_eq!(completed.seed_ratio, Some(0.0));

    // The season pack at 19.6 % with an ETA of 5744 seconds.
    let downloading = &items[1];
    assert_eq!(downloading.total_size, 4647319719);
    assert_eq!(
        downloading.remaining_size,
        (4647319719_f64 * (1.0 - 0.1958043633795448)) as i64
    );
    assert_eq!(downloading.remaining_time, Some(chrono::Duration::seconds(5744)));
    assert!(!downloading.can_be_removed);

    // After the import Sonarr deleted the torrent with its data.
    client.remove(completed, true).await.unwrap();
    let notes = served(&mock);
    assert!(
        notes.contains(&"capture/qbittorrent-import/api/v2/torrents/delete".to_string()),
        "{notes:?}"
    );
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");
}

#[tokio::test]
async fn qbittorrent_compatible_api_with_downloads_in_flight_as_recorded() {
    use unarr_core::downloadclients::DownloadItemStatus;
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let recorded = Utc.with_ymd_and_hms(2026, 11, 9, 13, 10, 10).unwrap();
    let context = ClientContext::new(http(), recorded);
    let def = definition(
        ClientKind::QBittorrent,
        mock.port(),
        "qbittorrent-compat-import",
        "/api/as-qbittorrent",
    );
    let client = Client::new(&def, &context);

    // Five single-file items, every unfinished one reported as
    // `downloading` whatever its progress, the finished one `pausedUP`.
    let items = client.items().await.unwrap();
    assert_eq!(items.len(), 5);
    assert!(items[..4]
        .iter()
        .all(|i| i.status == DownloadItemStatus::Downloading));
    let completed = &items[4];
    assert!(completed
        .title
        .starts_with("Below.S01E06.The.Unfathering.Deep.1080p.NF.WEB-DL"));
    assert_eq!(completed.status, DownloadItemStatus::Completed);
    assert_eq!(completed.download_id, "F6654E5C8E2E7951F31C5D2A907041AACD3AC14D");
    // The program answers API 2.0, before `content_path` (2.6.1), so the
    // listing gives no output path; the item's own ratio limit of 0 is
    // reached at once, which lets it be removed.
    assert_eq!(completed.output_path, None);
    assert!(completed.can_be_removed);
    assert_eq!(completed.remaining_size, 0);

    // Fractional ETAs (110.34 seconds) and a progress at 88.6 %.
    let first = &items[0];
    assert_eq!(first.remaining_time, Some(chrono::Duration::seconds(110)));
    assert_eq!(
        first.remaining_size,
        (1662080803_f64 * (1.0 - 0.8860017920560749)) as i64
    );

    // The import asks `files` and `properties`, as Sonarr did twice over,
    // and resolves the path to the single file under the save path.
    let imported = client.import_item(completed).await.unwrap();
    assert_eq!(
        imported.output_path.as_deref(),
        Some("/downloads/emule/Below.S01E06.The.Unfathering.Deep.1080p.NF.WEB-DL.ENG.Atmos.ITA.DDP5.1.H264-TBK.mkv")
    );
    client.remove(completed, true).await.unwrap();
    let notes = served(&mock);
    for expected in ["torrents/files", "torrents/properties", "torrents/delete"] {
        let note = format!("capture/qbittorrent-compat-import/api/as-qbittorrent/api/v2/{expected}");
        assert!(notes.contains(&note), "{expected} not served: {notes:?}");
    }
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");
}

const HEART: &str = "Heart Of The Beast - Nel Profondo Selvaggio (2026) 1080p WEB-DL H265 Ita Eng AC3 5.1 Sub Ita Eng iDN_CreW.mkv";
const HEART_HASH: &str = "2af8c52acd2c8ff26aef51ea6bb1563f0e1d809d";

#[tokio::test]
async fn radarr_qbittorrent_downloading_as_recorded() {
    use unarr_core::downloadclients::DownloadItemStatus;
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let recorded = Utc.with_ymd_and_hms(2026, 11, 11, 7, 57, 6).unwrap();
    let context = ClientContext::new(http(), recorded);
    let def = radarr_definition(
        ClientKind::QBittorrent,
        mock.port(),
        "radarr-qbittorrent-downloading",
        "",
    );
    let client = Client::new(&def, &context);

    // The listing is asked for the movie category, which is how the
    // capture is keyed: another category would be a miss.
    let items = client.items().await.unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.title, HEART);
    assert_eq!(item.category.as_deref(), Some("movies"));
    assert_eq!(item.status, DownloadItemStatus::Downloading);
    assert_eq!(item.download_id, HEART_HASH.to_ascii_uppercase());
    assert_eq!(item.total_size, 2959421173);
    assert_eq!(
        item.remaining_size,
        (2959421173_f64 * (1.0 - 0.2701286722192414)) as i64
    );
    assert_eq!(item.remaining_time, Some(chrono::Duration::seconds(176)));
    assert!(!item.can_be_removed);
    let notes = served(&mock);
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");
}

#[tokio::test]
async fn radarr_qbittorrent_grab_import_and_removal_as_recorded() {
    use unarr_core::downloadclients::DownloadItemStatus;
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    // The refresh that found the torrent stopped, 08:10:37 UTC.
    let recorded = Utc.with_ymd_and_hms(2026, 11, 11, 8, 10, 37).unwrap();
    let context = ClientContext::new(http(), recorded);
    let def = radarr_definition(
        ClientKind::QBittorrent,
        mock.port(),
        "radarr-qbittorrent-import",
        "",
    );
    let client = Client::new(&def, &context);

    // The grab: Radarr asked the preferences and the API version, then
    // added the magnet; qBittorrent 5.1 (API 2.16.2) answers the add with a
    // JSON summary, not "Ok.", and nothing else was asked afterwards.
    let magnet = format!("magnet:?xt=urn:btih:{HEART_HASH}");
    let release = movie_release(HEART, &magnet);
    let hash = client
        .add_magnet(&grab_of(&release), HEART_HASH, &magnet)
        .await
        .unwrap();
    assert_eq!(hash, HEART_HASH);
    let notes = served(&mock);
    assert!(
        notes.contains(&"capture/radarr-qbittorrent-import/api/v2/torrents/add".to_string()),
        "{notes:?}"
    );
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");

    // Fifteen minutes later: a single-file torrent, `stoppedUP` at the
    // global inactive seeding limit of one minute (ten minutes of seeding
    // allowed, 585 seconds done), so Radarr removed it right after the
    // import. The content path is the file itself.
    let items = client.items().await.unwrap();
    assert_eq!(items.len(), 1);
    let completed = &items[0];
    assert_eq!(completed.title, HEART);
    assert_eq!(completed.status, DownloadItemStatus::Completed);
    assert_eq!(completed.download_id, HEART_HASH.to_ascii_uppercase());
    assert_eq!(
        completed.output_path.as_deref(),
        Some(format!("/downloads/{HEART}").as_str())
    );
    assert!(completed.can_be_removed);
    assert!(completed.can_move_files);
    assert_eq!(completed.remaining_size, 0);

    client.remove(completed, true).await.unwrap();
    let notes = served(&mock);
    assert!(
        notes.contains(&"capture/radarr-qbittorrent-import/api/v2/torrents/delete".to_string()),
        "{notes:?}"
    );
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");
}

#[tokio::test]
async fn radarr_qbittorrent_compatible_api_grab_import_and_removal_as_recorded() {
    use unarr_core::downloadclients::DownloadItemStatus;
    const MUTINY: &str = "Mutiny Inverti la rotta (2026) 1080p x265 ita eng sub eng MD eac3 - Faccio.mkv";
    const MUTINY_HASH: &str = "54461b1ce945e5c20356db081ca0378156f8f0cb";
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let recorded = Utc.with_ymd_and_hms(2026, 11, 10, 21, 7, 32).unwrap();
    let context = ClientContext::new(http(), recorded);
    let def = radarr_definition(
        ClientKind::QBittorrent,
        mock.port(),
        "radarr-qbittorrent-compat-import",
        "/api/as-qbittorrent",
    );
    let client = Client::new(&def, &context);

    // The grab, answered "Ok." by a program on API 2.0.
    let magnet = format!("magnet:?xt=urn:btih:{MUTINY_HASH}&dn=Mutiny");
    let release = movie_release(MUTINY, &magnet);
    client
        .add_magnet(&grab_of(&release), MUTINY_HASH, &magnet)
        .await
        .unwrap();

    // Fifteen minutes later the one item is `pausedUP` with its own ratio
    // limit of 0, reached at once; no output path on this API level.
    let items = client.items().await.unwrap();
    assert_eq!(items.len(), 1);
    let completed = &items[0];
    assert_eq!(completed.title, MUTINY);
    assert_eq!(completed.category.as_deref(), Some("movies"));
    assert_eq!(completed.status, DownloadItemStatus::Completed);
    assert_eq!(completed.download_id, MUTINY_HASH.to_ascii_uppercase());
    assert_eq!(completed.output_path, None);
    assert!(completed.can_be_removed);

    // The import resolves the single file under the save path, from
    // `files` and `properties`, as Radarr did twice over.
    let imported = client.import_item(completed).await.unwrap();
    assert_eq!(
        imported.output_path.as_deref(),
        Some(format!("/downloads/emule/{MUTINY}").as_str())
    );
    client.remove(completed, true).await.unwrap();
    let notes = served(&mock);
    for expected in [
        "torrents/add",
        "torrents/files",
        "torrents/properties",
        "torrents/delete",
    ] {
        let note = format!("capture/radarr-qbittorrent-compat-import/api/as-qbittorrent/api/v2/{expected}");
        assert!(notes.contains(&note), "{expected} not served: {notes:?}");
    }
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");
}

#[tokio::test]
async fn radarr_nzbget_busy_with_another_category_as_recorded() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let recorded = Utc.with_ymd_and_hms(2026, 11, 11, 14, 15, 48).unwrap();
    let context = ClientContext::new(http(), recorded);
    let def = radarr_definition(
        ClientKind::Nzbget,
        mock.port(),
        "radarr-nzbget-other-category",
        "",
    );
    let client = Client::new(&def, &context);

    // NZBGet 26 answers Radarr with the whole queue and history: nineteen
    // groups and one finished item, all of the `tv` category, which is
    // Sonarr's. None is Radarr's to show.
    assert!(client.items().await.unwrap().is_empty());

    // The movie category has a destination of its own in the real config.
    let status = client.status().await.unwrap();
    assert_eq!(status.output_root_folders, ["/downloads/movies"]);
    let notes = served(&mock);
    for expected in ["listgroups", "history", "config"] {
        let note = format!("capture/radarr-nzbget-other-category/{expected}");
        assert!(notes.contains(&note), "{expected} not served: {notes:?}");
    }
    assert!(!notes.iter().any(|n| n.ends_with("/miss")), "{notes:?}");
}
