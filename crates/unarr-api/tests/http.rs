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
    app_for(Personality::Sonarr, "")
}

/// One personality on its fixture, under a URL base when one is given.
fn app_for(personality: Personality, url_base: &str) -> axum::Router {
    app_named(personality, url_base, None)
}

/// The same under another instance name, when one is given.
fn app_named(personality: Personality, url_base: &str, instance_name: Option<&str>) -> axum::Router {
    app_edited(personality, url_base, |host| {
        if let Some(name) = instance_name {
            host.instance_name = Some(name.to_string());
        }
    })
}

/// The same with the settings `edit` changes.
fn app_edited(
    personality: Personality,
    url_base: &str,
    edit: impl FnOnce(&mut unarr_core::config::HostConfig),
) -> axum::Router {
    app_and_state(personality, url_base, edit).0
}

/// The same, with the state, for a test that changes the settings after the
/// router was built, as a save from the UI does.
fn app_and_state(
    personality: Personality,
    url_base: &str,
    edit: impl FnOnce(&mut unarr_core::config::HostConfig),
) -> (axum::Router, AppState) {
    let name = personality.app_name().to_lowercase();
    let dir = fixtures_dir().join(&name);
    let db = Db::open_in_memory_for(personality).unwrap();
    let legacy = dir.join(personality.legacy_db_file());
    match personality {
        Personality::Radarr => unarr_import::radarr::import(&legacy, &db).map(|_| ()),
        _ => unarr_import::sonarr::import(&legacy, &db).map(|_| ()),
    }
    .expect("import fixture");
    let mut host = unarr_import::config_xml::read(&dir.join("config.xml"), personality.default_port())
        .expect("config.xml");
    host.url_base = url_base.to_string();
    edit(&mut host);
    let mut runtime = RuntimeInfo::detect(dir);
    runtime.media_cover_dir = covers_dir(personality);
    let state = AppState::new(
        personality,
        db,
        Settings::from_host_config(host, personality),
        runtime,
        LogBuffer::new(),
    );
    (unarr_api::router(state.clone()), state)
}

/// A cover folder of this test run's own, with one poster in it.
fn covers_dir(personality: Personality) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "unarr_http_covers_{}_{}",
        personality.app_name(),
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("6")).expect("cover folder");
    std::fs::write(dir.join("6/poster.jpg"), b"\xff\xd8\xff\xd9").expect("cover");
    dir
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

/// Under a URL base the root answers with or without its slash: the path
/// a `location /sonarr/` proxy forwards, and where the login form returns.
/// The login redirect names the path as it was asked, base and slash
/// included (recorded: `/tv/` gives `returnUrl=%2Ftv%2F`).
#[tokio::test]
async fn the_root_under_a_url_base_takes_a_trailing_slash() {
    let app = app_for(Personality::Sonarr, "/sonarr");
    for (path, back) in [("/sonarr", "%2Fsonarr"), ("/sonarr/", "%2Fsonarr%2F")] {
        let (status, headers, _) = ask(&app, "GET", path, &[], "").await;
        assert_eq!(status, StatusCode::FOUND, "{path}");
        assert_eq!(
            header(&headers, "location"),
            Some(format!("/sonarr/login?returnUrl={back}").as_str()),
            "{path}"
        );
    }
    for path in ["/sonarr/login", "/sonarr/login/"] {
        assert_eq!(ask(&app, "GET", path, &[], "").await.0, StatusCode::OK, "{path}");
    }
}

