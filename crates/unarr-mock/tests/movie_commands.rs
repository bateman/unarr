//! Radarr's bulk searches on the fixture library and the mock indexers and
//! clients. `RssSync` and `MoviesSearch` are replayed against goldens of the
//! real Radarr (`unarr-api/tests/contract_radarr.rs`); the two searches
//! here would take the original an afternoon at its indexers' pace, so
//! they are tried against the mock alone.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;
use unarr_core::commands::{
    CommandBody, CommandModel, CommandPriority, CommandResult, CommandStatus, CommandTrigger, Runtime,
};
use unarr_core::db::Db;
use unarr_core::downloadclients::{ClientKind, DownloadClientService, RADARR};
use unarr_core::indexers::settings::NewznabSettings;
use unarr_core::indexers::{IndexerClient, IndexerService};
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::movies::commands::MovieSide;
use unarr_core::movies::downloads::MovieDownloads;
use unarr_core::movies::search::MovieSearchService;
use unarr_core::store;
use unarr_core::Personality;
use unarr_mock::clients::{ItemState, MOCK_PASSWORD, MOCK_USER};
use unarr_mock::{fixtures_dir, Mock, API_KEY};

fn now() -> DateTime<Utc> {
    "2026-11-11T17:00:00Z".parse().unwrap()
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

fn client(id: i64, kind: ClientKind, port: u16) -> Provider {
    let settings = match kind {
        ClientKind::Sabnzbd => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/sabnzbd", "apiKey": API_KEY, "movieCategory": "movies"})
        }
        _ => {
            json!({"host": "127.0.0.1", "port": port, "urlBase": "/qbittorrent", "username": MOCK_USER, "password": MOCK_PASSWORD, "movieCategory": "movies"})
        }
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

/// The fixture with the mock indexers and clients in place of its own, and
/// five movies left monitored: The Holdovers (116), Nightmare Alley (70),
/// Last Night in Soho (62) and Lamb (61) without a file, Batman Begins
/// (11) with one.
async fn fixture_db(mock: &Mock) -> Db {
    let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
    unarr_import::radarr::import(&fixtures_dir().join("radarr/radarr.db"), &db).expect("import fixture");
    let (url, port) = (mock.url(), mock.port());
    db.call(move |conn| {
        conn.execute(
            "DELETE FROM providers WHERE kind IN ('indexer', 'downloadClient')",
            [],
        )?;
        store::providers::insert_provider(conn, &indexer(20, "Newznab", &format!("{url}/movies/newznab")))?;
        store::providers::insert_provider(conn, &indexer(21, "Torznab", &format!("{url}/movies/torznab")))?;
        store::providers::insert_provider(conn, &client(4, ClientKind::Sabnzbd, port))?;
        store::providers::insert_provider(conn, &client(6, ClientKind::QBittorrent, port))?;
        conn.execute(
            "UPDATE movies SET monitored = 0 WHERE id NOT IN (116, 70, 62, 61, 11)",
            [],
        )?;
        // The disk of whoever runs the test has no say.
        store::config::set_config(conn, "SkipFreeSpaceCheckWhenImporting", "True")?;
        Ok(())
    })
    .await
    .unwrap();
    db
}

async fn runtime(db: &Db) -> (Arc<Runtime>, Arc<MovieDownloads>) {
    runtime_with(db, None).await
}

/// The runtime, with Radarr's own HTTP client when `own` is one.
async fn runtime_with(db: &Db, own: Option<reqwest::Client>) -> (Arc<Runtime>, Arc<MovieDownloads>) {
    let http = unarr_core::http::build(std::time::Duration::from_secs(10));
    let indexers: &'static IndexerClient = Box::leak(Box::new(IndexerClient::for_personality(
        http.clone(),
        Personality::Radarr,
    )));
    let downloads = Arc::new(MovieDownloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, RADARR),
    ));
    let service = IndexerService::with_client(db.clone(), indexers);
    let side = MovieSide {
        downloads: downloads.clone(),
        search: MovieSearchService::new(db.clone(), service.clone()).with_queue(downloads.queue_source()),
        indexers: service,
        http: own,
    };
    let runtime = Runtime::new_radarr(db.clone(), side, Arc::new(now), Default::default())
        .await
        .unwrap();
    (runtime, downloads)
}

