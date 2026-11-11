//! What the shared HTTP layer answers outside the body, as measured on
//! Sonarr 4.0.20 and Radarr 6.4.4 with `scripts/http-audit.py`: the headers
//! of an API answer, the key as a bearer token, CORS, `HEAD`, a trailing
//! slash, the framework's answers, compression, the login redirect.

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;
use unarr_api::{AppState, LogBuffer, RuntimeInfo, Settings};

use unarr_core::{Db, Personality};
use unarr_mock::fixtures_dir;

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn app() -> axum::Router {
    let dir = fixtures_dir().join("sonarr");
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
    let host = unarr_import::config_xml::read(&dir.join("config.xml"), 8989).expect("config.xml");
    let state = AppState::new(
        Personality::Sonarr,
        db,
        Settings::from_host_config(host, Personality::Sonarr),
        RuntimeInfo::detect(dir),
        LogBuffer::new(),
    );
    unarr_api::router(state)
}

async fn ask(
    app: &axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &'static str,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (parts.status, parts.headers, bytes.to_vec())
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().unwrap())
}

#[tokio::test]
async fn an_api_answer_is_json_in_utf8_versioned_and_never_cached() {
    let app = app();
    let (status, headers, _) = ask(&app, "GET", "/api/v3/system/status", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(header(&headers, "cache-control"), Some("no-cache, no-store"));
    assert_eq!(header(&headers, "pragma"), Some("no-cache"));
    assert_eq!(header(&headers, "expires"), Some("-1"));
    assert_eq!(header(&headers, "x-application-version"), Some("4.0.20.3014"));
    assert_eq!(header(&headers, "access-control-allow-origin"), None);

    // `GET /api` is under the same pipeline; the feed is not versioned.
    let (status, headers, _) = ask(&app, "GET", "/api", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "x-application-version"), Some("4.0.20.3014"));
    let feed = format!("/feed/v3/calendar/Sonarr.ics?apikey={KEY}");
    let (status, headers, _) = ask(&app, "GET", &feed, &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "cache-control"), Some("no-cache, no-store"));
    assert_eq!(header(&headers, "x-application-version"), None);
}

#[tokio::test]
async fn a_refused_key_gets_a_bare_401_on_any_path() {
    let app = app();
    for path in ["/api/v3/system/status", "/api/v3/nosuchthing"] {
        let (status, headers, body) = ask(&app, "GET", path, &[], "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert!(body.is_empty());
        for name in ["x-application-version", "cache-control", "content-type"] {
            assert_eq!(header(&headers, name), None, "{path} {name}");
        }
    }
    // With the key, a path no action takes is an empty 404 of the API.
    let (status, headers, body) = ask(&app, "GET", "/api/v3/nosuchthing", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
    assert_eq!(header(&headers, "x-application-version"), Some("4.0.20.3014"));
}

#[tokio::test]
async fn the_key_is_taken_as_a_bearer_token_after_the_header() {
    let app = app();
    let status = |headers: &'static [(&'static str, &'static str)]| {
        let app = app.clone();
        async move { ask(&app, "GET", "/api/v3/system/status", headers, "").await.0 }
    };
    const BEARER: &str = "Bearer 0123456789abcdef0123456789abcdef";
    assert_eq!(status(&[("Authorization", BEARER)]).await, StatusCode::OK);
    assert_eq!(
        status(&[("Authorization", "bearer 0123456789abcdef0123456789abcdef")]).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(&[("Authorization", "Bearer nope")]).await,
        StatusCode::UNAUTHORIZED
    );
    // The first place that names a key is the one believed.
    assert_eq!(
        status(&[("Authorization", "Bearer nope"), ("X-Api-Key", KEY)]).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&[("Authorization", BEARER), ("X-Api-Key", "nope")]).await,
        StatusCode::UNAUTHORIZED
    );
    // SignalR's negotiation takes it too.
    let (negotiated, headers, _) = ask(
        &app,
        "POST",
        "/signalr/messages/negotiate",
        &[("Authorization", BEARER)],
        "",
    )
    .await;
    assert_eq!(negotiated, StatusCode::OK);
    assert_eq!(header(&headers, "cache-control"), Some("no-cache, no-store"));
}

