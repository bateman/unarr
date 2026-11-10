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
    state_in(mock, fixtures_dir().join("sonarr")).await
}

/// The same, with the application data in `app_data`.
async fn state_in(mock: &Mock, app_data: std::path::PathBuf) -> AppState {
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
        RuntimeInfo::detect(app_data),
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

/// The log files the daemon writes: listed with the last written first,
/// read through the API and downloaded where the list says.
#[tokio::test]
async fn log_files_are_listed_newest_first_and_downloaded() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let app_data = std::env::temp_dir().join(format!("unarr_routes_logs_{}", std::process::id()));
    let logs = app_data.join("logs");
    let _ = std::fs::remove_dir_all(&app_data);
    std::fs::create_dir_all(&logs).unwrap();
    let base = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    for (name, age, text) in [
        ("sonarr.0.txt", 0, "older lines\n"),
        ("sonarr.txt", 300, "2026-11-10 09:05:07.3|Info|unarrd|listening\n"),
        ("notes.md", 400, "not a log\n"),
    ] {
        std::fs::write(logs.join(name), text).unwrap();
        std::fs::File::options()
            .write(true)
            .open(logs.join(name))
            .and_then(|f| f.set_modified(base + std::time::Duration::from_secs(age)))
            .unwrap();
    }
    let app = unarr_api::router(state_in(&mock, app_data.clone()).await);

    let (status, list) = send(&app, "GET", "/api/v3/log/file", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let rows: Vec<(i64, &str, &str)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["id"].as_i64().unwrap(),
                f["filename"].as_str().unwrap(),
                f["downloadUrl"].as_str().unwrap(),
            )
        })
        .collect();
    // Numbered by name, shown by time.
    assert_eq!(
        rows,
        [
            (2, "sonarr.txt", "/logfile/sonarr.txt"),
            (1, "sonarr.0.txt", "/logfile/sonarr.0.txt")
        ]
    );
    assert_eq!(list[0]["contentsUrl"], "/api/v3/log/file/sonarr.txt");

    // The download link is a page of the UI: it takes the browser's
    // session, as the backups' links do.
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=unarr&password=fixture&rememberMe=on"))
                .unwrap(),
        )
        .await
        .unwrap();
    let session = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let text = |path: &'static str| {
        let app = app.clone();
        let session = session.clone();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("X-Api-Key", API_KEY)
                        .header("cookie", session)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    for path in ["/logfile/sonarr.txt", "/api/v3/log/file/sonarr.txt"] {
        let (status, body) = text(path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.ends_with("|Info|unarrd|listening\n"), "{path}: {body}");
    }
    // Only a listed file is served.
    for path in [
        "/logfile/notes.md",
        "/logfile/missing.txt",
        "/logfile/..%2Fconfig.xml",
        "/updatelogfile/sonarr.txt",
    ] {
        assert_eq!(text(path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    std::fs::remove_dir_all(&app_data).unwrap();
}

/// Behind a reverse proxy: with "Disabled for Local Addresses" the login is
/// skipped only for a LAN client that a proxy on a trusted network vouches
/// for, as the originals do with `TrustedNetworks`.
#[tokio::test]
async fn a_trusted_proxy_lets_local_clients_skip_the_login() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    use unarr_core::config::AuthenticationRequired;

    let dir = fixtures_dir().join("sonarr");
    let page = |trusted_networks: &'static str, peer: &'static str, forwarded: Option<&'static str>| {
        let dir = dir.clone();
        async move {
            let db = Db::open_in_memory().unwrap();
            unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
            let mut host = unarr_import::config_xml::read(&dir.join("config.xml"), 8989).expect("config.xml");
            host.authentication_required = AuthenticationRequired::DisabledForLocalAddresses;
            host.trusted_networks = trusted_networks.to_string();
            let state = AppState::new(
                Personality::Sonarr,
                db,
                Settings::from_host_config(host, Personality::Sonarr),
                RuntimeInfo::detect(dir),
                LogBuffer::new(),
            );
            let mut request = Request::builder().uri("/system/status");
            if let Some(forwarded) = forwarded {
                request = request.header("X-Forwarded-For", forwarded);
            }
            let mut request = request.body(Body::empty()).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            let response = unarr_api::router(state).oneshot(request).await.unwrap();
            (
                response.status(),
                response
                    .headers()
                    .get("location")
                    .map(|l| l.to_str().unwrap().to_string()),
            )
        }
    };
    let login = (
        StatusCode::FOUND,
        Some("/login?returnUrl=/system/status".to_string()),
    );
    const PROXY: &str = "172.29.4.2:41000";

    // The proxy's network is trusted and the client is on the LAN.
    assert_eq!(
        page("172.29.4.0/22", PROXY, Some("192.168.4.128")).await.0,
        StatusCode::OK
    );
    // The setting empty: the header is not believed, the client is unknown.
    assert_eq!(page("", PROXY, Some("192.168.4.128")).await, login);
    // Trusted proxy, client from the internet.
    assert_eq!(page("172.29.4.0/22", PROXY, Some("203.0.113.9")).await, login);
    // A local-looking address claimed by a sender nobody trusts.
    assert_eq!(
        page("172.29.4.0/22", "203.0.113.9:41000", Some("192.168.4.128")).await,
        login
    );
    assert_eq!(
        page("172.29.4.0/22", "192.168.4.50:41000", Some("192.168.4.128")).await,
        login
    );
    // No proxy at all: a LAN client is exempt, as before.
    assert_eq!(page("", "192.168.4.128:41000", None).await.0, StatusCode::OK);
    assert_eq!(page("", "203.0.113.9:41000", None).await, login);
}

/// The series page on a high-density screen asks for `poster-1000.jpg`,
/// which nothing makes: it gets the full poster, as from the originals.
#[tokio::test]
async fn a_cover_size_that_was_not_made_is_served_from_the_full_cover() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let app_data = std::env::temp_dir().join(format!("unarr_routes_covers_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&app_data);
    let covers = app_data.join("MediaCover").join("5");
    std::fs::create_dir_all(&covers).unwrap();
    std::fs::write(covers.join("poster.jpg"), b"full poster").unwrap();
    std::fs::write(covers.join("poster-500.jpg"), b"poster at 500").unwrap();
    let app = unarr_api::router(state_in(&mock, app_data.clone()).await);
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=unarr&password=fixture&rememberMe=on"))
                .unwrap(),
        )
        .await
        .unwrap();
    let session = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let get = |path: &'static str| {
        let app = app.clone();
        let session = session.clone();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("cookie", session)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    assert_eq!(
        get("/MediaCover/5/poster-500.jpg?lastWrite=1").await,
        (StatusCode::OK, "poster at 500".to_string())
    );
    assert_eq!(
        get("/MediaCover/5/poster-1000.jpg?lastWrite=1").await,
        (StatusCode::OK, "full poster".to_string())
    );
    assert_eq!(
        get("/MediaCover/5/poster.jpg").await,
        (StatusCode::OK, "full poster".to_string())
    );
    assert_eq!(get("/MediaCover/5/banner-70.jpg").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        get("/MediaCover/6/poster-1000.jpg").await.0,
        StatusCode::NOT_FOUND
    );
    std::fs::remove_dir_all(&app_data).unwrap();
}