/// Push a command and run what is queued until it has run, as the workers
/// would.
async fn run(runtime: &Runtime, body: CommandBody, trigger: CommandTrigger) -> CommandModel {
    let model = runtime
        .queue()
        .push(body, CommandPriority::Normal, trigger, runtime.now())
        .await
        .unwrap();
    while let Some(started) = runtime.queue().try_start(runtime.now()) {
        runtime.execute(started).await;
    }
    runtime.queue().get(model.id).await.unwrap().unwrap()
}

/// The movies the mock indexers were searched for, by their TMDB ids and
/// the titles of the text searches.
fn searched(mock: &Mock) -> Vec<String> {
    let mut out: Vec<String> = mock
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/api"))
        .filter_map(|r| {
            r.query
                .get("tmdbid")
                .map(|id| format!("tmdb {id}"))
                .or_else(|| r.query.get("q").map(|q| format!("q {q}")))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[tokio::test]
async fn a_missing_movies_search_grabs_what_is_wanted_and_leaves_the_queue_alone() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(&mock).await;
    let (runtime, downloads) = runtime(&db).await;

    // Radarr has no command of that name for a series.
    assert!(CommandBody::parse_for(Personality::Radarr, "MissingEpisodeSearch", &json!({})).is_none());
    let body = CommandBody::parse_for(Personality::Radarr, "MissingMoviesSearch", &json!({})).unwrap();
    let search = run(&runtime, body.clone(), CommandTrigger::Manual).await;
    assert_eq!(search.status, CommandStatus::Completed, "{:?}", search.exception);
    assert_eq!(search.result, CommandResult::Successful);
    // The four monitored movies without a file; the one with a file is
    // not searched. The Holdovers and Nightmare Alley have a release the
    // profile wants.
    assert_eq!(
        search.message.as_deref(),
        Some("Completed search for 4 movies. 2 reports downloaded.")
    );
    let asked = searched(&mock);
    for wanted in ["tmdb 840430", "tmdb 597208"] {
        assert!(asked.iter().any(|a| a == wanted), "{wanted} not in {asked:?}");
    }
    // Batman Begins (TMDB 272) has its file.
    assert!(!asked.iter().any(|a| a == "tmdb 272"), "{asked:?}");

    // The grabs left their history and are in the queue.
    let grabbed: Vec<i64> = db
        .call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT movie_id FROM history WHERE event_type = 'grabbed' AND id > 1008 ORDER BY movie_id",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<Vec<i64>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(grabbed, vec![70, 116]);
    // The grabs asked for a queue refresh themselves, at high priority, as
    // the episodes' grab does (the queue's lazy reload would hide it).
    let refreshes: Vec<CommandModel> = runtime
        .queue()
        .all()
        .into_iter()
        .filter(|c| c.body == CommandBody::RefreshMonitoredDownloads)
        .collect();
    assert_eq!(refreshes.len(), 1, "{refreshes:?}");
    assert_eq!(refreshes[0].priority, CommandPriority::High);
    assert_eq!(refreshes[0].status, CommandStatus::Completed);
    let queued: Vec<i64> = {
        let mut ids: Vec<i64> = downloads
            .queue()
            .iter()
            .filter_map(|q| q.movie.as_ref().map(|m| m.id))
            .collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(
        queued,
        vec![70, 116],
        "the refresh a grab queues read the clients"
    );

    // Again: what is in the queue is not searched for.
    mock.clear();
    let again = run(&runtime, body, CommandTrigger::Manual).await;
    assert_eq!(
        again.message.as_deref(),
        Some("Completed search for 2 movies. 0 reports downloaded.")
    );
    let asked = searched(&mock);
    assert!(!asked.iter().any(|a| a == "tmdb 840430"), "{asked:?}");
}

#[tokio::test]
async fn a_cutoff_unmet_search_takes_the_movies_whose_file_is_below_the_cutoff() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(&mock).await;
    // Batman Begins has a WEBDL-1080p file, the best its profile allows
    // and above the cutoff (HDTV-1080p); an HDTV-720p one is below.
    let set_file_quality = |quality: i64| {
        let db = db.clone();
        async move {
            db.call(move |conn| {
                conn.execute("UPDATE movie_files SET quality_id = ?1 WHERE id = 391", [quality])?;
                Ok(())
            })
            .await
            .unwrap()
        }
    };
    let (runtime, _) = runtime(&db).await;
    let body = CommandBody::parse_for(Personality::Radarr, "CutoffUnmetMoviesSearch", &json!({})).unwrap();

    set_file_quality(3).await;
    let met = run(&runtime, body.clone(), CommandTrigger::Manual).await;
    assert_eq!(met.status, CommandStatus::Completed, "{:?}", met.exception);
    assert_eq!(
        met.message.as_deref(),
        Some("Completed search for 0 movies. 0 reports downloaded.")
    );
    assert!(searched(&mock).is_empty());

    set_file_quality(4).await;
    let unmet = run(&runtime, body, CommandTrigger::Manual).await;
    // The movie is searched, and the ones without a file are not.
    assert!(
        unmet
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("Completed search for 1 movies.")),
        "{:?}",
        unmet.message
    );
    let asked = searched(&mock);
    assert!(asked.iter().any(|a| a == "tmdb 272"), "{asked:?}");
    assert!(!asked.iter().any(|a| a == "tmdb 840430"), "{asked:?}");
}

/// How many rows of a table a movie has.
async fn count(db: &Db, sql: &'static str, movie_id: i64) -> i64 {
    db.call(move |conn| Ok(conn.query_row(sql, [movie_id], |r| r.get(0))?))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_failed_download_is_recorded_blocklisted_removed_and_searched_again() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(&mock).await;
    let (runtime, downloads) = runtime(&db).await;
    let search = run(
        &runtime,
        CommandBody::MoviesSearch { movie_ids: vec![116] },
        CommandTrigger::Manual,
    )
    .await;
    assert_eq!(
        search.message.as_deref(),
        Some("Completed search for 1 movies. 1 reports downloaded.")
    );
    let first = downloads
        .queue()
        .into_iter()
        .find(|q| q.movie.as_ref().is_some_and(|m| m.id == 116))
        .expect("the grab is in the queue");
    let download_id = first.download_id.clone().unwrap();
    let first_title = first.title.clone();

    // The client says it failed; the next refresh sees it.
    assert!(mock.clients().advance("sabnzbd", &download_id, ItemState::Failed));
    mock.clear();
    let refresh = run(
        &runtime,
        CommandBody::RefreshMonitoredDownloads,
        CommandTrigger::Scheduled,
    )
    .await;
    assert_eq!(
        refresh.status,
        CommandStatus::Completed,
        "{:?}",
        refresh.exception
    );

    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM history WHERE movie_id = ?1 AND event_type = 'downloadFailed'",
            116
        )
        .await,
        1
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM blocklist WHERE movie_id = ?1", 116).await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM download_history WHERE movie_id = ?1 AND event_type = '3'",
            116
        )
        .await,
        1
    );
    // `RedownloadFailedDownloadService`: the movie was searched again, by
    // a command nobody posted, and the release that failed was not taken
    // a second time.
    let again: Vec<_> = runtime
        .queue()
        .all()
        .into_iter()
        .filter(|c| c.body == CommandBody::MoviesSearch { movie_ids: vec![116] })
        .collect();
    assert_eq!(again.len(), 2, "the search posted and the one the failure queued");
    assert!(again.iter().any(|c| c.trigger == CommandTrigger::Unspecified));
    assert!(searched(&mock).iter().any(|a| a == "tmdb 840430"));
    let now_queued: Vec<_> = downloads
        .queue()
        .into_iter()
        .filter(|q| q.movie.as_ref().is_some_and(|m| m.id == 116))
        .collect();
    assert!(
        now_queued
            .iter()
            .all(|q| q.download_id.as_deref() != Some(download_id.as_str())),
        "the failed item left the client"
    );
    assert!(
        now_queued.iter().all(|q| q.title != first_title),
        "the blocklisted release was not grabbed again"
    );
}

