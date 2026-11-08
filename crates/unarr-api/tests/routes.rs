//! Route behaviours the goldens cannot show: a pushed release that reaches
//! the download client (the fixture's episodes are unmonitored, so the
//! recorded push was decided but never grabbed), and request bodies the
//! originals' model binding rejects before a handler runs.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use unarr_api::{AppState, LogBuffer, RuntimeInfo, Settings};
use unarr_core::model::{HistoryEvent, Provider, ProviderKind};
use unarr_core::store;
use unarr_core::{Db, Personality};
use unarr_mock::{fixtures_dir, Mock, API_KEY};

/// The fixture imported, with a SABnzbd pointing at the mock as the only
/// enabled client and episode 1 of The Pitt monitored.
async fn state_with_mock(mock: &Mock) -> AppState {
    let dir = fixtures_dir().join("sonarr");
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
    let port = mock.port();
    db.call(move |conn| {
        store::providers::insert_provider(
            conn,
            &Provider {
                kind: ProviderKind::DownloadClient,
                id: 4,
                name: "Mock SABnzbd".into(),
                implementation: "Sabnzbd".into(),
                config_contract: "SabnzbdSettings".into(),
                settings: json!({"host": "127.0.0.1", "port": port, "urlBase": "/sabnzbd", "apiKey": API_KEY, "tvCategory": "tv"}),
                tags: vec![],
                attributes: json!({"enable": true, "priority": 1, "removeCompletedDownloads": true, "removeFailedDownloads": true}),
            },
        )?;
        conn.execute("UPDATE episodes SET monitored = 1 WHERE id = 1", [])?;
        conn.execute(
            "UPDATE providers SET attributes = json_set(attributes, '$.enable', json('false'))
             WHERE kind = 'downloadClient' AND id IN (1, 2, 3)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let host = unarr_import::config_xml::read(&dir.join("config.xml"), 8989).expect("config.xml");
    let settings = Settings::from_host_config(host, Personality::Sonarr);
    AppState::new(
        Personality::Sonarr,
        db,
        settings,
        RuntimeInfo::detect(dir),
        LogBuffer::new(),
    )
}

async fn send(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("X-Api-Key", API_KEY)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn a_pushed_release_is_sent_to_the_download_client() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let state = state_with_mock(&mock).await;
    let app = unarr_api::router(state.clone());
    let published = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let (status, body) = send(
        &app,
        "POST",
        "/api/v3/release/push",
        json!({
            // The fixture's profile wants Italian releases.
            "title": "The.Pitt.S01E01.iTALiAN.1080p.WEB.H264-MeM.GP",
            "downloadUrl": format!("{}/newznab/download/pushed-release.nzb", mock.url()),
            "protocol": "usenet",
            "publishDate": published,
            "size": 2_000_000_000u64,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let decision = &body[0];
    assert_eq!(decision["approved"], true, "{decision}");
    assert_eq!(decision["rejections"], serde_json::json!([]));

    // `ProcessDecision`: the client received it and the history says so.
    let served: Vec<String> = mock.requests().into_iter().map(|r| r.served).collect();
    assert!(
        served.iter().any(|s| s == "sabnzbd/addfile"),
        "nothing reached the client: {served:?}"
    );
    let rows = state
        .db()
        .call(|conn| store::history::find_by_download_id(conn, "SABnzbd_nzo_mock1"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].event_type, HistoryEvent::Grabbed);
    assert_eq!(rows[0].episode_id, 1);
    assert_eq!(rows[0].data["releaseSource"], "ReleasePush");
}

/// The streamed array must close when the first page is empty: the normal
/// answer to "anything new since I last asked?".
#[tokio::test]
async fn history_since_with_nothing_new_is_an_empty_array() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let state = state_with_mock(&mock).await;
    let app = unarr_api::router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/history/since?date=2100-01-01T00:00:00Z")
                .header("X-Api-Key", API_KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "[]");
    // With rows, the array is complete JSON too.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/history/since?date=2020-01-01T00:00:00Z")
                .header("X-Api-Key", API_KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let rows: Vec<Value> = serde_json::from_slice(&bytes).expect("a complete JSON array");
    assert!(!rows.is_empty());
}

#[tokio::test]
async fn a_body_that_is_not_an_object_is_a_bad_request() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let state = state_with_mock(&mock).await;
    let app = unarr_api::router(state);
    for path in ["/api/v3/indexer/1", "/api/v3/downloadclient/4"] {
        for body in [json!([]), json!("text"), json!(42)] {
            let (status, answer) = send(&app, "PUT", path, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} with {body}: {answer}");
            assert!(
                answer["message"].as_str().unwrap_or("").starts_with("BadRequest"),
                "{answer}"
            );
        }
    }
}
