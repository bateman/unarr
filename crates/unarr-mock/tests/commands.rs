//! Commands and the scheduler on the fixture library and the mock indexers
//! and clients: an RSS sync that holds releases back for a delay profile,
//! a search whose usenet client cannot be reached, the grab of a pending
//! release from the queue, the search a failed download queues again, and
//! the scheduler's tasks.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::json;
use unarr_core::commands::{
    CommandBody, CommandPriority, CommandResult, CommandStatus, CommandTrigger, Runtime,
};
use unarr_core::db::Db;
use unarr_core::downloadclients::{ClientKind, DownloadClientService};
use unarr_core::downloads::{Downloads, PendingReleaseReason, QueueStatus};
use unarr_core::health::{CheckSet, HealthType};
use unarr_core::indexers::settings::NewznabSettings;
use unarr_core::model::{Protocol, Provider, ProviderKind};
use unarr_core::store;
use unarr_mock::clients::{ItemState, MOCK_PASSWORD, MOCK_USER};
use unarr_mock::{fixtures_dir, Mock, API_KEY};

/// A clock the tests move.
#[derive(Clone)]
struct Clock(Arc<Mutex<DateTime<Utc>>>);

impl Clock {
    fn new() -> Clock {
        Clock(Arc::new(Mutex::new(
            Utc.with_ymd_and_hms(2025, 4, 13, 12, 0, 0).unwrap(),
        )))
    }

    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }

    fn source(&self) -> unarr_core::commands::executor::Clock {
        let clock = self.clone();
        Arc::new(move || clock.now())
    }
}

fn indexer(id: i64, implementation: &str, base_url: &str) -> Provider {
    Provider {
        kind: ProviderKind::Indexer,
        id,
        name: format!("Mock {implementation}"),
        implementation: implementation.into(),
        config_contract: format!("{implementation}Settings"),
        settings: NewznabSettings {
            base_url: base_url.into(),
            api_key: API_KEY.into(),
            ..Default::default()
        }
        .to_value(),
        tags: vec![],
        attributes: json!({
            "enableRss": true, "enableAutomaticSearch": true, "enableInteractiveSearch": true, "priority": 25
        }),
    }
}

fn client(id: i64, kind: ClientKind, port: u16, priority: i64) -> Provider {
    let settings = match kind {
        ClientKind::Sabnzbd => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/sabnzbd", "apiKey": API_KEY, "tvCategory": "tv"})
        }
        ClientKind::Nzbget => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/nzbget", "username": MOCK_USER, "password": MOCK_PASSWORD, "tvCategory": "tv"})
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
        attributes: json!({"enable": true, "priority": priority, "removeCompletedDownloads": true, "removeFailedDownloads": true}),
    }
}