#[tokio::test]
async fn redownloading_follows_the_setting_and_the_request() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(&mock).await;
    db.call(|conn| store::config::set_config(conn, "AutoRedownloadFailed", "False"))
        .await
        .unwrap();
    let (runtime, downloads) = runtime(&db).await;
    run(
        &runtime,
        CommandBody::MoviesSearch { movie_ids: vec![116] },
        CommandTrigger::Manual,
    )
    .await;
    let item = downloads
        .queue()
        .into_iter()
        .find(|q| q.movie.as_ref().is_some_and(|m| m.id == 116))
        .expect("the grab is in the queue");
    // Removed from the queue with blocklist: a failure, and with the
    // setting off no search follows.
    let failed = downloads
        .remove_queue_item(
            item.id,
            unarr_core::downloads::RemoveOptions {
                remove_from_client: true,
                blocklist: true,
                skip_redownload: false,
                change_category: false,
            },
            runtime.now(),
        )
        .await
        .ok()
        .flatten()
        .expect("a failure event");
    assert_eq!(failed.movie_id, 116);
    let searches = |runtime: &Runtime| {
        runtime
            .queue()
            .all()
            .into_iter()
            .filter(|c| matches!(c.body, CommandBody::MoviesSearch { .. }))
            .count()
    };
    assert_eq!(searches(&runtime), 1, "only the search that was posted");
    assert_eq!(
        count(&db, "SELECT count(*) FROM blocklist WHERE movie_id = ?1", 116).await,
        1
    );
}