/// Outside the URL base the originals route a request as if the base were
/// not there, authenticate it so, then send it into the base with a 307
/// (`UrlBaseMiddleware`). Every case is one recorded on Sonarr 4.0.20 and
/// Radarr 6.4.4 under the base `/tv`, Forms authentication, no login.
#[tokio::test]
async fn a_request_outside_the_url_base_is_sent_into_it_once_authenticated() {
    for personality in [Personality::Sonarr, Personality::Radarr] {
        let app = app_for(personality, "/tv");
        let version = unarr_api::compat::emulated_version(personality);
        let feed = format!("/feed/v3/calendar/{}.ics?apikey={KEY}", personality.app_name());
        let key = [("X-Api-Key", KEY)];
        let bearer = format!("Bearer {KEY}");
        let negotiate = format!("/signalr/messages/negotiate?access_token={KEY}");
        let status = format!("/api/v3/system/status?apikey={KEY}&x=1");
        for (method, path, headers, expected, location) in [
            // Anonymous routes go in at once.
            ("GET", "/ping", &[][..], 307, Some("/tv/ping".to_string())),
            ("HEAD", "/ping", &[][..], 307, Some("/tv/ping".to_string())),
            (
                "GET",
                "/login?returnUrl=%2F",
                &[][..],
                307,
                Some("/tv/login?returnUrl=%2F".to_string()),
            ),
            ("GET", "/logout", &[][..], 307, Some("/tv/logout".to_string())),
            (
                "GET",
                "/Content/manifest.json",
                &[][..],
                307,
                Some("/tv/Content/manifest.json".to_string()),
            ),
            // The API and the feed, with the key.
            (
                "GET",
                "/api/v3/system/status",
                &key[..],
                307,
                Some("/tv/api/v3/system/status".to_string()),
            ),
            ("GET", status.as_str(), &[][..], 307, Some(format!("/tv{status}"))),
            (
                "GET",
                "/api/v3/system/status",
                &[("Authorization", bearer.as_str())][..],
                307,
                Some("/tv/api/v3/system/status".to_string()),
            ),
            (
                "POST",
                "/api/v3/command",
                &key[..],
                307,
                Some("/tv/api/v3/command".to_string()),
            ),
            ("GET", "/api", &key[..], 307, Some("/tv/api".to_string())),
            (
                "GET",
                "/api/v1/foo",
                &key[..],
                307,
                Some("/tv/api/v1/foo".to_string()),
            ),
            ("GET", feed.as_str(), &[][..], 307, Some(format!("/tv{feed}"))),
            (
                "POST",
                negotiate.as_str(),
                &[][..],
                307,
                Some(format!("/tv{negotiate}")),
            ),
            // Without it, or with a wrong one: 401, a cookie would not do.
            ("GET", "/api/v3/system/status", &[][..], 401, None),
            (
                "GET",
                "/api/v3/system/status",
                &[("X-Api-Key", "nope")][..],
                401,
                None,
            ),
            ("POST", "/signalr/messages/negotiate", &[][..], 401, None),
            // A method no route takes asks for the key too.
            ("POST", "/ping", &[][..], 401, None),
            ("POST", "/foo", &[][..], 401, None),
            ("POST", "/foo", &key[..], 307, Some("/tv/foo".to_string())),
            // Pages ask for the login, outside the base.
            ("GET", "/", &[][..], 302, Some("/login?returnUrl=%2F".to_string())),
            (
                "GET",
                "/series?x=1",
                &[][..],
                302,
                Some("/login?returnUrl=%2Fseries%3Fx%3D1".to_string()),
            ),
            (
                "GET",
                "/foo",
                &key[..],
                302,
                Some("/login?returnUrl=%2Ffoo".to_string()),
            ),
            (
                "GET",
                "/initialize.json",
                &[][..],
                302,
                Some("/login?returnUrl=%2Finitialize.json".to_string()),
            ),
            (
                "GET",
                "/favicon.ico",
                &[][..],
                302,
                Some("/login?returnUrl=%2Ffavicon.ico".to_string()),
            ),
            (
                "GET",
                "/tvx/ping",
                &[][..],
                302,
                Some("/login?returnUrl=%2Ftvx%2Fping".to_string()),
            ),
            // Inside, in any case: the login redirect in the case asked.
            ("GET", "/TV/ping", &[][..], 200, None),
            (
                "GET",
                "/tv/series?x=1",
                &[][..],
                302,
                Some("/tv/login?returnUrl=%2Ftv%2Fseries%3Fx%3D1".to_string()),
            ),
            (
                "GET",
                "/TV/series",
                &[][..],
                302,
                Some("/TV/login?returnUrl=%2FTV%2Fseries".to_string()),
            ),
        ] {
            let (got, answer, _) = ask(&app, method, path, headers, "").await;
            assert_eq!(got.as_u16(), expected, "{personality:?} {method} {path}");
            assert_eq!(
                header(&answer, "location"),
                location.as_deref(),
                "{personality:?} {method} {path}"
            );
            let versioned = got == StatusCode::TEMPORARY_REDIRECT && path.starts_with("/api");
            assert_eq!(
                header(&answer, "x-application-version"),
                versioned.then_some(version),
                "{personality:?} {method} {path}"
            );
            if got == StatusCode::TEMPORARY_REDIRECT {
                assert_eq!(
                    header(&answer, "cache-control"),
                    None,
                    "{personality:?} {method} {path}"
                );
            }
        }
    }
}

