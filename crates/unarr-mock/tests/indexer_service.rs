//! The indexer service against the mock: RSS sync with its bookmark,
//! searches, and the back-off an unreachable indexer earns.

use chrono::Utc;
use serde_json::json;
use unarr_core::db::Db;
use unarr_core::indexers::request::{SearchCriteria, SearchKind, SearchMode, SeriesIds};
use unarr_core::indexers::settings::NewznabSettings;
use unarr_core::indexers::{IndexerService, Purpose};
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::store;
use unarr_mock::{fixtures_dir, Mock, API_KEY};

fn provider(id: i64, name: &str, implementation: &str, base_url: &str) -> Provider {
    Provider {
        kind: ProviderKind::Indexer,
        id,
        name: name.into(),
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
async fn rss_sync_merges_feeds_and_remembers_the_newest_release() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = db_with(vec![
        provider(1, "Newznab", "Newznab", &format!("{}/newznab", mock.url())),
        provider(2, "Torznab", "Torznab", &format!("{}/torznab", mock.url())),
    ])
    .await;
    let service = IndexerService::new(db.clone());

    let releases = service.fetch_recent(Utc::now()).await.unwrap();
    assert_eq!(releases.len(), 18, "nine items from each feed");
    assert!(
        releases
            .windows(2)
            .all(|w| w[0].publish_date >= w[1].publish_date),
        "newest first"
    );
    assert_eq!(releases.iter().filter(|r| r.indexer_id == 1).count(), 9);
    assert_eq!(releases.iter().filter(|r| r.indexer_id == 2).count(), 9);
    let torznab = releases.iter().find(|r| r.indexer_id == 2).unwrap();
    assert_eq!(torznab.indexer, "Torznab");
    assert!(torznab.seeders.is_some());

    // The newest release of each feed is the bookmark of the next sync.
    let last = service.last_rss_release(1).await.expect("bookmark stored");
    let newest = releases
        .iter()
        .filter(|r| r.indexer_id == 1)
        .max_by_key(|r| r.publish_date)
        .unwrap();
    assert_eq!(last.publish_date, newest.publish_date);
    assert_eq!(last.download_url, newest.download_url);

    // The second sync reaches the bookmark on the first page.
    mock.clear();
    let again = service.fetch_recent(Utc::now()).await.unwrap();
    assert_eq!(again.len(), 18);
    let pages: Vec<_> = mock
        .requests()
        .into_iter()
        .filter(|r| r.query.get("t").map(String::as_str) == Some("tvsearch"))
        .collect();
    assert_eq!(pages.len(), 2, "one RSS page per indexer: {pages:?}");
    assert!(pages
        .iter()
        .all(|p| p.query.get("offset").map(String::as_str) == Some("0")));
}

#[tokio::test]
async fn searches_ask_every_enabled_indexer() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = db_with(vec![
        provider(1, "Newznab", "Newznab", &format!("{}/newznab", mock.url())),
        provider(2, "Torznab", "Torznab", &format!("{}/torznab", mock.url())),
    ])
    .await;
    let service = IndexerService::new(db);
    let criteria = SearchCriteria {
        series: SeriesIds {
            tvdb_id: 448176,
            imdb_id: None,
            tv_rage_id: 0,
            tv_maze_id: 0,
            tmdb_id: 0,
        },
        scene_titles: vec!["The Pitt".into()],
        mode: SearchMode::Default,
        kind: SearchKind::SingleEpisode {
            season: 1,
            episode: 1,
        },
    };
    let releases = service
        .search(&criteria, true, &[], Utc::now())
        .await
        .unwrap()
        .releases;
    assert_eq!(releases.len(), 10, "five from each indexer");
    let requests = mock.requests();
    let by_id = |path: &str| {
        requests
            .iter()
            .any(|r| r.path == path && r.query.get("tvdbid").map(String::as_str) == Some("448176"))
    };
    assert!(by_id("/newznab/api"));
    assert!(by_id("/torznab/api"));
    assert!(
        !requests.iter().any(|r| r.query.contains_key("q")),
        "the id search found results, so no title search: {requests:?}"
    );
}

#[tokio::test]
async fn an_unreachable_indexer_is_backed_off_and_recovers() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = db_with(vec![
        provider(1, "Newznab", "Newznab", &format!("{}/newznab", mock.url())),
        provider(2, "Dead", "Newznab", "http://127.0.0.1:18999/newznab"),
        provider(3, "Wrong key", "Newznab", &format!("{}/newznab", mock.url())),
    ])
    .await;
    db.call(|conn| {
        let mut p = store::providers::get_provider(conn, ProviderKind::Indexer, 3)?.unwrap();
        p.settings["apiKey"] = json!("not-the-key");
        store::providers::update_provider(conn, &p)?;
        Ok(())
    })
    .await
    .unwrap();
    let service = IndexerService::new(db.clone());

    let now = Utc::now();
    let releases = service.fetch_recent(now).await.unwrap();
    assert_eq!(releases.len(), 9, "only the reachable indexer answers");
    let statuses = db
        .call(|conn| store::provider_status::provider_statuses(conn, ProviderKind::Indexer))
        .await
        .unwrap();
    let status = |id: i64| statuses.iter().find(|(i, _)| *i == id).map(|(_, s)| s.clone());
    assert!(status(1).map(|s| s.escalation_level == 0).unwrap_or(true));
    for id in [2, 3] {
        let s = status(id).expect("failure recorded");
        assert_eq!(s.escalation_level, 1);
        assert!(s.is_disabled(now));
        assert!(s.initial_failure.is_some() && s.most_recent_failure.is_some());
    }
    assert!(status(2).unwrap().last_rss_sync_release_info.is_none());

    // While blocked, the failed indexers are not asked at all.
    mock.clear();
    let names: Vec<String> = service
        .enabled_for(Purpose::Rss, now)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, ["Newznab"]);
    service.fetch_recent(now).await.unwrap();
    assert!(mock
        .requests()
        .iter()
        .all(|r| r.query.get("apikey").map(String::as_str) != Some("not-the-key")));
}