#[tokio::test]
async fn a_preflight_is_answered_without_the_key_and_any_origin_may_read() {
    let app = app();
    let preflight = [
        ("Origin", "http://example.invalid"),
        ("Access-Control-Request-Method", "PUT"),
        ("Access-Control-Request-Headers", "x-api-key, content-type"),
    ];
    let (status, headers, _) = ask(&app, "OPTIONS", "/api/v3/series", &preflight, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(header(&headers, "access-control-allow-origin"), Some("*"));
    assert_eq!(header(&headers, "access-control-allow-methods"), Some("PUT"));
    assert_eq!(
        header(&headers, "access-control-allow-headers"),
        Some("x-api-key,content-type")
    );
    assert_eq!(header(&headers, "allow"), None);

    // Where there is no action there is no policy: the key is asked for.
    let (status, headers, _) = ask(&app, "OPTIONS", "/api/v3/nosuchthing", &preflight, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header(&headers, "access-control-allow-origin"), None);

    // The answer itself, a 401 included.
    let origin = [("Origin", "http://example.invalid")];
    let (status, headers, _) = ask(&app, "GET", "/api/v3/system/status", &origin, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header(&headers, "access-control-allow-origin"), Some("*"));
    let with_key = [("Origin", "http://example.invalid"), ("X-Api-Key", KEY)];
    let (status, headers, _) = ask(&app, "GET", "/api/v3/system/status", &with_key, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "access-control-allow-origin"), Some("*"));
}

#[tokio::test]
async fn head_is_not_get_on_the_api_and_allow_lists_what_is_declared() {
    let app = app();
    let key = [("X-Api-Key", KEY)];
    for method in ["HEAD", "DELETE"] {
        let (status, headers, _) = ask(&app, method, "/api/v3/system/status", &key, "").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(header(&headers, "allow"), Some("GET"), "{method}");
        assert_eq!(header(&headers, "cache-control"), Some("no-cache, no-store"));
    }
    // `/ping` declares both.
    assert_eq!(ask(&app, "HEAD", "/ping", &[], "").await.0, StatusCode::OK);
    assert_eq!(
        header(&ask(&app, "GET", "/ping", &[], "").await.1, "content-type"),
        Some("application/json; charset=utf-8")
    );
}