/// Under `/api` the originals answer what no action takes behind the key:
/// 401 without it, then an empty 404 for `GET` and 405 `Allow: GET` for any
/// other method, `/api/v3` and the other versions alike, with the API's
/// headers. A page's path takes `GET` only: another method asks for the
/// key (a cookie does not do) and gets 405. Recorded on Sonarr 4.0.20 and
/// Radarr 6.4.4.
#[tokio::test]
async fn what_no_action_takes_is_refused_as_the_originals_refuse_it() {
    for personality in [Personality::Sonarr, Personality::Radarr] {
        let app = app_for(personality, "");
        let version = unarr_api::compat::emulated_version(personality);
        let key = [("X-Api-Key", KEY)];
        for path in [
            "/api/v1/foo",
            "/api/v1/system/status",
            "/api/v2/system/status",
            "/api/foo",
            "/API/V1/foo",
            "/api/v3/nosuchthing",
        ] {
            for (method, expected) in [
                ("GET", 404),
                ("POST", 405),
                ("PUT", 405),
                ("DELETE", 405),
                ("HEAD", 405),
                ("OPTIONS", 405),
            ] {
                let (got, headers, body) = ask(&app, method, path, &key, "").await;
                assert_eq!(got.as_u16(), expected, "{personality:?} {method} {path}");
                assert!(body.is_empty(), "{personality:?} {method} {path}");
                assert_eq!(
                    header(&headers, "allow"),
                    (expected == 405).then_some("GET"),
                    "{personality:?} {method} {path}"
                );
                assert_eq!(
                    header(&headers, "x-application-version"),
                    Some(version),
                    "{personality:?} {method} {path}"
                );
                // Not on an answer to `OPTIONS`, as recorded.
                let cached = (method != "OPTIONS").then_some("no-cache, no-store");
                assert_eq!(
                    header(&headers, "cache-control"),
                    cached,
                    "{personality:?} {method} {path}"
                );
            }
            for method in ["GET", "POST"] {
                let (got, headers, _) = ask(&app, method, path, &[], "").await;
                assert_eq!(got, StatusCode::UNAUTHORIZED, "{personality:?} {method} {path}");
                assert_eq!(header(&headers, "location"), None);
            }
        }
        let (got, _, body) = ask(&app, "GET", "/API", &key, "").await;
        assert_eq!(got, StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, serde_json::json!({"current": "v3", "deprecated": []}));
        // `/apix` is a page, behind the login.
        let (got, headers, _) = ask(&app, "GET", "/apix/foo", &key, "").await;
        assert_eq!(got, StatusCode::FOUND);
        assert_eq!(
            header(&headers, "location"),
            Some("/login?returnUrl=%2Fapix%2Ffoo")
        );
        for method in ["POST", "PUT", "DELETE", "HEAD", "OPTIONS"] {
            assert_eq!(
                ask(&app, method, "/foo", &[], "").await.0,
                StatusCode::UNAUTHORIZED,
                "{method}"
            );
            let (got, headers, _) = ask(&app, method, "/foo", &key, "").await;
            assert_eq!(got, StatusCode::METHOD_NOT_ALLOWED, "{method}");
            assert_eq!(header(&headers, "allow"), Some("GET"), "{method}");
        }
        // The key does not open a page.
        let (got, headers, _) = ask(&app, "GET", "/foo", &key, "").await;
        assert_eq!(got, StatusCode::FOUND);
        assert_eq!(header(&headers, "location"), Some("/login?returnUrl=%2Ffoo"));
    }
}

