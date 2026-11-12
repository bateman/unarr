//! Two personalities side by side, as `unarrd --personalities
//! sonarr,radarr` runs them: each with its own database, routes, web UI,
//! strings and outgoing-connection settings.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use unarr_api::{AppState, LogBuffer, RuntimeInfo, Settings};
use unarr_core::{Db, Personality};

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

fn fixture(personality: Personality) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(personality.app_name().to_lowercase())
}

fn build_state(personality: Personality) -> AppState {
    let dir = fixture(personality);
    let db = Db::open_in_memory_for(personality).expect("in-memory db");
    let legacy = dir.join(personality.legacy_db_file());
    match personality {
        Personality::Sonarr => unarr_import::sonarr::import(&legacy, &db).map(|_| ()),
        Personality::Radarr => unarr_import::radarr::import(&legacy, &db).map(|_| ()),
        Personality::Lidarr => Ok(()),
    }
    .expect("import fixture");
    let host = unarr_import::config_xml::read(&dir.join("config.xml"), personality.default_port())
        .expect("config.xml");
    let settings = Settings::from_host_config(host, personality);
    let mut runtime = RuntimeInfo::detect(dir);
    runtime.media_cover_dir = std::env::temp_dir().join(format!(
        "unarr_personalities_covers_{}_{}",
        personality.app_name(),
        std::process::id()
    ));
    AppState::new(personality, db, settings, runtime, LogBuffer::new())
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(uri)
        .header("X-Api-Key", API_KEY)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn send(app: &axum::Router, method: &str, uri: &str) -> StatusCode {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Api-Key", API_KEY)
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

async fn set_proxy(state: &AppState, host: &str) {
    let host = host.to_string();
    state
        .db()
        .call(move |conn| {
            unarr_core::store::config::set_config(conn, "proxyenabled", "True")?;
            unarr_core::store::config::set_config(conn, "proxyhostname", &host)?;
            unarr_core::store::config::set_config(conn, "proxyport", "3128")?;
            // The default bypasses names without a dot; the URL asked below has one.
            Ok(())
        })
        .await
        .unwrap();
    state.apply_network_settings().await.unwrap();
}

#[tokio::test]
async fn each_personality_follows_its_own_outgoing_settings() {
    let sonarr = build_state(Personality::Sonarr);
    let radarr = build_state(Personality::Radarr);
    sonarr.apply_network_settings().await.unwrap();
    radarr.apply_network_settings().await.unwrap();
    let url = "https://indexer.example.com/api".parse().unwrap();
    assert_eq!(sonarr.http().outbound.proxy_for(&url), None);
    assert_eq!(radarr.http().outbound.proxy_for(&url), None);

    // A proxy saved for Radarr is Radarr's alone.
    set_proxy(&radarr, "proxy-radarr.example.com").await;
    let proxied = radarr
        .http()
        .outbound
        .proxy_for(&url)
        .expect("radarr goes through its proxy");
    assert_eq!(proxied.host_str(), Some("proxy-radarr.example.com"));
    assert_eq!(sonarr.http().outbound.proxy_for(&url), None);
    // The process-wide settings the series side reads are Sonarr's, untouched.
    assert_eq!(unarr_core::outbound::process().proxy_for(&url), None);

    // And one saved for Sonarr leaves Radarr's where it was.
    set_proxy(&sonarr, "proxy-sonarr.example.com").await;
    assert_eq!(
        sonarr
            .http()
            .outbound
            .proxy_for(&url)
            .and_then(|u| u.host_str().map(str::to_string))
            .as_deref(),
        Some("proxy-sonarr.example.com")
    );
    assert_eq!(
        unarr_core::outbound::process()
            .proxy_for(&url)
            .and_then(|u| u.host_str().map(str::to_string))
            .as_deref(),
        Some("proxy-sonarr.example.com")
    );
    assert_eq!(
        radarr
            .http()
            .outbound
            .proxy_for(&url)
            .and_then(|u| u.host_str().map(str::to_string))
            .as_deref(),
        Some("proxy-radarr.example.com")
    );
    // Leave the process-wide settings as other tests of this binary expect.
    sonarr
        .db()
        .call(|conn| unarr_core::store::config::set_config(conn, "proxyenabled", "False"))
        .await
        .unwrap();
    sonarr.apply_network_settings().await.unwrap();
}

#[tokio::test]
async fn each_personality_serves_its_own_routes_and_strings() {
    let sonarr = unarr_api::router(build_state(Personality::Sonarr));
    let radarr = unarr_api::router(build_state(Personality::Radarr));

    let (status, body) = get(&sonarr, "/api/v3/system/status").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"appName\":\"Sonarr\"") && body.contains("\"version\":\"4.0.20.3014\""));
    let (status, body) = get(&radarr, "/api/v3/system/status").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"appName\":\"Radarr\"") && body.contains("\"version\":\"6.4.4.10685\""));

    // Series and episodes are Sonarr's, movies Radarr's: the other answers
    // the originals' empty 404 for a route it does not have.
    assert_eq!(get(&sonarr, "/api/v3/series").await.0, StatusCode::OK);
    assert_eq!(get(&radarr, "/api/v3/movie/6").await.0, StatusCode::OK);
    for (app, uri) in [
        (&radarr, "/api/v3/series"),
        (&radarr, "/api/v3/episode?seriesId=1"),
        (&radarr, "/api/v3/languageprofile"),
        (&sonarr, "/api/v3/movie"),
        (&sonarr, "/api/v3/collection"),
    ] {
        let (status, body) = get(app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(body.is_empty(), "{uri}: {body}");
    }

    // A cover that is not on disk is a 404 naming each runtime's document.
    let (status, body) = get(&radarr, "/api/v3/mediacover/6/poster.jpg").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("rfc9110"), "{body}");
    let (status, body) = get(&sonarr, "/api/v3/mediacover/1/poster.jpg").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("rfc7231"), "{body}");

    // Each feed under its own name only.
    let key = format!("?apikey={API_KEY}");
    assert_eq!(
        get(&sonarr, &format!("/feed/v3/calendar/Sonarr.ics{key}"))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        get(&radarr, &format!("/feed/v3/calendar/Radarr.ics{key}"))
            .await
            .0,
        StatusCode::OK
    );
    let (_, body) = get(&radarr, &format!("/feed/v3/calendar/Radarr.ics{key}")).await;
    assert!(body.contains("NAME:Radarr Movies Calendar"));

    // The strings and the provider schemas are each application's.
    let (_, strings) = get(&sonarr, "/api/v3/localization").await;
    assert!(strings.starts_with("{\"strings\":"));
    let (_, strings) = get(&radarr, "/api/v3/localization").await;
    assert!(strings.starts_with("{\"Strings\":"));
    let (_, schema) = get(&sonarr, "/api/v3/downloadclient/schema").await;
    assert!(schema.contains("tvCategory") && !schema.contains("movieCategory"));
    let (_, schema) = get(&radarr, "/api/v3/downloadclient/schema").await;
    assert!(schema.contains("movieCategory") && schema.contains("recentMoviePriority"));
}