/// A grab whose history cannot be written is still a grab: the client has
/// the release, and the next release of the movie must not follow it.
#[tokio::test]
async fn a_grab_counts_even_when_its_history_cannot_be_written() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(&mock).await;
    let (runtime, _) = runtime(&db).await;
    // The database refuses every history row from here on.
    db.call(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER no_history BEFORE INSERT ON history
             BEGIN SELECT RAISE(ABORT, 'the disk is full'); END;",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    mock.clear();
    // The Holdovers has several releases the profile wants, on both
    // indexers.
    let search = run(
        &runtime,
        CommandBody::MoviesSearch { movie_ids: vec![116] },
        CommandTrigger::Manual,
    )
    .await;
    assert_eq!(search.status, CommandStatus::Completed, "{:?}", search.exception);
    assert_eq!(
        search.message.as_deref(),
        Some("Completed search for 1 movies. 1 reports downloaded.")
    );
    let fetched: Vec<String> = mock
        .requests()
        .iter()
        .filter(|r| r.path.contains("/download/"))
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(fetched.len(), 1, "one release went to a client: {fetched:?}");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM history WHERE movie_id = ?1 AND event_type = 'grabbed' AND id > 1008",
            116
        )
        .await,
        0,
        "and its history is what was lost"
    );
}

/// Radarr's proxy and certificate settings are its own, and its covers
/// and metadata follow them from the start: nobody has to redirect the
/// metadata hosts first for the runtime to take the application's client.
#[tokio::test]
async fn the_covers_go_out_through_radarrs_own_client_from_the_start() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use unarr_core::outbound::{Handle, Outbound, Proxy, ProxyType};

    // Radarr's settings name a proxy, here a listener of the test's.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let settings = Handle::default();
    settings.set(Outbound {
        proxy: Some(Proxy::new(
            ProxyType::Http,
            "127.0.0.1",
            i64::from(port),
            "",
            "",
            "",
            false,
        )),
        certificate_validation: Default::default(),
    });
    let own = unarr_core::http::build_with(
        std::time::Duration::from_secs(10),
        &settings,
        unarr_core::Personality::Radarr,
    );
    let asked = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 2048];
        let read = socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8_lossy(&request[..read]).to_string()
    });

    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(&mock).await;
    // No `set_upstreams`: the runtime as `unarrd` starts it.
    let (runtime, _) = runtime_with(&db, Some(own)).await;
    let answer = runtime
        .augment_client()
        .client()
        .get("http://covers.invalid/poster.jpg")
        .send()
        .await
        .expect("the request goes to the proxy of Radarr's settings");
    assert_eq!(answer.status().as_u16(), 204);
    let request = asked.await.unwrap();
    assert!(
        request.starts_with("GET http://covers.invalid/poster.jpg "),
        "{request}"
    );
}