/// With Allowed Hosts set, the host checked is the one a proxy the
/// installation trusts says the client asked for: the originals run the
/// forwarded headers before the host filter. Refused, the request gets the
/// framework's page and a line in the log. Recorded on Sonarr 4.0.20 and
/// Radarr 6.4.4 with `AllowedHosts` `tv.example.com`, the loopback trusted
/// by default, `192.0.2.2` not.
#[tokio::test]
async fn a_trusted_proxys_forwarded_host_is_what_allowed_hosts_checks() {
    use tracing_subscriber::layer::SubscriberExt;
    let logs = LogBuffer::new();
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
    let app = app_edited(Personality::Sonarr, "", |host| {
        host.allowed_hosts = "tv.example.com".into()
    });
    let lo: std::net::SocketAddr = "127.0.0.1:40000".parse().unwrap();
    let other: std::net::SocketAddr = "192.0.2.2:40000".parse().unwrap();
    for (peer, host, forwarded, expected) in [
        (lo, Some("tv.example.com"), &[][..], 200),
        (lo, Some("unarr:8989"), &[][..], 400),
        (
            lo,
            Some("unarr:8989"),
            &[("X-Forwarded-Host", "tv.example.com")][..],
            200,
        ),
        (
            lo,
            Some("unarr:8989"),
            &[("X-Forwarded-Host", "tv.example.com:443")][..],
            200,
        ),
        (
            lo,
            Some("tv.example.com"),
            &[("X-Forwarded-Host", "evil.example")][..],
            400,
        ),
        (
            lo,
            Some("unarr:8989"),
            &[("X-Forwarded-Host", "evil.example, tv.example.com")][..],
            400,
        ),
        (
            lo,
            Some("unarr:8989"),
            &[("X-Forwarded-Host", "tv.example.com, evil.example")][..],
            200,
        ),
        (
            lo,
            Some("unarr:8989"),
            &[
                ("X-Forwarded-Host", "tv.example.com"),
                ("X-Forwarded-For", "10.1.2.3"),
            ][..],
            200,
        ),
        (
            lo,
            Some("unarr:8989"),
            &[
                ("X-Forwarded-Host", "evil.example, tv.example.com"),
                ("X-Forwarded-For", "10.1.2.3, 127.0.0.1"),
            ][..],
            400,
        ),
        (
            other,
            Some("unarr:8989"),
            &[("X-Forwarded-Host", "tv.example.com")][..],
            400,
        ),
        (
            other,
            Some("tv.example.com"),
            &[("X-Forwarded-Host", "evil.example")][..],
            200,
        ),
        (lo, Some("unarr:8989"), &[("X-Forwarded-Host", "")][..], 400),
        (lo, None, &[][..], 200),
    ] {
        let mut request = Request::builder()
            .uri("/api/v3/system/status")
            .header("X-Api-Key", KEY)
            .extension(axum::extract::ConnectInfo(peer));
        if let Some(host) = host {
            request = request.header("Host", host);
        }
        for (name, value) in forwarded {
            request = request.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            expected,
            "{peer} {host:?} {forwarded:?}"
        );
        if expected == 400 {
            assert_eq!(header(response.headers(), "content-type"), Some("text/html"));
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert!(body.starts_with(b"<!DOCTYPE HTML PUBLIC"));
            assert!(
                body.ends_with(b"<p>HTTP Error 400. The request hostname is invalid.</p>\r\n</BODY></HTML>")
            );
        }
    }
    let refused: Vec<String> = logs
        .entries()
        .into_iter()
        .rev()
        .filter(|e| e.logger == "Microsoft.AspNetCore.HostFiltering.HostFilteringMiddleware")
        .map(|e| format!("{}|{}", e.level, e.message))
        .collect();
    assert_eq!(
        refused,
        [
            "info|The host 'unarr:8989' does not match an allowed host.",
            "info|The host 'evil.example' does not match an allowed host.",
            "info|The host 'evil.example' does not match an allowed host.",
            "info|The host 'evil.example' does not match an allowed host.",
            "info|The host 'unarr:8989' does not match an allowed host.",
            "info|The host 'unarr:8989' does not match an allowed host.",
        ]
    );
}