/// The fixture with the mock indexers, the given clients, and episode 1 of
/// The Pitt monitored so its releases are wanted.
async fn fixture_db(mock: &Mock, clients: Vec<Provider>) -> Db {
    let dir = fixtures_dir().join("sonarr");
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
    let url = mock.url();
    db.call(move |conn| {
        store::providers::insert_provider(conn, &indexer(20, "Newznab", &format!("{url}/newznab")))?;
        store::providers::insert_provider(conn, &indexer(21, "Torznab", &format!("{url}/torznab")))?;
        for c in &clients {
            store::providers::insert_provider(conn, c)?;
        }
        conn.execute("UPDATE episodes SET monitored = 1 WHERE id = 1", [])?;
        // The fixture's own clients point at hosts that do not exist.
        conn.execute(
            "UPDATE providers SET attributes = json_set(attributes, '$.enable', json('false'))
             WHERE kind = 'downloadClient' AND id IN (1, 2, 3)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    db
}

async fn runtime(db: &Db, clock: &Clock) -> Arc<Runtime> {
    let http = unarr_core::http::build(std::time::Duration::from_secs(10));
    let downloads = Arc::new(Downloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, unarr_core::downloadclients::SONARR),
    ));
    Runtime::new(db.clone(), downloads, clock.source(), Default::default())
        .await
        .unwrap()
}

/// Push a command and run it on the spot, as a worker would.
async fn run(runtime: &Runtime, body: CommandBody) -> unarr_core::commands::CommandModel {
    let model = runtime
        .queue()
        .push(
            body,
            CommandPriority::Normal,
            CommandTrigger::Manual,
            runtime.now(),
        )
        .await
        .unwrap();
    // A refresh queues the processing of what completed, ahead of the
    // next command; run whatever is queued until ours has run.
    loop {
        let started = runtime.queue().try_start(runtime.now()).unwrap();
        let ours = started.id == model.id;
        runtime.execute(started).await;
        if ours {
            break;
        }
    }
    // ...and the processing a refresh of ours queued, so that the queue is
    // left as the caller expects.
    let processing = runtime
        .queue()
        .all()
        .iter()
        .any(|c| c.status == CommandStatus::Queued && c.body == CommandBody::ProcessMonitoredDownloads);
    if processing {
        let started = runtime.queue().try_start(runtime.now()).unwrap();
        runtime.execute(started).await;
    }
    runtime.queue().get(model.id).await.unwrap().unwrap()
}

#[tokio::test]
async fn an_rss_sync_holds_releases_back_for_the_delay_profile_until_grabbed_from_the_queue() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let clock = Clock::new();
    let db = fixture_db(
        &mock,
        vec![
            client(4, ClientKind::Sabnzbd, mock.port(), 1),
            client(6, ClientKind::QBittorrent, mock.port(), 1),
        ],
    )
    .await;
    // A delay longer than the feed's releases are old, on both protocols,
    // the fixture's bypass for the best quality off.
    let delay = 200_000;
    db.call(move |conn| {
        conn.execute(
            "UPDATE delay_profiles SET usenet_delay = ?1, torrent_delay = ?1, bypass_if_highest_quality = 0",
            [delay],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let runtime = runtime(&db, &clock).await;

    let sync = run(&runtime, CommandBody::RssSync).await;
    assert_eq!(sync.status, CommandStatus::Completed);
    assert_eq!(sync.result, CommandResult::Successful);
    assert_eq!(
        sync.message.as_deref(),
        Some("RSS Sync Completed. Reports found: 18, Reports grabbed: 0, Reports pending: 2")
    );
    // The usenet release is pending; the torrent one for the same episode
    // is its fallback.
    let rows = db.call(|conn| store::pending::all(conn)).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].release.protocol, Protocol::Usenet);
    assert_eq!(rows[0].reason, PendingReleaseReason::Delay);
    assert_eq!(rows[1].release.protocol, Protocol::Torrent);
    assert_eq!(rows[1].reason, PendingReleaseReason::Fallback);
    assert!(rows[0].title.starts_with("The.Pitt.S01E01.iTALiAN"));

    // The queue lists the pending one, with the delay's end as its
    // estimate, and nothing tracked.
    let downloads = runtime.downloads();
    assert!(downloads.refresh(clock.now()).await.unwrap() || downloads.queue().is_empty());
    let queue = downloads.pending_queue(clock.now()).await.unwrap();
    assert_eq!(queue.len(), 1, "{queue:?}");
    let item = &queue[0];
    assert_eq!(item.status, QueueStatus::Delay);
    assert_eq!(item.id, unarr_core::downloads::queue::hash_int31("pending-1-ep1"));
    assert_eq!(item.episode.as_ref().map(|e| e.id), Some(1));
    assert_eq!(item.download_id, None);
    assert_eq!(item.download_client, None);
    assert!(item.status_messages.is_none());
    assert_eq!(item.size_left, item.size);
    let ect = item.estimated_completion_time.unwrap();
    assert!(
        ect >= rows[0].release.publish_date + Duration::minutes(delay),
        "{ect}"
    );
    assert_eq!(item.time_left, Some((ect - clock.now()).max(Duration::zero())));

    // Grabbed from the queue: sent to SABnzbd, and both rows gone.
    let pending = downloads
        .find_pending_queue_item(item.id, clock.now())
        .await
        .unwrap()
        .unwrap();
    let grabbed = downloads
        .download_report(pending.remote.as_ref().unwrap(), None, clock.now())
        .await
        .unwrap();
    assert_eq!(grabbed.client.id, 4);
    assert_eq!(grabbed.download_id.as_deref(), Some("SABnzbd_nzo_mock1"));
    assert_eq!(db.call(|conn| store::pending::count(conn)).await.unwrap(), 0);
    assert!(downloads
        .find_pending_queue_item(item.id, clock.now())
        .await
        .unwrap()
        .is_none());

    // The grab asked for the clients to be read again
    // (`DownloadMonitoringService.Handle(EpisodeGrabbedEvent)`). Here the
    // download is gone from the client before they are, as when it is
    // removed there by hand: nothing is tracked, the queue is empty again.
    mock.clients().reset();
    run(&runtime, CommandBody::RefreshMonitoredDownloads).await;
    // Pending again after the next sync (the grabbed episode has no file
    // yet, so the feed's release is still wanted), a further sync sees the
    // release twice, from the feed and from the table, and every decision
    // counts as pending. As in the originals, the copy from the table comes
    // after the feed's and turns the stored row into a fallback, which
    // leaves the queue.
    let resync = run(&runtime, CommandBody::RssSync).await;
    let rows = db.call(|conn| store::pending::all(conn)).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?} after {:?}", resync.message);
    assert_eq!(rows[0].reason, PendingReleaseReason::Delay);
    let again = run(&runtime, CommandBody::RssSync).await;
    assert_eq!(
        again.message.as_deref(),
        Some("RSS Sync Completed. Reports found: 20, Reports grabbed: 0, Reports pending: 4")
    );
    let rows = db.call(|conn| store::pending::all(conn)).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].reason, PendingReleaseReason::Fallback);
    assert!(downloads.pending_queue(clock.now()).await.unwrap().is_empty());
}

