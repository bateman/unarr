//! Downloads from grab to queue on the fixture library and the mock
//! clients: the history a grab leaves, the queue the clients fill, a
//! completed download waiting for import, a failed one blocklisted and
//! dropped from the client, items ignored or blocklisted from the queue.

use chrono::{TimeZone, Utc};
use serde_json::json;
use unarr_core::db::Db;
use unarr_core::decision::{self, DecisionInput, RemoteEpisode};
use unarr_core::downloadclients::{ClientKind, DownloadClientService};
use unarr_core::downloads::{
    queue, Downloads, QueueStatus, RemoveError, RemoveOptions, TrackedDownloadState,
};
use unarr_core::indexers::rss::{parse_feed, ReleaseInfo};
use unarr_core::indexers::IndexerKind;
use unarr_core::model::{HistoryEvent, Protocol, Provider, ProviderKind};
use unarr_core::store;
use unarr_core::store::download_history::DownloadHistoryEvent;
use unarr_mock::clients::{ItemState, MOCK_PASSWORD, MOCK_USER};
use unarr_mock::{fixtures_dir, Mock, API_KEY};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 4, 13, 12, 0, 0).unwrap()
}

fn client(id: i64, kind: ClientKind, port: u16) -> Provider {
    let settings = match kind {
        ClientKind::Sabnzbd => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/sabnzbd", "apiKey": API_KEY, "tvCategory": "tv"})
        }
        ClientKind::QBittorrent => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/qbittorrent", "username": MOCK_USER, "password": MOCK_PASSWORD, "tvCategory": "tv"})
        }
        _ => unreachable!(),
    };
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

async fn fixture_db(port: u16) -> Db {
    let dir = fixtures_dir().join("sonarr");
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
    db.call(move |conn| {
        store::providers::insert_provider(conn, &client(4, ClientKind::Sabnzbd, port))?;
        store::providers::insert_provider(conn, &client(6, ClientKind::QBittorrent, port))?;
        Ok(())
    })
    .await
    .unwrap();
    db
}

/// The mock's feed, its download links pointed at this mock's port.
fn feed(kind: IndexerKind, mock: &Mock) -> Vec<ReleaseInfo> {
    let dir = match kind {
        IndexerKind::Newznab => "newznab",
        IndexerKind::Torznab => "torznab",
    };
    let xml = std::fs::read_to_string(fixtures_dir().join(format!("indexers/{dir}/rss.xml"))).unwrap();
    let mut releases = parse_feed(&xml, kind, &format!("{}/{dir}/api", mock.url())).unwrap();
    for r in &mut releases {
        r.indexer_id = if kind == IndexerKind::Newznab { 20 } else { 21 };
        r.indexer = format!("Mock {dir}");
        r.download_url = r
            .download_url
            .take()
            .map(|u| u.replace("http://127.0.0.1:18989", &mock.url()));
    }
    releases
}

async fn decided(db: &Db, kind: IndexerKind, mock: &Mock, prefix: &str) -> RemoteEpisode {
    let releases = feed(kind, mock);
    let prefix = prefix.to_string();
    db.call(move |conn| {
        let catalog = decision::Catalog::load(conn, now())?;
        let lookup = decision::StoreLookup::new(conn);
        let decisions = decision::decide(&catalog, &lookup, now(), releases, DecisionInput::Rss);
        Ok(decisions
            .into_iter()
            .find(|d| d.remote.release.title.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no decision for {prefix}"))
            .remote)
    })
    .await
    .unwrap()
}

/// Two readers of a stale queue ask for a refresh at once: the clients are
/// polled once, and the second reader finds the queue fresh.
#[tokio::test]
async fn concurrent_refreshes_poll_the_clients_once() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(mock.port()).await;
    let http = unarr_core::http::build(std::time::Duration::from_secs(10));
    let downloads = Downloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, unarr_core::downloadclients::SONARR),
    );
    assert!(downloads.is_dirty());
    mock.clear();
    let (a, b) = tokio::join!(
        downloads.refresh_if_dirty(now()),
        downloads.refresh_if_dirty(now())
    );
    a.unwrap();
    b.unwrap();
    let polls = mock
        .requests()
        .into_iter()
        .filter(|r| r.served == "sabnzbd/queue")
        .count();
    assert_eq!(polls, 1);
    assert!(!downloads.is_dirty());
    // A grab during a refresh is not lost: the flag is cleared before the
    // clients are asked, so marking it again survives the refresh.
    downloads.mark_dirty();
    mock.clear();
    downloads.refresh_if_dirty(now()).await.unwrap();
    assert_eq!(
        mock.requests()
            .into_iter()
            .filter(|r| r.served == "sabnzbd/queue")
            .count(),
        1
    );
}