/// A login, a failed one, a logout, and Basic credentials on every
/// request leave the lines log-based intrusion detection reads, as the
/// originals write them (recorded on Sonarr 4.0.20 and Radarr 6.4.4):
/// the client's address once the trusted proxies have spoken, the user
/// name as given. Radarr does not log a success.
#[tokio::test]
async fn logins_and_logouts_are_logged_for_intrusion_detection() {
    use tracing_subscriber::layer::SubscriberExt;
    let lo: std::net::SocketAddr = "127.0.0.1:40000".parse().unwrap();
    let other: std::net::SocketAddr = "192.0.2.2:40000".parse().unwrap();
    let send =
        |app: axum::Router, method: &str, path: &str, peer, headers: &[(&str, &str)], body: &'static str| {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .extension(axum::extract::ConnectInfo(peer));
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            app.oneshot(request.body(Body::from(body)).unwrap())
        };
    let form = [("Content-Type", "application/x-www-form-urlencoded")];
    let forwarded = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("X-Forwarded-For", "10.9.8.7"),
    ];
    for personality in [Personality::Sonarr, Personality::Radarr] {
        let logs = LogBuffer::new();
        let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
        let app = app_for(personality, "");
        // The last name carries a line break and the rest of a forged line.
        let forged = "username=x%27%0A2026-11-19+10%3A00%3A00.0%7CWarn%7CAuth%7CAuth-Failure+ip+192.168.1.10+username+%27y&password=nope";
        for (peer, headers, body) in [
            (lo, &form[..], "username=unarr&password=nope"),
            (lo, &form[..], "username=nobody&password=nope"),
            (other, &form[..], "username=unarr&password=nope"),
            (lo, &forwarded[..], "username=unarr&password=nope"),
            (lo, &form[..], "username=unarr&password=fixture"),
            (lo, &form[..], forged),
        ] {
            assert_eq!(
                send(app.clone(), "POST", "/login", peer, headers, body)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FOUND
            );
        }
        assert_eq!(
            send(app.clone(), "GET", "/logout", lo, &[], "")
                .await
                .unwrap()
                .status(),
            StatusCode::FOUND
        );
        let mut expected = vec![
            "warn|Auth-Failure ip 127.0.0.1 username 'unarr'",
            "warn|Auth-Failure ip 127.0.0.1 username 'nobody'",
            "warn|Auth-Failure ip 192.0.2.2 username 'unarr'",
            "warn|Auth-Failure ip 10.9.8.7 username 'unarr'",
            "info|Auth-Success ip 127.0.0.1 username 'unarr'",
            // One line, its address the client's: the line break is gone.
            "warn|Auth-Failure ip 127.0.0.1 username 'x'2026-11-19 10:00:00.0|Warn|Auth|Auth-Failure ip 192.168.1.10 username 'y'",
            "info|Auth-Logout ip 127.0.0.1 username ''",
        ];
        if personality == Personality::Radarr {
            expected.remove(4);
        }
        let lines: Vec<String> = logs
            .entries()
            .into_iter()
            .rev()
            .filter(|e| e.logger == "Auth")
            .map(|e| format!("{}|{}", e.level, e.message))
            .collect();
        assert_eq!(lines, expected, "{personality:?}");
    }

    // Basic, Sonarr's: each request with credentials, none without.
    let logs = LogBuffer::new();
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
    let app = app_edited(Personality::Sonarr, "", |host| {
        host.authentication_method = unarr_core::config::AuthenticationMethod::Basic
    });
    use base64::Engine;
    let basic = |user: &str, password: &str| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    };
    for (peer, credentials, expected) in [
        (lo, Some(basic("unarr", "nope")), 401),
        (lo, Some(basic("nobody", "nope")), 401),
        (lo, Some(basic("unarr", "fixture")), 200),
        (lo, None, 401),
        (other, Some(basic("unarr", "nope")), 401),
        (lo, Some(basic("a\r\nb", "nope")), 401),
    ] {
        let headers: Vec<(&str, &str)> = credentials
            .iter()
            .map(|c| ("Authorization", c.as_str()))
            .collect();
        let response = send(app.clone(), "GET", "/", peer, &headers, "").await.unwrap();
        assert_eq!(response.status().as_u16(), expected, "{peer} {credentials:?}");
        if expected == 401 {
            assert_eq!(
                header(response.headers(), "www-authenticate"),
                Some("Basic realm=\"Sonarr\"")
            );
        }
    }
    let lines: Vec<String> = logs
        .entries()
        .into_iter()
        .rev()
        .filter(|e| e.logger == "Auth")
        .map(|e| format!("{}|{}", e.level, e.message))
        .collect();
    assert_eq!(
        lines,
        [
            "warn|Auth-Failure ip 127.0.0.1 username 'unarr'",
            "warn|Auth-Failure ip 127.0.0.1 username 'nobody'",
            "info|Auth-Success ip 127.0.0.1 username 'unarr'",
            "warn|Auth-Failure ip 192.0.2.2 username 'unarr'",
            "warn|Auth-Failure ip 127.0.0.1 username 'ab'",
        ]
    );
}