#[tokio::test]
async fn shutdown_stops_the_workers_from_taking_commands() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let clock = Clock::new();
    let db = fixture_db(&mock, vec![]).await;
    let runtime = runtime(&db, &clock).await;
    let _handles = runtime.start();
    runtime.shutdown(std::time::Duration::from_secs(2)).await;
    assert!(runtime.is_stopping());
    // A command pushed now stays queued: no worker takes it.
    let model = runtime
        .queue()
        .push(
            CommandBody::CheckHealth,
            CommandPriority::Normal,
            CommandTrigger::Manual,
            runtime.now(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let after = runtime.queue().get(model.id).await.unwrap().unwrap();
    assert_eq!(after.status, CommandStatus::Queued);
}

#[tokio::test]
async fn an_rss_sync_grabs_a_wanted_episode_into_the_queue_and_the_history() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let clock = Clock::new();
    let db = fixture_db(&mock, vec![client(4, ClientKind::Sabnzbd, mock.port(), 1)]).await;
    let runtime = runtime(&db, &clock).await;

    let sync = run(&runtime, CommandBody::RssSync).await;
    assert_eq!(
        sync.message.as_deref(),
        Some("RSS Sync Completed. Reports found: 18, Reports grabbed: 1")
    );
    // The grab asked for a queue refresh; after it the queue holds the item
    // the mock client reports, and the history its Grabbed row.
    let refresh = runtime.queue().try_start(clock.now()).unwrap();
    assert_eq!(refresh.body, CommandBody::RefreshMonitoredDownloads);
    runtime.execute(refresh).await;
    let queue = runtime.downloads().queue();
    assert_eq!(queue.len(), 1, "{queue:?}");
    assert_eq!(queue[0].download_id.as_deref(), Some("SABnzbd_nzo_mock1"));
    assert_eq!(queue[0].episode.as_ref().map(|e| e.id), Some(1));
    assert_eq!(queue[0].status, QueueStatus::Downloading);
    assert!(queue[0].title.starts_with("The.Pitt.S01E01.iTALiAN"));
    let rows = db
        .call(|conn| store::history::find_by_download_id(conn, "SABnzbd_nzo_mock1"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event_type, unarr_core::model::HistoryEvent::Grabbed);
    assert_eq!(rows[0].data["releaseSource"], "Rss");
    assert_eq!(rows[0].data["downloadClientName"], "Mock SABnzbd");
    // The same release is in the feed on the next sync, and is left alone:
    // the episode is in the queue.
    let again = run(&runtime, CommandBody::RssSync).await;
    assert_eq!(
        again.message.as_deref(),
        Some("RSS Sync Completed. Reports found: 18, Reports grabbed: 0")
    );
}

#[tokio::test]
async fn a_release_whose_client_cannot_be_reached_waits_for_another_client() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let clock = Clock::new();
    // NZBGet on a closed port first, the mock SABnzbd behind it.
    let db = fixture_db(
        &mock,
        vec![
            client(12, ClientKind::Nzbget, 1, 1),
            client(4, ClientKind::Sabnzbd, mock.port(), 2),
        ],
    )
    .await;
    let runtime = runtime(&db, &clock).await;

    let search = run(
        &runtime,
        CommandBody::SeasonSearch {
            series_id: 1,
            season_number: 1,
        },
    )
    .await;
    assert_eq!(search.status, CommandStatus::Completed);
    assert_eq!(
        search.message.as_deref(),
        Some("Season search completed. 0 reports downloaded.")
    );
    let rows = db.call(|conn| store::pending::all(conn)).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].reason, PendingReleaseReason::DownloadClientUnavailable);
    assert_eq!(rows[0].release.protocol, Protocol::Usenet);
    // No torrent client at all: the torrent release is a fallback too.
    assert_eq!(rows[1].reason, PendingReleaseReason::Fallback);

    let downloads = runtime.downloads();
    let queue = downloads.pending_queue(clock.now()).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, QueueStatus::DownloadClientUnavailable);

    // Grabbed again from the queue: NZBGet, first by priority, fails again
    // as it is not blocked yet.
    let remote = queue[0].remote.clone().unwrap();
    let retry = downloads.download_report(&remote, None, clock.now()).await;
    assert!(
        matches!(retry, Err(unarr_core::downloads::GrabError::Client(_))),
        "{retry:?}"
    );
    // Once its failures block it (a client is not blocked within five
    // minutes of its first failure), a release pending for want of a client
    // goes to the next client.
    downloads.clients().record_failure(12, clock.now()).await;
    clock.advance(Duration::minutes(6));
    downloads.clients().record_failure(12, clock.now()).await;
    let grabbed = downloads
        .download_report(&remote, None, clock.now())
        .await
        .unwrap();
    assert_eq!(grabbed.client.id, 4);
    assert_eq!(db.call(|conn| store::pending::count(conn)).await.unwrap(), 0);
    let names: Vec<&str> = runtime.queue().all().iter().map(|c| c.name()).collect();
    assert_eq!(
        names,
        ["SeasonSearch", "RefreshMonitoredDownloads"],
        "the search grabbed nothing and asked for no refresh; the release sent afterwards did"
    );
}