#[tokio::test]
async fn grabs_are_tracked_through_the_queue_and_the_history() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(mock.port()).await;
    let http = unarr_core::http::build(std::time::Duration::from_secs(10));
    let downloads = Downloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, unarr_core::downloadclients::SONARR),
    );

    // A usenet release handed to SABnzbd.
    let pitt = decided(&db, IndexerKind::Newznab, &mock, "The.Pitt.S01E01.1080p.AMZN").await;
    let grabbed = downloads.download_report(&pitt, Some(4), now()).await.unwrap();
    assert_eq!(grabbed.download_id.as_deref(), Some("SABnzbd_nzo_mock1"));
    assert_eq!(grabbed.client.id, 4);
    let rows = db
        .call(|conn| store::history::find_by_download_id(conn, "SABnzbd_nzo_mock1"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let grab = &rows[0];
    assert_eq!(grab.event_type, HistoryEvent::Grabbed);
    assert_eq!((grab.series_id, grab.episode_id), (1, 1));
    assert_eq!(grab.data["downloadClient"], "SABnzbd");
    assert_eq!(grab.data["downloadClientName"], "Mock SABnzbd");
    assert_eq!(grab.data["releaseSource"], "Rss");
    assert_eq!(grab.data["seriesMatchType"], "Title");
    assert_eq!(grab.data["protocol"], "1");
    assert_eq!(grab.data["indexer"], "Mock newznab");
    assert_eq!(grab.data["releaseType"], "SingleEpisode");
    assert!(grab.data["age"].is_string() && grab.data["size"].is_string());
    let latest = db
        .call(|conn| store::download_history::latest_for_download_id(conn, "SABnzbd_nzo_mock1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.event_type, DownloadHistoryEvent::DownloadGrabbed);
    assert_eq!((latest.download_client_id, latest.indexer_id), (4, 20));

    // The queue after a refresh.
    assert!(downloads.is_dirty());
    assert!(downloads.refresh(now()).await.unwrap());
    assert!(!downloads.is_dirty());
    let queue = downloads.queue();
    assert_eq!(queue.len(), 1, "{queue:?}");
    let item = &queue[0];
    assert_eq!(
        item.id,
        queue::hash_int31("trackedDownload-4-SABnzbd_nzo_mock1-ep1")
    );
    assert_eq!(item.id, 489905582);
    assert_eq!(item.series.as_ref().map(|s| s.id), Some(1));
    assert_eq!(item.episode.as_ref().map(|e| e.id), Some(1));
    assert_eq!(item.status, QueueStatus::Downloading);
    assert_eq!(
        item.tracked_download_state,
        Some(TrackedDownloadState::Downloading)
    );
    assert_eq!(item.download_client.as_deref(), Some("Mock SABnzbd"));
    assert_eq!(item.indexer.as_deref(), Some("Mock newznab"));
    assert_eq!(item.added, Some(now()));
    assert_eq!(item.protocol, Protocol::Usenet);
    assert!(item.custom_formats.iter().any(|f| f.name == "AMZN"));
    // The decision engine sees the download too.
    let queued = downloads.queued_releases();
    assert_eq!(queued.len(), 1);
    assert_eq!((queued[0].series_id, queued[0].episode_ids.clone()), (1, vec![1]));

    // Completed: waiting for the importer, its path known.
    assert!(mock
        .clients()
        .advance("sabnzbd", "SABnzbd_nzo_mock1", ItemState::Completed));
    downloads.refresh(now()).await.unwrap();
    let item = &downloads.queue()[0];
    assert_eq!(item.status, QueueStatus::Completed);
    assert_eq!(
        item.tracked_download_state,
        Some(TrackedDownloadState::ImportPending)
    );
    assert_eq!(
        item.output_path.as_deref(),
        Some("/downloads/complete/tv/The.Pitt.S01E01.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX")
    );
    assert!(item.status_messages.as_ref().is_some_and(|m| m.is_empty()));

    // A torrent through qBittorrent: the client names it by the torrent,
    // the grab history says which episode it is.
    let daredevil = decided(&db, IndexerKind::Torznab, &mock, "Daredevil.Born.Again").await;
    let grabbed = downloads
        .download_report(&daredevil, Some(6), now())
        .await
        .unwrap();
    let hash = grabbed.download_id.clone().unwrap();
    assert_eq!(hash.len(), 40);
    downloads.refresh(now()).await.unwrap();
    let queue = downloads.queue();
    assert_eq!(queue.len(), 2, "{queue:?}");
    let torrent = queue
        .iter()
        .find(|q| q.download_id.as_deref() == Some(hash.as_str()))
        .unwrap();
    assert_eq!(torrent.series.as_ref().map(|s| s.id), Some(2));
    assert_eq!(torrent.episode.as_ref().map(|e| e.id), Some(46));
    assert_eq!(
        torrent.quality.quality_id, 3,
        "WEBDL-1080p from the grabbed title"
    );
    assert_eq!(torrent.protocol, Protocol::Torrent);
    assert_ne!(
        torrent.title, daredevil.release.title,
        "the client's own name for it"
    );

    // A failed usenet download: history, blocklist, gone from the client.
    let ethel = decided(&db, IndexerKind::Newznab, &mock, "The.Pitt.S01E01.720p").await;
    let failed_id = downloads
        .download_report(&ethel, Some(4), now())
        .await
        .unwrap()
        .download_id
        .unwrap();
    assert!(mock.clients().advance("sabnzbd", &failed_id, ItemState::Failed));
    downloads.refresh(now()).await.unwrap();
    assert_eq!(downloads.queue().len(), 2, "the failed one is not listed");
    assert_eq!(
        downloads.tracked(&failed_id).map(|t| t.state),
        Some(TrackedDownloadState::Failed)
    );
    let failed_rows = db
        .call({
            let id = failed_id.clone();
            move |conn| store::history::find_by_download_id(conn, &id)
        })
        .await
        .unwrap();
    let failed = failed_rows
        .iter()
        .find(|h| h.event_type == HistoryEvent::DownloadFailed)
        .expect("a DownloadFailed row");
    assert_eq!(
        failed.data["message"],
        "Unpacking failed, archive requires a password"
    );
    assert_eq!(failed.data["downloadClient"], "SABnzbd");
    let blocklist = db
        .call(|conn| store::history::blocklist_for_series(conn, 1))
        .await
        .unwrap();
    let blocked = blocklist.last().unwrap();
    assert_eq!(
        blocked.source_title.as_deref(),
        Some(ethel.release.title.as_str())
    );
    assert_eq!(blocked.protocol, Protocol::Usenet);
    assert_eq!(
        blocked.message.as_deref(),
        Some("Unpacking failed, archive requires a password")
    );
    assert!(
        !mock.clients().sabnzbd.iter().any(|i| i.id == failed_id),
        "removed from SABnzbd as removeFailedDownloads asks"
    );

    // Ignored from the queue: a history row, and the client keeps it.
    let torrent_id = torrent.id;
    assert!(downloads
        .remove_queue_item(
            torrent_id,
            RemoveOptions {
                remove_from_client: false,
                ..Default::default()
            },
            now()
        )
        .await
        .unwrap());
    assert_eq!(downloads.queue().len(), 1);
    let ignored = db
        .call({
            let id = hash.clone();
            move |conn| store::history::find_by_download_id(conn, &id)
        })
        .await
        .unwrap();
    assert!(ignored
        .iter()
        .any(|h| h.event_type == HistoryEvent::DownloadIgnored && h.data["message"] == "Manually ignored"));
    assert!(mock.clients().qbittorrent.iter().any(|t| t.id == hash));
    downloads.refresh(now()).await.unwrap();
    assert_eq!(
        downloads.queue().len(),
        1,
        "ignored downloads stay out of the queue"
    );

    // Blocklisted from the queue: dropped from the client too.
    let pitt_id = downloads.queue()[0].id;
    assert!(downloads
        .remove_queue_item(
            pitt_id,
            RemoveOptions {
                remove_from_client: true,
                blocklist: true,
                skip_redownload: true,
                change_category: false,
            },
            now()
        )
        .await
        .unwrap());
    let blocklist = db
        .call(|conn| store::history::blocklist_for_series(conn, 1))
        .await
        .unwrap();
    assert_eq!(
        blocklist.last().and_then(|b| b.message.clone()).as_deref(),
        Some("Manually marked as failed")
    );
    assert!(mock.clients().sabnzbd.is_empty(), "{:?}", mock.clients().sabnzbd);
    assert!(downloads.queue().is_empty());
    assert!(matches!(
        downloads
            .remove_queue_item(123, RemoveOptions::default(), now())
            .await,
        Err(RemoveError::NotFound)
    ));
}

/// `TrackedDownloadService`'s handlers of a refresh that removed episodes,
/// of a deleted series and of an added one: the download is matched again
/// at once, not at the next poll of the clients.
#[tokio::test]
async fn tracked_downloads_follow_the_library() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(mock.port()).await;
    let http = unarr_core::http::build(std::time::Duration::from_secs(10));
    let downloads = Downloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, unarr_core::downloadclients::SONARR),
    );
    let pitt = decided(&db, IndexerKind::Newznab, &mock, "The.Pitt.S01E01.1080p.AMZN").await;
    downloads.download_report(&pitt, Some(4), now()).await.unwrap();
    downloads.refresh(now()).await.unwrap();
    let item = &downloads.queue()[0];
    assert_eq!(item.series.as_ref().map(|s| s.id), Some(1));
    assert_eq!(item.episode.as_ref().map(|e| e.id), Some(1));
    let tvdb_id = item.series.as_ref().unwrap().tvdb_id;

    // Episodes no download is for: nothing to do.
    assert!(!downloads.episodes_removed(&[2, 3], now()).await.unwrap());
    assert!(!downloads.episodes_removed(&[], now()).await.unwrap());

    // The refresh removed the episode: the series is still known, the
    // episode is not.
    db.call(|conn| store::episodes::delete_episodes(conn, &[1]))
        .await
        .unwrap();
    assert!(downloads.episodes_removed(&[1], now()).await.unwrap());
    let queue = downloads.queue();
    assert_eq!(queue.len(), 1, "{queue:?}");
    assert_eq!(queue[0].series.as_ref().map(|s| s.id), Some(1));
    assert_eq!(queue[0].episode, None);
    assert!(downloads
        .queued_releases()
        .iter()
        .all(|q| q.episode_ids.is_empty()));

    // The series is deleted: the download stays in the queue, unmatched.
    db.call(|conn| store::series::delete_cascade(conn, &[1]))
        .await
        .unwrap();
    assert!(!downloads.series_deleted(&[(99, 99)], now()).await.unwrap());
    assert!(downloads.series_deleted(&[(1, tvdb_id)], now()).await.unwrap());
    let queue = downloads.queue();
    assert_eq!(queue.len(), 1, "{queue:?}");
    assert_eq!(queue[0].series, None);

    // Any series added has the unmatched downloads looked at again.
    assert!(downloads.series_added(tvdb_id, now()).await.unwrap());
    assert_eq!(downloads.queue()[0].series, None);
}