/// Radarr's `ProxyCheck` tests the proxy of Radarr's settings: its ping
/// goes out through Radarr's own client, as the original's goes through
/// its own `IHttpClient`. The process-wide settings, Sonarr's, name no
/// proxy here, as in a process that runs Radarr alone; asked through
/// them, the ping would reach nobody.
#[tokio::test]
async fn radarrs_proxy_check_asks_through_radarrs_own_proxy() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use unarr_core::health::{CheckSet, HealthType};
    use unarr_core::outbound::{Handle, Outbound, Proxy, ProxyType};

    // Radarr's settings name a proxy, here a listener of the test's that
    // answers the first ping and refuses the second.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let settings = Handle::default();
    settings.set(Outbound {
        proxy: Some(Proxy::new(
            ProxyType::Http,
            "127.0.0.1",
            i64::from(port),
            "",
            "",
            "",
            false,
        )),
        certificate_validation: Default::default(),
    });
    let own = unarr_core::http::build_with(std::time::Duration::from_secs(10), &settings);
    let (asked, mut seen) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for status in ["200 OK", "400 Bad Request"] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 2048];
            let read = socket.read(&mut request).await.unwrap();
            let line = String::from_utf8_lossy(&request[..read])
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            let _ = asked.send(line);
            socket
                .write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
        }
    });

    // The proxy as Settings > General keeps it; the check resolves its name.
    let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
    db.call(|conn| {
        store::config::set_config(conn, "ProxyEnabled", "True")?;
        store::config::set_config(conn, "ProxyHostname", "127.0.0.1")
    })
    .await
    .unwrap();
    let (runtime, _) = runtime_with(&db, Some(own)).await;
    // The services' ping on a host only the proxy answers for.
    runtime.set_upstreams(unarr_core::metadata::Upstreams::mock("http://services.invalid"));
    let ping = "GET http://services.invalid/radarr.servarr.com/v1/ping HTTP/1.1";

    // Answered through the proxy: nothing to say.
    runtime.check_health(CheckSet::ConfigSaved).await;
    let health = runtime.health();
    assert!(health.iter().all(|h| h.source != "ProxyCheck"), "{health:?}");
    assert_eq!(seen.try_recv().as_deref(), Ok(ping));

    // A 400 from the proxy is the one answer held against it, in
    // Radarr's words.
    assert!(runtime.check_health(CheckSet::ConfigSaved).await);
    assert_eq!(seen.try_recv().as_deref(), Ok(ping));
    let proxy = runtime
        .health()
        .into_iter()
        .find(|h| h.source == "ProxyCheck")
        .expect("the refusal is reported");
    assert_eq!(proxy.kind, HealthType::Error);
    assert_eq!(proxy.message, "Failed to test proxy. StatusCode: BadRequest");
    assert_eq!(
        proxy.wiki_url,
        "https://wiki.servarr.com/radarr/system#proxy-failed-test"
    );
}

