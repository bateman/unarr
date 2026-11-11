//! `ImportListSync` on the fixture library against the hand-made Simkl
//! answers, and what Clean Library does with the series no list names.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use unarr_core::commands::{CommandBody, CommandPriority, CommandStatus, CommandTrigger, Runtime};
use unarr_core::db::Db;
use unarr_core::downloadclients::DownloadClientService;
use unarr_core::downloads::Downloads;
use unarr_core::metadata::Upstreams;
use unarr_core::model::{ImportListExclusion, ProviderKind, Series};
use unarr_core::store;
use unarr_mock::{fixtures_dir, Mock};

const LIST: i64 = 1;
const THE_PITT: i64 = 448176;
const PETE_HOLMES: i64 = 271728;
const CHERNOBYL: i64 = 360893;

struct Setup {
    _mock: Mock,
    db: Db,
    runtime: Arc<Runtime>,
    covers: std::path::PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.covers);
    }
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 9, 19, 40, 0).unwrap()
}

/// The fixture library with its one Simkl list pointed at the mock, The
/// Pete Holmes Show excluded and Clean Library set to `level`.
async fn setup(name: &str, level: &str) -> Setup {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&fixtures_dir().join("sonarr/sonarr.db"), &db).unwrap();
    let level = level.to_string();
    db.call(move |conn| {
        // The fixture's list carries a placeholder address; the mock stands
        // in for the real one.
        let mut list = store::providers::get_provider(conn, ProviderKind::ImportList, LIST)?.unwrap();
        list.settings["baseUrl"] = "https://api.simkl.com".into();
        store::providers::update_provider(conn, &list)?;
        store::providers::insert_import_list_exclusion(
            conn,
            &ImportListExclusion {
                id: 1,
                tvdb_id: PETE_HOLMES,
                title: "The Pete Holmes Show".to_string(),
            },
        )?;
        store::config::set_config(conn, "listsynclevel", &level)?;
        store::config::set_config(conn, "listsynctag", "2")?;
        Ok(())
    })
    .await
    .unwrap();
    let http = unarr_core::http::build(Duration::from_secs(10));
    let downloads = Arc::new(Downloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, unarr_core::downloadclients::SONARR),
    ));
    let runtime = Runtime::new(db.clone(), downloads, Arc::new(now), Default::default())
        .await
        .unwrap();
    runtime.set_upstreams(Upstreams::mock(&mock.url()));
    let covers = std::env::temp_dir().join(format!("unarr_import_lists_{name}_{}", std::process::id()));
    runtime.set_media_cover_dir(covers.clone());
    Setup {
        _mock: mock,
        db,
        runtime,
        covers,
    }
}

/// Run a sync of every list, then whatever it queued.
async fn sync(s: &Setup) {
    let model = s
        .runtime
        .queue()
        .push(
            CommandBody::ImportListSync { definition_id: None },
            CommandPriority::Normal,
            CommandTrigger::Manual,
            s.runtime.now(),
        )
        .await
        .unwrap();
    while let Some(started) = s.runtime.queue().try_start(s.runtime.now()) {
        tokio::time::timeout(Duration::from_secs(30), s.runtime.execute(started))
            .await
            .expect("a command finishes within 30 s");
    }
    let done = s.runtime.queue().get(model.id).await.unwrap().unwrap();
    assert_eq!(done.status, CommandStatus::Completed, "{:?}", done.exception);
}

async fn library(s: &Setup) -> Vec<Series> {
    s.db.call(|conn| {
        let ids: Vec<i64> = store::series::list(conn)?.iter().map(|s| s.id).collect();
        store::series::get_many(conn, &ids)
    })
    .await
    .unwrap()
}

async fn owes_a_clean(s: &Setup) -> bool {
    s.db.call(|conn| store::import_lists::sync_status(conn, LIST))
        .await
        .unwrap()
        .has_removed_item_since_last_clean
}