/// A URL base saved from the UI changes the settings at once, while the
/// routes stay under the base they were built with until the restart. The
/// 307 into the base and what counts as inside it follow the routes: a
/// request under the new base is not sent into it again and again, and an
/// empty base saved does not send a request to itself.
#[tokio::test]
async fn a_url_base_saved_from_the_ui_sends_no_request_round_in_circles() {
    let (app, state) = app_and_state(Personality::Sonarr, "/sonarr", |_| {});
    for saved in ["/tv", ""] {
        let mut host = state.settings().host.clone();
        host.url_base = saved.to_string();
        state.set_settings(unarr_api::Settings::from_host_config(host, Personality::Sonarr));
        let (status, headers, _) = ask(&app, "GET", "/ping", &[], "").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{saved:?}");
        assert_eq!(header(&headers, "location"), Some("/sonarr/ping"), "{saved:?}");
        assert_eq!(
            ask(&app, "GET", "/sonarr/ping", &[], "").await.0,
            StatusCode::OK,
            "{saved:?}"
        );
        let (status, headers, _) = ask(&app, "GET", "/tv/x", &[], "").await;
        assert_eq!(status, StatusCode::FOUND, "{saved:?}");
        assert_eq!(
            header(&headers, "location"),
            Some("/login?returnUrl=%2Ftv%2Fx"),
            "{saved:?}"
        );
        let (status, headers, _) = ask(&app, "GET", "/api/v3/system/status", &[("X-Api-Key", KEY)], "").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{saved:?}");
        assert_eq!(
            header(&headers, "location"),
            Some("/sonarr/api/v3/system/status"),
            "{saved:?}"
        );
    }
}

