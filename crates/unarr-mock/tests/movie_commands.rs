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