#[tokio::test]
async fn radarr_writes_only_what_its_milestones_brought() {
    let radarr = unarr_api::router(build_state(Personality::Radarr));
    for (method, uri) in [
        ("POST", "/api/v3/movie"),
        ("PUT", "/api/v3/movie/6"),
        ("DELETE", "/api/v3/movie/6"),
        ("POST", "/api/v3/tag"),
        ("PUT", "/api/v3/qualityprofile/11"),
        ("PUT", "/api/v3/config/ui/1"),
        // Radarr's notification controller does not route the bulk actions.
        ("PUT", "/api/v3/notification/bulk"),
    ] {
        let status = send(&radarr, method, uri).await;
        assert!(
            status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
            "{method} {uri} answered {status}"
        );
    }
    // Milestone 4b brought the indexers and the download clients: a body
    // that says nothing is refused as invalid, not as a method.
    // The notifications came with the import of milestone 4c.
    for uri in [
        "/api/v3/indexer",
        "/api/v3/downloadclient",
        "/api/v3/notification",
    ] {
        assert_eq!(send(&radarr, "POST", uri).await, StatusCode::BAD_REQUEST, "{uri}");
    }
    // Nothing was written.
    let (status, body) = get(&radarr, "/api/v3/movie/6").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"title\":\"Alien: Romulus\""));
    // What milestone 4d brings is not a route yet.
    assert_eq!(
        get(&radarr, "/api/v3/movie/lookup?term=alien").await.0,
        StatusCode::NOT_FOUND
    );
    // Milestone 4c brought the rename preview: nothing to rename for a
    // movie whose file the fixture's library does not hold on disk under
    // another name than the settings give it.
    assert_eq!(get(&radarr, "/api/v3/rename?movieId=6").await.0, StatusCode::OK);
    assert_eq!(get(&radarr, "/api/v3/rename").await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn each_personality_serves_its_own_web_ui() {
    let sonarr = unarr_api::router(build_state(Personality::Sonarr));
    let radarr = unarr_api::router(build_state(Personality::Radarr));
    for (app, name) in [(&sonarr, "Sonarr"), (&radarr, "Radarr")] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let page = String::from_utf8_lossy(&bytes);
        assert!(page.contains(&format!("<title>Login - {name}</title>")), "{name}");
    }
}