/// The web app manifest and the tile description are filled in as they
/// are served: the instance name, and the addresses under the URL base.
/// The values are those Sonarr 4.0.20 and Radarr 6.4.4 answered on their
/// fixtures, without a base under their own name and under `/tv-app` as
/// "Sonarr Salotto", `/film` as "Radarr 4K"; without a login, in any case
/// of the path.
#[tokio::test]
async fn the_manifest_and_the_tile_description_are_filled_in() {
    for (personality, base, name) in [
        (Personality::Sonarr, "", None),
        (Personality::Sonarr, "/tv-app", Some("Sonarr Salotto")),
        (Personality::Radarr, "", None),
        (Personality::Radarr, "/film", Some("Radarr 4K")),
    ] {
        let app = app_named(personality, base, name);
        let shown = name.unwrap_or(personality.app_name());
        for path in ["/Content/manifest.json", "/content/Manifest.json"] {
            let (status, headers, body) = ask(&app, "GET", &format!("{base}{path}"), &[], "").await;
            assert_eq!(status, StatusCode::OK, "{base}{path}");
            assert_eq!(header(&headers, "content-type"), Some("application/json"));
            let manifest: serde_json::Value = serde_json::from_slice(&body).expect("a JSON manifest");
            assert_eq!(manifest["name"], shown);
            assert_eq!(manifest["start_url"], format!("{base}/"));
            assert_eq!(
                manifest["icons"][0]["src"],
                format!("{base}/Content/Images/Icons/android-chrome-192x192.png")
            );
            assert_eq!(
                manifest["icons"][1]["src"],
                format!("{base}/Content/Images/Icons/android-chrome-512x512.png")
            );
            // The icon the manifest names is there to be fetched.
            let icon = manifest["icons"][0]["src"].as_str().unwrap();
            assert_eq!(ask(&app, "GET", icon, &[], "").await.0, StatusCode::OK);
        }
        let (status, headers, body) =
            ask(&app, "GET", &format!("{base}/Content/browserconfig.xml"), &[], "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header(&headers, "content-type"), Some("text/xml"));
        let text = String::from_utf8(body).unwrap();
        assert!(
            text.contains(&format!(
                "<square150x150logo src=\"{base}/Content/Images/Icons/mstile-150x150.png\" />"
            )),
            "{text}"
        );
        assert!(!text.contains("__"), "{text}");
    }
}

/// A body over the extractors' 2 MiB is taken, as the originals take it
/// (Sonarr 4.0.20 answered 202 to a `PUT` of 120 MiB): a library import
/// or a manual import of a large folder. The ceiling is far above, and
/// stands only for a caller with the key.
#[tokio::test]
async fn a_large_body_is_taken_from_a_caller_with_the_key() {
    let send = |app: axum::Router, method: &'static str, path: &'static str, key: bool, body: Vec<u8>| async move {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if key {
            request = request.header("X-Api-Key", KEY);
        }
        app.oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap()
            .status()
    };
    let padded = |mib: usize, mut resource: serde_json::Value| {
        resource["padding"] = serde_json::Value::String("x".repeat(mib * 1024 * 1024));
        resource.to_string().into_bytes()
    };
    for personality in [Personality::Sonarr, Personality::Radarr] {
        let app = app_for(personality, "");
        let (_, _, ui) = ask(&app, "GET", "/api/v3/config/ui", &[("X-Api-Key", KEY)], "").await;
        let ui: serde_json::Value = serde_json::from_slice(&ui).unwrap();
        // A `POST`, whose body only the handler reads: a command of 3 MiB
        // is refused for what it says, not for its size.
        let command = padded(3, serde_json::json!({"name": "NoSuchCommand"}));
        let status = send(app.clone(), "POST", "/api/v3/command", true, command).await;
        assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE, "{personality:?}");
        // Past the ceiling, 413; without the key, 401 and nothing read.
        let huge = padded(65, ui.clone());
        assert_eq!(
            send(app.clone(), "POST", "/api/v3/command", true, huge.clone()).await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{personality:?}"
        );
        assert_eq!(
            send(app.clone(), "PUT", "/api/v3/config/ui/1", false, huge).await,
            StatusCode::UNAUTHORIZED,
            "{personality:?}"
        );
    }
    // A `PUT`, whose body is read before the route for the id it carries.
    let app = app();
    let (_, _, ui) = ask(&app, "GET", "/api/v3/config/ui", &[("X-Api-Key", KEY)], "").await;
    let ui: serde_json::Value = serde_json::from_slice(&ui).unwrap();
    assert_eq!(
        send(
            app.clone(),
            "PUT",
            "/api/v3/config/ui/1",
            true,
            padded(3, ui.clone())
        )
        .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send(app.clone(), "PUT", "/api/v3/config/ui/1", true, padded(65, ui)).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // The login form is anyone's to send: it keeps the small limit.
    let form = format!("username=u&password={}", "x".repeat(3 * 1024 * 1024));
    let request = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form))
        .unwrap();
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