#[tokio::test]
async fn a_failed_download_is_searched_for_again() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let clock = Clock::new();
    let db = fixture_db(&mock, vec![client(4, ClientKind::Sabnzbd, mock.port(), 1)]).await;
    let runtime = runtime(&db, &clock).await;
    let blocklisted = |db: &Db| {
        let db = db.clone();
        async move {
            db.call(|conn| store::history::blocklist_for_series(conn, 1))
                .await
                .unwrap()
                .len()
        }
    };
    let queued = |runtime: &Runtime| -> Vec<CommandBody> {
        runtime
            .queue()
            .all()
            .iter()
            .filter(|c| c.status == CommandStatus::Queued)
            .map(|c| c.body.clone())
            .collect()
    };
    // A grab, the refresh it asks for, and the client reporting a failure.
    async fn grab_and_fail(runtime: &Runtime, mock: &Mock, clock: &Clock) {
        let search = run(runtime, CommandBody::EpisodeSearch { episode_ids: vec![1] }).await;
        assert_eq!(
            search.message.as_deref(),
            Some("Episode search completed. 1 reports downloaded.")
        );
        let refresh = runtime.queue().try_start(clock.now()).unwrap();
        assert_eq!(refresh.body, CommandBody::RefreshMonitoredDownloads);
        assert_eq!(refresh.priority, CommandPriority::High);
        runtime.execute(refresh).await;
        let download_id = runtime.downloads().queue()[0].download_id.clone().unwrap();
        assert!(mock.clients().advance("sabnzbd", &download_id, ItemState::Failed));
        let refresh = run(runtime, CommandBody::RefreshMonitoredDownloads).await;
        assert_eq!(refresh.status, CommandStatus::Completed);
        assert!(
            runtime.downloads().queue().is_empty(),
            "the failed download leaves the queue"
        );
    }

    // With automatic redownloads off, a failure is blocklisted and that is all.
    db.call(|conn| store::config::set_config(conn, "AutoRedownloadFailed", "False"))
        .await
        .unwrap();
    let before = blocklisted(&db).await;
    grab_and_fail(&runtime, &mock, &clock).await;
    assert_eq!(blocklisted(&db).await, before + 1);
    assert!(queued(&runtime).is_empty());

    // With them on (and the blocklist cleared so the release is wanted
    // again), the failure queues the search.
    db.call(|conn| store::config::set_config(conn, "AutoRedownloadFailed", "True"))
        .await
        .unwrap();
    let clear = run(&runtime, CommandBody::ClearBlocklist).await;
    assert_eq!(clear.status, CommandStatus::Completed);
    assert_eq!(clear.message, None);
    assert_eq!(blocklisted(&db).await, 0);
    grab_and_fail(&runtime, &mock, &clock).await;
    assert_eq!(blocklisted(&db).await, 1);
    assert_eq!(
        queued(&runtime),
        [CommandBody::EpisodeSearch { episode_ids: vec![1] }]
    );
    let search = runtime.queue().try_start(clock.now()).unwrap();
    assert_eq!(search.trigger, CommandTrigger::Unspecified);
}