/// `CleanLibrary` at the two levels that remove: the movie no list names
/// goes from the library, and the outcome says whether its folder goes
/// too, which is the caller's to do.
#[tokio::test]
async fn cleaning_the_library_says_whether_the_folders_go() {
    use unarr_core::movies::importlists::MovieImportLists;

    for (level, folders_go) in [("removeAndKeep", false), ("removeAndDelete", true)] {
        let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
        let db = fixture_db(&mock).await;
        db.call(move |conn| {
            conn.execute("DELETE FROM providers WHERE kind = 'importList'", [])?;
            store::providers::insert_provider(
                conn,
                &Provider {
                    kind: ProviderKind::ImportList,
                    id: 1,
                    name: "Watchlist".into(),
                    implementation: "IMDbListImport".into(),
                    config_contract: "IMDbListSettings".into(),
                    settings: json!({"listId": "ur0000001"}),
                    tags: vec![],
                    attributes: json!({"enabled": true, "enableAuto": true}),
                },
            )?;
            // The list names every movie but Nightmare Alley (70).
            conn.execute("DELETE FROM import_list_movies", [])?;
            conn.execute(
                "INSERT INTO import_list_movies (id, import_list_id, movie_metadata_id)
                 SELECT id, 1, movie_metadata_id FROM movies WHERE id != 70",
                [],
            )?;
            store::config::set_config(conn, "listsynclevel", level)?;
            Ok(())
        })
        .await
        .unwrap();

        let outcome = MovieImportLists::new(db.clone()).clean_library().await.unwrap();
        let (deleted, delete_files) = outcome.removed.expect("a movie was removed");
        let ids: Vec<i64> = deleted.movies.iter().map(|m| m.id).collect();
        assert_eq!(ids, [70], "{level}");
        assert_eq!(delete_files, folders_go, "{level}");
        assert!(outcome.updated.is_empty(), "{level}");
        assert_eq!(
            count(&db, "SELECT count(*) FROM movies WHERE id = ?1", 70).await,
            0
        );
        assert_eq!(
            count(&db, "SELECT count(*) FROM movies WHERE id = ?1", 116).await,
            1
        );
    }
}