/// The series of `before` as they are now, next to what they were.
async fn pairs(s: &Setup, before: &[Series]) -> Vec<(Series, Series)> {
    let after = library(s).await;
    before
        .iter()
        .map(|was| {
            (
                was.clone(),
                after.iter().find(|is| is.id == was.id).unwrap().clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_sync_adds_what_the_library_lacks_the_way_the_list_asks() {
    let s = setup("sync", "disabled").await;
    let before = library(&s).await;
    sync(&s).await;

    let after = library(&s).await;
    let added: Vec<&Series> = after
        .iter()
        .filter(|a| before.iter().all(|b| b.id != a.id))
        .collect();
    // The Pitt is in the library, The Pete Holmes Show is excluded and the
    // other two have no TVDB id anyone can find.
    assert_eq!(
        added.len(),
        1,
        "{:?}",
        added.iter().map(|s| &s.title).collect::<Vec<_>>()
    );
    let chernobyl = added[0];
    assert_eq!(chernobyl.tvdb_id, CHERNOBYL);
    assert_eq!(chernobyl.path, "/tv/Chernobyl (2019)");
    assert_eq!(chernobyl.quality_profile_id, 10);
    assert_eq!(chernobyl.tags, vec![2]);
    assert!(after.iter().all(|s| s.tvdb_id != PETE_HOLMES));

    let status =
        s.db.call(|conn| store::import_lists::sync_status(conn, LIST))
            .await
            .unwrap();
    assert_eq!(status.last_info_sync, Some(now()));
    // The fixture's list held Crystal Lake, which Simkl no longer names:
    // with Clean Library off that stays owed to the next clean.
    assert!(status.has_removed_item_since_last_clean);

    // A second sync finds nothing new.
    sync(&s).await;
    assert_eq!(library(&s).await.len(), after.len());
}

#[tokio::test]
async fn clean_library_tags_the_series_no_list_names() {
    let s = setup("tag", "keepAndTag").await;
    let before = library(&s).await;
    // The list loses Crystal Lake in this sync, so the clean follows it.
    sync(&s).await;
    let mut tagged = 0;
    for (was, is) in pairs(&s, &before).await {
        if is == was {
            continue;
        }
        tagged += 1;
        assert!(!was.tags.contains(&2), "{}", was.title);
        assert_eq!(is.tags, [was.tags.clone(), vec![2]].concat(), "{}", is.title);
        // Only the settings are written: the rest of the row stays.
        assert_eq!(
            Series {
                tags: was.tags.clone(),
                ..is.clone()
            },
            was,
            "{}",
            is.title
        );
        assert_ne!(is.tvdb_id, THE_PITT);
    }
    assert!(tagged > 0);

    // The clean is done until a list loses an item again.
    assert!(!owes_a_clean(&s).await);
    assert!(s
        .runtime
        .import_lists()
        .try_clean_library()
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn clean_library_unmonitors_the_series_no_list_names() {
    let s = setup("unmonitor", "keepAndUnmonitor").await;
    let before = library(&s).await;
    sync(&s).await;
    let mut unmonitored = 0;
    for (was, is) in pairs(&s, &before).await {
        if is == was {
            continue;
        }
        unmonitored += 1;
        assert!(was.monitored && !is.monitored, "{}", is.title);
        assert_eq!(
            Series {
                monitored: true,
                ..is.clone()
            },
            was,
            "{}",
            is.title
        );
        assert_ne!(is.tvdb_id, THE_PITT);
    }
    assert!(unmonitored > 0);
    assert!(!owes_a_clean(&s).await);
}

#[tokio::test]
async fn clean_library_waits_for_every_list_and_only_logs_when_asked_to() {
    // A list that has never synced holds the clean back.
    let s = setup("unsynced", "keepAndTag").await;
    s.db.call(|conn| {
        store::import_lists::set_sync_status(
            conn,
            LIST,
            &store::import_lists::SyncStatus {
                last_info_sync: None,
                has_removed_item_since_last_clean: true,
            },
        )
    })
    .await
    .unwrap();
    let before = library(&s).await;
    assert!(s
        .runtime
        .import_lists()
        .try_clean_library()
        .await
        .unwrap()
        .is_empty());
    assert_eq!(library(&s).await, before);
    assert!(owes_a_clean(&s).await);

    // Logging counts as a clean; with the setting off nothing happens.
    for (level, owed) in [("logOnly", false), ("disabled", true)] {
        let s = setup(level, level).await;
        let before = library(&s).await;
        sync(&s).await;
        for (was, is) in pairs(&s, &before).await {
            assert_eq!(is, was, "{level}");
        }
        assert_eq!(owes_a_clean(&s).await, owed, "{level}");
    }
}