/// A client that refuses the answer as it is still gets it, as from the
/// originals: never a 406 after the handler has done its work.
#[tokio::test]
async fn a_refused_encoding_is_no_reason_to_refuse_the_answer() {
    let app = app();
    for encoding in ["identity;q=0", "*;q=0", "identity;q=0, br;q=0"] {
        let (status, _, body) = ask(&app, "GET", "/ping", &[("Accept-Encoding", encoding)], "").await;
        assert_eq!(status, StatusCode::OK, "{encoding}");
        assert_eq!(body, b"{\"status\":\"OK\"}", "{encoding}");
        let (status, _, _) = ask(
            &app,
            "GET",
            "/api/v3/system/status",
            &[("Accept-Encoding", encoding)],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{encoding}");
    }
    // What it does take is still used.
    let (status, headers, _) = ask(
        &app,
        "GET",
        "/api/v3/system/status",
        &[("X-Api-Key", KEY), ("Accept-Encoding", "gzip, identity;q=0")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-encoding"), Some("gzip"));
}

/// A body that is not JSON is answered the same with a slash after the
/// path as without.
#[tokio::test]
async fn a_slash_after_the_path_does_not_change_how_a_bad_body_is_answered() {
    for personality in [Personality::Sonarr, Personality::Radarr] {
        let app = app_for(personality, "");
        let json = [("X-Api-Key", KEY), ("Content-Type", "application/json")];
        let plain = ask(&app, "POST", "/api/v3/command", &json, "{not json").await;
        let slashed = ask(&app, "POST", "/api/v3/command/", &json, "{not json").await;
        assert_eq!(plain.0, StatusCode::BAD_REQUEST);
        assert_eq!(slashed.0, plain.0, "{personality:?}");
        assert_eq!(
            header(&slashed.1, "content-type"),
            header(&plain.1, "content-type"),
            "{personality:?}"
        );
        assert_eq!(
            header(&plain.1, "content-type"),
            Some("application/json; charset=utf-8")
        );
    }
}

/// What the audit measured on Radarr 6.4.4 where it differs from Sonarr,
/// and the rows both share that only this test holds in place.
#[tokio::test]
async fn radarr_answers_outside_the_body_as_measured() {
    let app = app_for(Personality::Radarr, "");
    let key = [("X-Api-Key", KEY)];

    // An id that names nothing: the framework's problem details.
    let (status, headers, body) = ask(&app, "GET", "/api/v3/movie/999999", &key, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/problem+json; charset=utf-8")
    );
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["status"], 404);
    assert_eq!(problem["title"], "Not Found");
    assert_eq!(header(&headers, "x-application-version"), Some("6.4.4.10685"));

    // No body where one is needed: the error is under the empty key.
    let json = [("X-Api-Key", KEY), ("Content-Type", "application/json")];
    let (status, headers, body) = ask(&app, "POST", "/api/v3/command", &json, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/json; charset=utf-8")
    );
    let errors: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(errors["errors"][""][0], "A non-empty request body is required.");

    // A cover with the key: kept for a year, as the static files are.
    let (status, headers, _) = ask(&app, "GET", "/api/v3/mediacover/6/poster.jpg", &key, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), Some("image/jpeg"));
    assert_eq!(
        header(&headers, "cache-control"),
        Some("max-age=31536000, public")
    );
    assert!(header(&headers, "last-modified").is_some());
    assert_eq!(header(&headers, "pragma"), None);

    // The feed takes GET alone.
    let feed = format!("/feed/v3/calendar/Radarr.ics?apikey={KEY}");
    let (status, headers, body) = ask(&app, "HEAD", &feed, &[], "").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(header(&headers, "allow"), Some("GET"));
    assert!(body.is_empty());
    let (status, headers, _) = ask(&app, "GET", &feed, &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), Some("text/calendar"));
    assert_eq!(header(&headers, "x-application-version"), None);

    // The login redirect names the host asked; what a client that is no
    // trusted proxy says it forwards is not believed.
    let (status, headers, _) = ask(
        &app,
        "GET",
        "/settings",
        &[
            ("Host", "radarr:7878"),
            ("X-Forwarded-Proto", "https"),
            ("X-Forwarded-Host", "movies.example.invalid"),
        ],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        header(&headers, "location"),
        Some("http://radarr:7878/login?returnUrl=%2Fsettings")
    );

    // From a proxy that is trusted (the loopback is), it is.
    let peer: std::net::SocketAddr = "127.0.0.1:40000".parse().unwrap();
    let request = Request::builder()
        .method("GET")
        .uri("/settings")
        .header("Host", "radarr:7878")
        .header("X-Forwarded-For", "203.0.113.7")
        .header("X-Forwarded-Proto", "https")
        .header("X-Forwarded-Host", "movies.example.invalid")
        .extension(axum::extract::ConnectInfo(peer))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        header(response.headers(), "location"),
        Some("https://movies.example.invalid/login?returnUrl=%2Fsettings")
    );
}