/// `ImportListSync` of every list, the one that cleans (the sync of one
/// list does not, in the original either), with Clean Library removing
/// what the watchlist does not name, against the recorded list: the
/// folders go with `removeAndDelete` and stay with `removeAndKeep`, and a
/// folder a kept movie shares is never deleted.
#[tokio::test]
async fn the_list_sync_deletes_folders_only_when_the_level_says_so() {
    use unarr_core::metadata::Upstreams;

    for (level, deleted) in [("removeAndDelete", true), ("removeAndKeep", false)] {
        let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
        let db = fixture_db(&mock).await;
        let root = std::env::temp_dir().join(format!("unarr_list_clean_{level}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (gone, shared) = (root.join("Nightmare Alley (2021)"), root.join("Shared"));
        for dir in [&gone, &shared] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("movie.mkv"), b"video").unwrap();
        }
        let (gone_path, shared_path) = (
            gone.to_string_lossy().into_owned(),
            shared.to_string_lossy().into_owned(),
        );
        db.call(move |conn| {
            // The fixture's watchlist, pointed at the recorded one, adding
            // nothing so that only the clean runs.
            let mut list = store::providers::get_provider(conn, ProviderKind::ImportList, 1)?.unwrap();
            list.settings = json!({"listId": "ur0000001"});
            list.attributes["enabled"] = json!(true);
            list.attributes["enableAuto"] = json!(false);
            store::providers::update_provider(conn, &list)?;
            store::config::set_config(conn, "listsynclevel", level)?;
            store::config::set_config(conn, "recyclebin", "")?;
            // Due now: the sync of every list is the one that cleans.
            conn.execute("DELETE FROM import_list_status", [])?;
            // Nightmare Alley (70) and The Holdovers (116) are not on the
            // list; Alien: Romulus (6) is, and shares The Holdovers' folder.
            conn.execute("UPDATE movies SET path = ?1 WHERE id = 70", [&gone_path])?;
            conn.execute("UPDATE movies SET path = ?1 WHERE id IN (6, 116)", [&shared_path])?;
            Ok(())
        })
        .await
        .unwrap();
        let (runtime, _) = runtime(&db).await;
        runtime.set_upstreams(Upstreams::mock(&mock.url()));

        let sync = run(
            &runtime,
            CommandBody::ImportListSync { definition_id: None },
            CommandTrigger::Manual,
        )
        .await;
        assert_eq!(
            sync.status,
            CommandStatus::Completed,
            "{level}: {:?}",
            sync.exception
        );
        for (movie_id, kept) in [(70, false), (116, false), (6, true)] {
            let left = count(&db, "SELECT count(*) FROM movies WHERE id = ?1", movie_id).await;
            assert_eq!(left, i64::from(kept), "{level}: movie {movie_id}");
        }
        assert_eq!(gone.is_dir(), !deleted, "{level}: the folder of a removed movie");
        assert!(
            shared.join("movie.mkv").is_file(),
            "{level}: a folder a kept movie shares"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `UpdateHistoryService`: the first start records its version and tells
/// nobody; a start on the same version neither; a start on another one
/// tells every connection that asks, Discord and the webhook as Radarr
/// builds them, whatever the connection's tags.
#[tokio::test]
async fn a_start_on_another_version_tells_the_connections() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = fixture_db(&mock).await;
    let url = mock.url();
    db.call(move |conn| {
        conn.execute("DELETE FROM providers WHERE kind = 'notification'", [])?;
        let notification =
            |id: i64, implementation: &str, settings: serde_json::Value, tags: Vec<i64>| Provider {
                kind: ProviderKind::Notification,
                id,
                name: format!("Mock {implementation}"),
                implementation: implementation.into(),
                config_contract: format!("{implementation}Settings"),
                settings,
                tags,
                attributes: json!({"onApplicationUpdate": true}),
            };
        store::providers::insert_provider(
            conn,
            &notification(
                1,
                "Discord",
                json!({"webHookUrl": format!("{url}/discord/api/webhooks/100/fixturetoken")}),
                vec![],
            ),
        )?;
        store::providers::insert_provider(
            conn,
            &notification(
                2,
                "Webhook",
                json!({"url": format!("{url}/webhook"), "method": 1}),
                vec![7],
            ),
        )?;
        let mut renames_only = notification(
            3,
            "Webhook",
            json!({"url": format!("{url}/webhook/renames"), "method": 1}),
            vec![],
        );
        renames_only.attributes = json!({"onRename": true});
        store::providers::insert_provider(conn, &renames_only)?;
        store::config::set_config(conn, "ApplicationUrl", "http://radarr.test")?;
        Ok(())
    })
    .await
    .unwrap();
    let http = unarr_core::http::build(std::time::Duration::from_secs(10));
    let indexers: &'static IndexerClient = Box::leak(Box::new(IndexerClient::for_personality(
        http.clone(),
        Personality::Radarr,
    )));
    let downloads = Arc::new(MovieDownloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, RADARR),
    ));
    let service = IndexerService::with_client(db.clone(), indexers);
    let side = MovieSide {
        downloads: downloads.clone(),
        search: MovieSearchService::new(db.clone(), service.clone()).with_queue(downloads.queue_source()),
        indexers: service,
        http: None,
    };
    let identity = unarr_core::notifications::Identity {
        instance_name: "Radarr".into(),
        app_name: "Radarr".into(),
        ..Default::default()
    };
    let runtime = Runtime::new_radarr(db.clone(), side, Arc::new(now), identity)
        .await
        .unwrap();

    runtime.application_started("0.1.0").await;
    runtime.application_started("0.1.0").await;
    assert!(mock.clients().notifications.is_empty());

    runtime.application_started("0.2.0").await;
    let sent = mock.clients().notifications.clone();
    assert_eq!(sent.len(), 2, "{sent:#?}");
    let webhook = sent.iter().find(|n| n["service"] == "webhook").unwrap();
    assert_eq!(
        webhook["body"],
        json!({
            "eventType": "ApplicationUpdate",
            "instanceName": "Radarr",
            "applicationUrl": "http://radarr.test",
            "message": "Radarr updated from 0.1.0 to 0.2.0",
            "previousVersion": "0.1.0",
            "newVersion": "0.2.0",
        })
    );
    let discord = sent.iter().find(|n| n["service"] == "discord").unwrap();
    let embed = &discord["body"]["embeds"][0];
    assert_eq!(embed["author"]["name"], "Radarr");
    assert_eq!(embed["title"], "Application Updated");
    assert_eq!(embed["color"], 16761392);
    assert!(embed["timestamp"].is_string());
    assert_eq!(
        embed["fields"],
        json!([
            {"name": "Previous Version", "value": "0.1.0", "inline": false},
            {"name": "New Version", "value": "0.2.0", "inline": false},
        ])
    );
    // The update is recorded: the next start on it tells nobody again.
    mock.clients().notifications.clear();
    runtime.application_started("0.2.0").await;
    assert!(mock.clients().notifications.is_empty());
}