#[tokio::test]
async fn one_trailing_slash_is_the_same_path() {
    let app = app();
    let key = [("X-Api-Key", KEY)];
    assert_eq!(ask(&app, "GET", "/api/v3/tag/", &key, "").await.0, StatusCode::OK);
    assert_eq!(
        ask(&app, "GET", "/api/v3/tag/1/", &key, "").await.0,
        StatusCode::OK
    );
    assert_eq!(
        ask(&app, "GET", "/api/v3/tag//", &key, "").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(ask(&app, "GET", "/ping/", &[], "").await.0, StatusCode::OK);
    let (status, _, body) = ask(&app, "GET", "/login/", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("<form"));
}

#[tokio::test]
async fn what_no_action_takes_is_answered_as_the_framework_does() {
    let app = app();
    let key = [("X-Api-Key", KEY)];
    // An id that is not a number matches no route.
    let (status, headers, body) = ask(&app, "GET", "/api/v3/series/abc", &key, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty());
    assert_eq!(header(&headers, "content-type"), None);

    // A body that is not JSON: the validation problem details.
    let json = [("X-Api-Key", KEY), ("Content-Type", "application/json")];
    for (path, content_type) in [
        ("/api/v3/command", "application/json; charset=utf-8"),
        ("/api/v3/tag", "application/problem+json; charset=utf-8"),
    ] {
        for body in ["{not json", ""] {
            let (status, headers, answer) = ask(&app, "POST", path, &json, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {body:?}");
            assert_eq!(header(&headers, "content-type"), Some(content_type), "{path}");
            let answer: serde_json::Value = serde_json::from_slice(&answer).unwrap();
            assert_eq!(answer["title"], "One or more validation errors occurred.");
            assert_eq!(answer["status"], 400);
            assert!(answer["errors"]["$"][0].is_string());
        }
    }
    // One that is not announced as JSON is read by nobody.
    let form = [
        ("X-Api-Key", KEY),
        ("Content-Type", "application/x-www-form-urlencoded"),
    ];
    let (status, headers, body) = ask(&app, "POST", "/api/v3/command", &form, "name=RssSync").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(body.is_empty());
    assert_eq!(header(&headers, "content-type"), None);
}

#[tokio::test]
async fn an_exception_is_answered_by_the_handler_that_starts_from_nothing() {
    let app = app();
    let json = [("X-Api-Key", KEY), ("Content-Type", "application/json")];
    let (status, headers, _) = ask(&app, "POST", "/api/v3/command", &json, r#"{"name":"Nope"}"#).await;
    assert!(status.is_client_error() || status.is_server_error());
    assert_eq!(header(&headers, "content-type"), Some("application/json"));
    assert_eq!(header(&headers, "cache-control"), Some("no-cache,no-store"));
    assert_eq!(header(&headers, "pragma"), Some("no-cache"));
    assert_eq!(header(&headers, "expires"), Some("-1"));
    assert_eq!(header(&headers, "x-application-version"), None);
    assert_eq!(header(&headers, "vary"), None);
}

#[tokio::test]
async fn json_and_pages_are_gzipped_for_a_client_that_takes_it() {
    let app = app();
    let gzip = [("X-Api-Key", KEY), ("Accept-Encoding", "gzip, deflate, br")];
    let (status, headers, body) = ask(&app, "GET", "/api/v3/series", &gzip, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-encoding"), Some("gzip"));
    assert!(header(&headers, "vary").is_some_and(|v| v.eq_ignore_ascii_case("accept-encoding")));
    assert_eq!(&body[..2], &[0x1f, 0x8b]);

    // Not asked for: as it is, and a cache is told the answer depends on it.
    let (_, headers, body) = ask(&app, "GET", "/api/v3/series", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(header(&headers, "content-encoding"), None);
    assert!(header(&headers, "vary").is_some());
    assert_eq!(body[0], b'[');

    // The calendar is not of a type the originals compress.
    let feed = format!("/feed/v3/calendar/Sonarr.ics?apikey={KEY}");
    let (_, headers, _) = ask(&app, "GET", &feed, &[("Accept-Encoding", "gzip")], "").await;
    assert_eq!(header(&headers, "content-encoding"), None);
    assert_eq!(header(&headers, "vary"), None);
}

#[tokio::test]
async fn the_login_redirect_names_the_whole_url_and_escapes_where_to_return() {
    let app = app();
    let (status, headers, _) = ask(
        &app,
        "GET",
        "/settings/general?x=1",
        &[("Host", "sonarr.example.invalid:8989")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        header(&headers, "location"),
        Some("http://sonarr.example.invalid:8989/login?returnUrl=%2Fsettings%2Fgeneral%3Fx%3D1")
    );
    // A request that names no host is sent to a path.
    let (_, headers, _) = ask(&app, "GET", "/", &[], "").await;
    assert_eq!(header(&headers, "location"), Some("/login?returnUrl=%2F"));
    // A host that could not be one is not echoed.
    let (_, headers, _) = ask(&app, "GET", "/", &[("Host", "evil.invalid/x?y")], "").await;
    assert_eq!(header(&headers, "location"), Some("/login?returnUrl=%2F"));

    // The login page, as the originals send a page.
    let (status, headers, _) = ask(&app, "GET", "/login", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), Some("text/html"));
    assert_eq!(header(&headers, "cache-control"), Some("no-cache, no-store"));
    assert_eq!(header(&headers, "pragma"), Some("no-cache"));
    assert_eq!(header(&headers, "expires"), Some("-1"));
}