#[tokio::test]
async fn the_scheduler_pushes_due_tasks_and_records_their_runs() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let clock = Clock::new();
    let db = fixture_db(&mock, vec![]).await;
    let runtime = runtime(&db, &clock).await;

    // The fixture's tasks were last run in 2026, after this clock: nothing
    // is due until the intervals pass.
    let tasks = db
        .call(|conn| store::providers::list_scheduled_tasks(conn))
        .await
        .unwrap();
    assert_eq!(tasks.len(), 11);
    let rss = tasks
        .iter()
        .find(|t| t.type_name.ends_with("RssSyncCommand"))
        .unwrap();
    assert_eq!(rss.interval_minutes, 60);
    assert_eq!(runtime.tick_scheduler().await.unwrap(), 0);

    // A year on, everything is due; the queue refresh carries the task's
    // priority and last run.
    clock.advance(Duration::days(600));
    assert_eq!(runtime.tick_scheduler().await.unwrap(), 11);
    let all = runtime.queue().all();
    assert_eq!(all.len(), 11);
    let refresh = all
        .iter()
        .find(|c| c.body == CommandBody::RefreshMonitoredDownloads)
        .unwrap();
    assert_eq!(refresh.priority, CommandPriority::High);
    assert_eq!(refresh.trigger, CommandTrigger::Scheduled);
    assert!(refresh.last_execution_time.is_some());
    assert!(!refresh.send_updates_to_client);
    assert!(refresh.body_json()["lastExecutionTime"].is_string());
    // Pushing again while they are queued adds nothing.
    assert_eq!(runtime.tick_scheduler().await.unwrap(), 11);
    assert_eq!(runtime.queue().all().len(), 11);

    // The high priority one starts first; its run moves the task on.
    let started = runtime.queue().try_start(clock.now()).unwrap();
    assert_eq!(started.body, CommandBody::RefreshMonitoredDownloads);
    runtime.execute(started).await;
    let task = db
        .call(|conn| store::providers::list_scheduled_tasks(conn))
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.type_name.ends_with("RefreshMonitoredDownloadsCommand"))
        .unwrap();
    assert_eq!(task.last_execution, Some(clock.now()));
    let next = db
        .call(|conn| {
            unarr_core::commands::scheduler::next_execution(
                conn,
                "NzbDrone.Core.Download.RefreshMonitoredDownloadsCommand",
            )
        })
        .await
        .unwrap();
    assert_eq!(next, Some(clock.now() + Duration::minutes(1)));
    // Everything else runs to completion; the cleanup drops nothing yet.
    while let Some(c) = runtime.queue().try_start(clock.now()) {
        runtime.execute(c).await;
    }
    let all = runtime.queue().all();
    assert!(
        all.iter().all(|c| c.status == CommandStatus::Completed),
        "{all:?}"
    );
    let messaging = all
        .iter()
        .find(|c| c.body == CommandBody::MessagingCleanup)
        .unwrap();
    assert_eq!(messaging.result, CommandResult::Successful);
    assert_eq!(runtime.tick_scheduler().await.unwrap(), 0);
}

