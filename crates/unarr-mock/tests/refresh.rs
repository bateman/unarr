//! `RefreshSeries` on the fixture library against the recorded metadata:
//! the series and episodes as the provider says, ids continuing the
//! original's sequence, the covers and their thumbnails, the cast kept.

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use unarr_core::commands::{CommandBody, CommandPriority, CommandStatus, CommandTrigger, Runtime};
use unarr_core::db::Db;
use unarr_core::downloadclients::DownloadClientService;
use unarr_core::downloads::Downloads;
use unarr_core::metadata::Upstreams;
use unarr_core::store;
use unarr_mock::{fixtures_dir, Mock};

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

async fn setup(name: &str) -> Setup {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&fixtures_dir().join("sonarr/sonarr.db"), &db).unwrap();
    let http = unarr_core::http::build(Duration::from_secs(10));
    let downloads = Arc::new(Downloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http),
    ));
    let now = Utc.with_ymd_and_hms(2026, 11, 9, 19, 40, 0).unwrap();
    let runtime = Runtime::new(db.clone(), downloads, Arc::new(move || now), Default::default())
        .await
        .unwrap();
    runtime.set_upstreams(Upstreams::mock(&mock.url()));
    let covers = std::env::temp_dir().join(format!("unarr_refresh_{name}_{}", std::process::id()));
    runtime.set_media_cover_dir(covers.clone());
    Setup {
        _mock: mock,
        db,
        runtime,
        covers,
    }
}

/// Push a command and run it, then whatever it queued.
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
    while let Some(started) = runtime.queue().try_start(runtime.now()) {
        tokio::time::timeout(Duration::from_secs(30), runtime.execute(started))
            .await
            .expect("a command finishes within 30 s");
    }
    runtime.queue().get(model.id).await.unwrap().unwrap()
}

#[tokio::test]
async fn refreshing_one_series_adds_its_new_episodes_and_covers() {
    let s = setup("one").await;
    let before =
        s.db.call(|conn| Ok(store::series::get(conn, 23)?.unwrap()))
            .await
            .unwrap();
    let done = run(
        &s.runtime,
        CommandBody::RefreshSeries {
            series_ids: vec![23],
            is_new_series: false,
        },
    )
    .await;
    assert_eq!(done.status, CommandStatus::Completed, "{:?}", done.exception);

    let (series, episodes) =
        s.db.call(|conn| {
            Ok((
                store::series::get(conn, 23)?.unwrap(),
                store::episodes::list_episodes(conn, 23, None)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(episodes.len(), 44);
    let new_ids: Vec<i64> = episodes.iter().filter(|e| e.id > 1631).map(|e| e.id).collect();
    assert_eq!(
        new_ids,
        (1632..=1640).collect::<Vec<_>>(),
        "the original's sequence continues"
    );
    assert!(series.last_info_sync.is_some());
    assert_eq!(series.monitored, before.monitored, "the user's settings stay");
    assert_eq!(series.quality_profile_id, before.quality_profile_id);
    assert!(
        series.actors.as_array().is_some_and(|a| !a.is_empty()),
        "the cast is written"
    );
    for file in [
        "poster.jpg",
        "poster-500.jpg",
        "poster-250.jpg",
        "fanart-360.jpg",
        "banner-35.jpg",
        "clearlogo.png",
    ] {
        assert!(s.covers.join("23").join(file).is_file(), "{file}");
    }
}

#[tokio::test]
async fn refreshing_every_series_keeps_the_cast_of_each() {
    let s = setup("all").await;
    let done = run(
        &s.runtime,
        CommandBody::RefreshSeries {
            series_ids: vec![],
            is_new_series: false,
        },
    )
    .await;
    assert_eq!(done.status, CommandStatus::Completed, "{:?}", done.exception);
    let series =
        s.db.call(|conn| {
            let ids: Vec<i64> = store::series::list(conn)?.iter().map(|s| s.id).collect();
            ids.into_iter()
                .map(|id| Ok(store::series::get(conn, id)?.unwrap()))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .await
        .unwrap();
    assert_eq!(series.len(), 38);
    for one in &series {
        assert!(one.last_info_sync.is_some(), "{} refreshed", one.title);
        assert!(one.actors.as_array().is_some(), "{}", one.title);
    }
    let mappings = s.db.call(|conn| store::scene::all(conn)).await.unwrap();
    assert!(mappings
        .iter()
        .any(|m| m.mapping_type.as_deref() == Some("ServicesProvider")));
    assert!(mappings
        .iter()
        .any(|m| m.mapping_type.as_deref() == Some("XemService")));
}

/// `docs/DESIGN.md` section 14: a series listed without its cast must never
/// be written back whole. The PUT, the editor and the season pass write by
/// columns, so the cast a refresh stored survives all three.
#[tokio::test]
async fn series_writes_keep_the_cast() {
    use unarr_core::tv::series::{self as service, BulkEdit, SeriesEdit};
    let s = setup("cast").await;
    run(
        &s.runtime,
        CommandBody::RefreshSeries {
            series_ids: vec![23],
            is_new_series: false,
        },
    )
    .await;
    let cast = |db: Db| async move {
        db.call(|conn| Ok(store::series::get(conn, 23)?.unwrap().actors))
            .await
            .unwrap()
    };
    let before = cast(s.db.clone()).await;
    assert!(before.as_array().is_some_and(|a| !a.is_empty()));
    s.db.call(|conn| {
        let listed = store::series::list(conn)?
            .into_iter()
            .find(|x| x.id == 23)
            .unwrap();
        assert_eq!(
            listed.actors,
            serde_json::json!([]),
            "a listed series carries no cast"
        );
        service::update(
            conn,
            &SeriesEdit {
                id: 23,
                tvdb_id: listed.tvdb_id,
                path: listed.path.clone(),
                quality_profile_id: listed.quality_profile_id,
                season_folder: listed.season_folder,
                monitored: false,
                monitor_new_items: listed.monitor_new_items,
                series_type: listed.series_type,
                tags: vec![1],
                seasons: listed.seasons.clone(),
            },
        )?;
        service::update_many(
            conn,
            &BulkEdit {
                series_ids: vec![23],
                monitored: Some(true),
                tags: Some(vec![2]),
                apply_tags: "add".into(),
                ..BulkEdit::default()
            },
        )?;
        service::season_pass(conn, 23, Some(true), &[], None, Utc::now())?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(cast(s.db.clone()).await, before);
}