#[tokio::test]
async fn health_checks_follow_the_providers_and_the_library() {
    // The library and the client's folder are under a directory that does
    // not exist, whatever this machine has at /tv and /downloads.
    let missing = std::env::temp_dir().join(format!("unarr-missing-{}", std::process::id()));
    let tv = missing.join("tv").to_string_lossy().into_owned();
    let downloads = missing.join("downloads");
    let mock = Mock::start_with(
        fixtures_dir(),
        0,
        unarr_mock::MockDirs {
            downloads: Some(downloads.clone()),
            tv: None,
            movies: None,
        },
    )
    .await
    .unwrap();
    let clock = Clock::new();
    // One client that cannot be reached and one mock.
    let db = fixture_db(
        &mock,
        vec![
            client(12, ClientKind::Nzbget, 1, 1),
            client(4, ClientKind::Sabnzbd, mock.port(), 2),
        ],
    )
    .await;
    let tv_path = tv.clone();
    db.call(move |conn| {
        conn.execute("UPDATE root_folders SET path = ?1", [&tv_path])?;
        conn.execute(
            "UPDATE series SET path = ?1 || substr(path, 4) WHERE path LIKE '/tv/%'",
            [&tv_path],
        )?;
        conn.execute(
            "UPDATE providers SET settings = replace(settings, '\"/tv\"', '\"' || ?1 || '\"')",
            [&tv_path],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let runtime = runtime(&db, &clock).await;
    assert!(runtime.health().is_empty(), "nothing before the first run");

    let check = run(&runtime, CommandBody::CheckHealth).await;
    assert_eq!(check.status, CommandStatus::Completed);
    let health = runtime.health();
    let sources: Vec<&str> = health.iter().map(|h| h.source).collect();
    assert_eq!(
        sources,
        [
            "DownloadClientCheck",
            "IndexerDownloadClientCheck",
            "RemotePathMappingCheck",
            "ImportListRootFolderCheck",
            "RootFolderCheck",
        ],
        "{health:?}"
    );
    let reach = &health[0];
    assert_eq!(reach.kind, HealthType::Error);
    assert_eq!(
        reach.message,
        "Unable to communicate with Mock NZBGet. Unable to connect to NzbGet. Connection refused (127.0.0.1:1)"
    );
    assert_eq!(
        reach.wiki_url,
        "https://wiki.servarr.com/sonarr/system#unable-to-communicate-with-download-client"
    );
    // The fixture's Torznab 14 names one of the clients the test disabled.
    assert_eq!(
        health[1].message,
        "Indexers with invalid download clients: Torznab 14."
    );
    assert_eq!(
        health[2].message,
        format!(
            "Download client Mock SABnzbd places downloads in {}/complete/tv but Sonarr cannot see this directory. You may need to adjust the folder's permissions.",
            downloads.display()
        )
    );
    assert_eq!(health[4].message, format!("Missing root folder: {tv}"));

    // Blocked clients and indexers show up, and clear again.
    clock.advance(Duration::minutes(10));
    runtime
        .downloads()
        .clients()
        .record_failure(12, clock.now())
        .await;
    clock.advance(Duration::minutes(6));
    runtime
        .downloads()
        .clients()
        .record_failure(12, clock.now())
        .await;
    assert!(runtime.check_health(CheckSet::Statuses).await);
    let status = runtime
        .health()
        .into_iter()
        .find(|h| h.source == "DownloadClientStatusCheck")
        .expect("a blocked client is reported");
    assert_eq!(status.kind, HealthType::Warning);
    assert_eq!(
        status.message,
        "Download clients unavailable due to failures: Mock NZBGet"
    );
    clock.advance(Duration::minutes(2));
    assert!(runtime.check_health(CheckSet::Statuses).await);
    assert!(runtime
        .health()
        .iter()
        .all(|h| h.source != "DownloadClientStatusCheck"));

    // Without any indexer the RSS check is an error and the search check a
    // warning.
    db.call(|conn| {
        conn.execute("DELETE FROM providers WHERE kind = 'indexer'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(runtime.check_health(CheckSet::Providers).await);
    let health = runtime.health();
    let rss = health.iter().find(|h| h.source == "IndexerRssCheck").unwrap();
    assert_eq!(rss.kind, HealthType::Error);
    assert_eq!(
        rss.message,
        "No indexers available with RSS sync enabled, Sonarr will not grab new releases automatically"
    );
    let search = health.iter().find(|h| h.source == "IndexerSearchCheck").unwrap();
    assert_eq!(search.kind, HealthType::Warning);

    // A proxy whose host does not resolve is reported; without a proxy
    // there is nothing to say.
    assert!(runtime.health().iter().all(|h| h.source != "ProxyCheck"));
    db.call(|conn| {
        unarr_core::store::config::set_config(conn, "proxyenabled", "True")?;
        unarr_core::store::config::set_config(conn, "proxyhostname", "proxy.does.not.resolve.invalid")
    })
    .await
    .unwrap();
    assert!(runtime.check_health(CheckSet::All).await);
    let health = runtime.health();
    let proxy = health
        .iter()
        .find(|h| h.source == "ProxyCheck")
        .expect("the proxy is checked");
    assert_eq!(proxy.kind, HealthType::Error);
    assert_eq!(
        proxy.message,
        "Failed to resolve the IP Address for the Configured Proxy Host proxy.does.not.resolve.invalid"
    );
    assert_eq!(
        proxy.wiki_url,
        "https://wiki.servarr.com/sonarr/system#proxy-failed-resolve-ip"
    );

    // The checks a save of the settings asks for (`ConfigSavedEvent`) see
    // the proxy switched off again, and leave the other verdicts alone.
    let others = runtime.health().len() - 1;
    db.call(|conn| unarr_core::store::config::set_config(conn, "proxyenabled", "False"))
        .await
        .unwrap();
    assert!(runtime.check_health(CheckSet::ConfigSaved).await);
    let health = runtime.health();
    assert!(health.iter().all(|h| h.source != "ProxyCheck"));
    assert_eq!(health.len(), others);
}
