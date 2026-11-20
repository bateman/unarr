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
async fn json_and_pages_are_compressed_as_the_originals_compress() {
    let app = app();
    // Brotli when the client weighs it as gzip (recorded with
    // `gzip, br`), gzip when it weighs gzip more; deflate is not offered.
    let both = [("X-Api-Key", KEY), ("Accept-Encoding", "gzip, deflate, br")];
    let (status, headers, _) = ask(&app, "GET", "/api/v3/series", &both, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-encoding"), Some("br"));
    assert_eq!(header(&headers, "vary"), Some("Accept-Encoding"));
    let gzip = [("X-Api-Key", KEY), ("Accept-Encoding", "br;q=0.5, gzip")];
    let (_, headers, body) = ask(&app, "GET", "/api/v3/series", &gzip, "").await;
    assert_eq!(header(&headers, "content-encoding"), Some("gzip"));
    assert_eq!(&body[..2], &[0x1f, 0x8b]);
    let deflate = [("X-Api-Key", KEY), ("Accept-Encoding", "deflate")];
    let (_, headers, body) = ask(&app, "GET", "/api/v3/series", &deflate, "").await;
    assert_eq!(header(&headers, "content-encoding"), None);
    assert_eq!(header(&headers, "vary"), Some("Accept-Encoding"));
    assert_eq!(body[0], b'[');
    // A static file of a type that is compressed, and one that is not.
    let (_, headers, _) = ask(
        &app,
        "GET",
        "/Content/styles.css",
        &[("Accept-Encoding", "br")],
        "",
    )
    .await;
    assert_eq!(header(&headers, "content-encoding"), Some("br"));
    let png = "/Content/Images/Icons/favicon-32x32.png";
    let (_, headers, _) = ask(&app, "GET", png, &[("Accept-Encoding", "gzip, br")], "").await;
    assert_eq!(header(&headers, "content-encoding"), None);
    assert_eq!(header(&headers, "vary"), None);

    // Not asked for: as it is, and no `Vary` at all.
    let (_, headers, body) = ask(&app, "GET", "/api/v3/series", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(header(&headers, "content-encoding"), None);
    assert_eq!(header(&headers, "vary"), None);
    assert_eq!(body[0], b'[');

    // Over HTTPS, as a proxy on the loopback says: not compressed.
    let https = [
        ("X-Api-Key", KEY),
        ("Accept-Encoding", "gzip, br"),
        ("X-Forwarded-Proto", "https"),
    ];
    let request = Request::builder().uri("/api/v3/series");
    let request = https.iter().fold(request, |r, (k, v)| r.header(*k, *v));
    let mut request = request.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(header(response.headers(), "content-encoding"), None);
    assert_eq!(header(response.headers(), "vary"), None);

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

/// Radarr 6.4.4 has no Basic authentication: with `Basic` in `config.xml`
/// it shows its login page, says `forms` in `config/host`, and credentials
/// sent the Basic way open nothing and are not logged.
#[tokio::test]
async fn radarr_takes_basic_for_forms() {
    use base64::Engine;
    use tracing_subscriber::layer::SubscriberExt;
    let logs = LogBuffer::new();
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
    let app = app_edited(Personality::Radarr, "", |host| {
        host.authentication_method = unarr_core::config::AuthenticationMethod::Basic
    });
    let credentials = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("unarr:fixture")
    );
    for headers in [&[][..], &[("Authorization", credentials.as_str())][..]] {
        let (status, answer, _) = ask(&app, "GET", "/", headers, "").await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(header(&answer, "location"), Some("/login?returnUrl=%2F"));
        assert_eq!(header(&answer, "www-authenticate"), None);
    }
    let (status, _, body) = ask(&app, "GET", "/api/v3/config/host", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["authenticationMethod"], "forms");
    assert!(logs.entries().iter().all(|e| e.logger != "Auth"));
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
        // Row 1.35: what the originals generate once keeps the base the
        // routes have, the page and the session cookie's path; what they
        // read on each request, the bootstrap, says the new one.
        let (_, _, page) = ask(&app, "GET", "/sonarr/login", &[], "").await;
        let page = String::from_utf8(page).unwrap();
        assert!(page.contains("href=\"/sonarr/Content/styles.css?h="), "{saved:?}");
        let form = [("Content-Type", "application/x-www-form-urlencoded")];
        let (_, headers, _) = ask(
            &app,
            "POST",
            "/sonarr/login",
            &form,
            "username=unarr&password=fixture",
        )
        .await;
        let cookie = header(&headers, "set-cookie").unwrap();
        assert!(cookie.contains("; Path=/sonarr;"), "{cookie}");
        let session = cookie.split(';').next().unwrap();
        let (_, _, body) = ask(&app, "GET", "/sonarr/initialize.json", &[("Cookie", session)], "").await;
        let bootstrap: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(bootstrap["urlBase"], saved, "{saved:?}");
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

// --- The low rows of `docs/audit/2-http.md`, as recorded on Sonarr 4.0.20
// and Radarr 6.4.4 with `scripts/http-probe.py` (`lowhttp*`).

/// A session cookie, from the login form.
async fn session(app: &axum::Router) -> String {
    let form = [("Content-Type", "application/x-www-form-urlencoded")];
    let (status, headers, _) = ask(app, "POST", "/login", &form, "username=unarr&password=fixture").await;
    assert_eq!(status, StatusCode::FOUND);
    header(&headers, "set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// The hashed name of a file of the embedded bundle.
fn bundle_file(personality: Personality, prefix: &str, suffix: &str) -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join(personality.app_name().to_lowercase());
    std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .find(|name| name.starts_with(prefix) && name.ends_with(suffix))
        .unwrap_or_else(|| panic!("no {prefix}*{suffix} in {}", dir.display()))
}

fn without_auth(personality: Personality, url_base: &str) -> axum::Router {
    app_edited(personality, url_base, |host| {
        host.authentication_method = unarr_core::config::AuthenticationMethod::None;
    })
}

const BUILT_SONARR: &str = "Wed, 16 Sep 2026 16:37:42 GMT";
const BUILT_RADARR: &str = "Mon, 14 Sep 2026 15:54:37 GMT";

/// Rows 4.9, 4.19: every file is kept a year and dated with the build of
/// the release whose UI is embedded, unless `CacheableSpecification` says
/// otherwise; its type is ASP.NET's for the extension.
#[tokio::test]
async fn static_files_are_kept_a_year_and_typed_as_aspnet_types_them() {
    for (personality, built, script) in [
        (Personality::Sonarr, BUILT_SONARR, "application/javascript"),
        (Personality::Radarr, BUILT_RADARR, "text/javascript"),
    ] {
        let app = without_auth(personality, "");
        let index = format!("/{}", bundle_file(personality, "index-", ".js"));
        for (path, content_type) in [
            ("/Content/styles.css", "text/css"),
            ("/Content/Fonts/fonts.css", "text/css"),
            ("/Content/Fonts/Roboto-Light.woff2", "font/woff2"),
            ("/Content/Fonts/Roboto-Light.woff", "application/font-woff"),
            ("/Content/Fonts/Roboto-Light.ttf", "application/x-font-ttf"),
            (
                "/Content/Fonts/UbuntuMono-Regular.eot",
                "application/vnd.ms-fontobject",
            ),
            ("/Content/Images/Icons/favicon-32x32.png", "image/png"),
            ("/Content/Images/Icons/safari-pinned-tab.svg", "image/svg+xml"),
            ("/Content/browserconfig.xml", "text/xml"),
            ("/Content/manifest.json", "application/json"),
            ("/Content/robots.txt", "text/plain; charset=utf-8"),
            ("/favicon.ico", "image/x-icon"),
            (index.as_str(), script),
            ("/Content/styles.css?h=abc", "text/css"),
        ] {
            let (status, headers, _) = ask(&app, "GET", path, &[], "").await;
            assert_eq!(status, StatusCode::OK, "{personality:?} {path}");
            assert_eq!(header(&headers, "content-type"), Some(content_type), "{path}");
            assert_eq!(
                header(&headers, "cache-control"),
                Some("max-age=31536000, public"),
                "{path}"
            );
            assert_eq!(header(&headers, "last-modified"), Some(built), "{path}");
            assert_eq!(header(&headers, "pragma"), None, "{path}");
        }
        // Pages are never kept, `oauth.html` among them; `?h=` on the API
        // makes its answer one to keep.
        for path in ["/", "/login", "/oauth.html", "/VERSION"] {
            let (status, headers, _) = ask(&app, "GET", path, &[], "").await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(header(&headers, "content-type"), Some("text/html"), "{path}");
            assert_eq!(
                header(&headers, "cache-control"),
                Some("no-cache, no-store"),
                "{path}"
            );
            assert_eq!(header(&headers, "last-modified"), None, "{path}");
        }
        let (_, headers, _) = ask(
            &app,
            "GET",
            "/api/v3/system/status?h=1",
            &[("X-Api-Key", KEY)],
            "",
        )
        .await;
        assert_eq!(
            header(&headers, "cache-control"),
            Some("max-age=31536000, public")
        );
        assert_eq!(header(&headers, "last-modified"), Some(built));
        // Health checks ask `/ping` in this case: left uncached by choice.
        let (_, headers, _) = ask(&app, "GET", "/ping", &[], "").await;
        assert_eq!(header(&headers, "cache-control"), None);
        let (_, headers, _) = ask(&app, "GET", "/Ping", &[], "").await;
        assert_eq!(
            header(&headers, "cache-control"),
            Some("max-age=31536000, public")
        );
    }
}

/// Rows 4.16, 4.20, 4.7: what no mapper takes, or a file that is not
/// there, is the controller's 404 in problem details, kept a year like
/// the rest; `/robots.txt` is `Content/robots.txt`.
#[tokio::test]
async fn what_no_mapper_takes_is_a_404_in_problem_details() {
    for (personality, document) in [
        (
            Personality::Sonarr,
            "https://tools.ietf.org/html/rfc7231#section-6.5.4",
        ),
        (
            Personality::Radarr,
            "https://tools.ietf.org/html/rfc9110#section-15.5.5",
        ),
    ] {
        let app = without_auth(personality, "");
        let licence = format!("/{}", bundle_file(personality, "", ".LICENSE.txt"));
        for path in [
            "/index.html",
            licence.as_str(),
            "/nope.png",
            "/nope.js",
            "/nope/x.png",
            "/Content/nope.js",
            "/Content/index.html",
            "/initialize.js",
            "/Content/../index.html",
            "/MediaCover/6/fanart.jpg",
            "/mediacover/6/poster.jpg",
            "/MediaCoverProxy/0123456789abcdef/poster.txt",
        ] {
            let (status, headers, body) = ask(&app, "GET", path, &[], "").await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{personality:?} {path}");
            assert_eq!(
                header(&headers, "content-type"),
                Some("application/problem+json; charset=utf-8"),
                "{path}"
            );
            assert_eq!(
                header(&headers, "cache-control"),
                Some("max-age=31536000, public")
            );
            let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(problem["type"], document);
            assert_eq!(problem["status"], 404);
            assert!(problem["traceId"].as_str().unwrap().starts_with("00-"));
            assert!(body.starts_with(b"{\n  \"type\""), "indented: {path}");
        }
        let (status, headers, body) = ask(&app, "GET", "/robots.txt", &[], "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            header(&headers, "content-type"),
            Some("text/plain; charset=utf-8")
        );
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("ui")
            .join(personality.app_name().to_lowercase())
            .join("Content/robots.txt");
        assert_eq!(body, std::fs::read(file).unwrap());
        assert!(body.ends_with(b"Disallow: /"));
        // Under Forms it is behind the login, as the rest is.
        let (status, _, _) = ask(&app_for(personality, ""), "GET", "/robots.txt", &[], "").await;
        assert_eq!(status, StatusCode::FOUND);
    }
}

/// Row 4.11: a cover is a static file: kept a year, dated with the build,
/// typed by its name, without the ranges and validators of a file server.
#[tokio::test]
async fn covers_are_static_files() {
    let app = without_auth(Personality::Sonarr, "");
    let (status, headers, body) = ask(&app, "GET", "/MediaCover/6/poster.jpg?lastWrite=1", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"\xff\xd8\xff\xd9");
    assert_eq!(header(&headers, "content-type"), Some("image/jpeg"));
    assert_eq!(
        header(&headers, "cache-control"),
        Some("max-age=31536000, public")
    );
    assert_eq!(header(&headers, "last-modified"), Some(BUILT_SONARR));
    assert_eq!(header(&headers, "etag"), None);
    assert_eq!(header(&headers, "accept-ranges"), None);
    // A resized one that was not made is the full one.
    let (status, _, body) = ask(&app, "GET", "/MediaCover/6/poster-500.jpg", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"\xff\xd8\xff\xd9");
}

/// Row 4.3: the pages' asset links carry the URL base and `?h=` with the
/// MD5 of the file; `data-no-hash` keeps none and goes.
#[tokio::test]
async fn the_pages_asset_links_carry_cache_breakers() {
    use base64::Engine;
    use md5::Digest;
    let styles = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/ui/sonarr/Content/styles.css"
    ))
    .unwrap();
    let hash = base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(&styles));
    let hash = hash.trim_end_matches('=');
    let index = bundle_file(Personality::Sonarr, "index-", ".js");
    for base in ["", "/tv"] {
        let app = without_auth(Personality::Sonarr, base);
        let (_, _, body) = ask(&app, "GET", &format!("{base}/"), &[], "").await;
        let page = String::from_utf8(body).unwrap();
        assert!(
            page.contains(&format!("href=\"{base}/Content/styles.css?h={hash}\"")),
            "{page}"
        );
        assert!(
            page.contains(&format!("<script src=\"{base}/{index}\"></script>")),
            "{page}"
        );
        assert!(!page.contains("data-no-hash"), "{page}");
        let (_, _, body) = ask(&app, "GET", &format!("{base}/login"), &[], "").await;
        let page = String::from_utf8(body).unwrap();
        assert!(
            page.contains(&format!("href=\"{base}/Content/styles.css?h={hash}\"")),
            "{page}"
        );
        assert!(page.contains(&format!("href=\"{base}/favicon.ico?h=")), "{page}");
    }
}

/// Row 1.32: `HEAD` finds an action on `/ping` alone; elsewhere it is the
/// framework's 405 endpoint, which asks for the key, under any method.
#[tokio::test]
async fn head_finds_no_action_but_on_ping() {
    for app in [app(), without_auth(Personality::Sonarr, "")] {
        for path in [
            "/Content/manifest.json",
            "/Content/styles.css",
            "/login",
            "/",
            "/MediaCover/6/poster.jpg",
        ] {
            let (status, headers, _) = ask(&app, "HEAD", path, &[], "").await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(header(&headers, "location"), None);
        }
        let (status, headers, _) =
            ask(&app, "HEAD", "/Content/manifest.json", &[("X-Api-Key", KEY)], "").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(header(&headers, "allow"), Some("GET"));
        assert_eq!(
            header(&headers, "cache-control"),
            Some("max-age=31536000, public")
        );
        let (status, _, _) = ask(&app, "HEAD", "/ping", &[], "").await;
        assert_eq!(status, StatusCode::OK);
    }
    // A cookie does not open the 405 endpoint.
    let app = app();
    let cookie = session(&app).await;
    let (status, _, _) = ask(
        &app,
        "HEAD",
        "/MediaCover/6/poster.jpg",
        &[("Cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Row 1.18: `/content` takes the `AllowGet` policy: any origin may read
/// it, and a preflight for `GET` is answered without a session.
#[tokio::test]
async fn content_may_be_read_from_any_origin() {
    let app = app();
    let origin = ("Origin", "http://other.example");
    let (status, headers, _) = ask(&app, "GET", "/Content/manifest.json", &[origin], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "access-control-allow-origin"), Some("*"));
    let (status, headers, _) = ask(&app, "GET", "/Content/nope.png", &[origin], "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header(&headers, "access-control-allow-origin"), Some("*"));
    for path in ["/Content/manifest.json", "/CONTENT/manifest.json"] {
        let preflight = [
            origin,
            ("Access-Control-Request-Method", "GET"),
            ("Access-Control-Request-Headers", "X-Custom, Content-Type"),
        ];
        let (status, headers, _) = ask(&app, "OPTIONS", path, &preflight, "").await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{path}");
        assert_eq!(header(&headers, "access-control-allow-origin"), Some("*"));
        assert_eq!(
            header(&headers, "access-control-allow-methods"),
            Some("GET,OPTIONS")
        );
        assert_eq!(
            header(&headers, "access-control-allow-headers"),
            Some("X-Custom,Content-Type")
        );
    }
    // Not for another method, nor without an origin, nor elsewhere.
    let post = [origin, ("Access-Control-Request-Method", "POST")];
    assert_eq!(
        ask(&app, "OPTIONS", "/Content/manifest.json", &post, "").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ask(&app, "OPTIONS", "/Content/manifest.json", &[], "").await.0,
        StatusCode::UNAUTHORIZED
    );
    let get = [origin, ("Access-Control-Request-Method", "GET")];
    assert_eq!(
        ask(&app, "OPTIONS", "/", &get, "").await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, headers, _) = ask(&app, "GET", "/Content/manifest.json", &[], "").await;
    assert_eq!(header(&headers, "access-control-allow-origin"), None);
}

/// Row 1.30: `ReturnHttpNotAcceptable`. The action runs, then a JSON result
/// the client does not take is an empty 406; `*/*` anywhere in `Accept`
/// makes it irrelevant. Radarr's formatter writes `text/json` too.
#[tokio::test]
async fn an_accept_without_json_is_406_once_the_action_has_run() {
    let app = app();
    let status_with = |accept: &'static str| {
        let app = app.clone();
        async move {
            ask(
                &app,
                "GET",
                "/api/v3/system/status",
                &[("X-Api-Key", KEY), ("Accept", accept)],
                "",
            )
            .await
        }
    };
    for accept in [
        "text/html",
        "application/xml",
        "text/plain",
        "text/*",
        "text/json",
        "application/problem+json",
    ] {
        let (status, headers, body) = status_with(accept).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE, "{accept}");
        assert!(body.is_empty());
        assert_eq!(header(&headers, "content-type"), None);
        assert_eq!(header(&headers, "cache-control"), Some("no-cache, no-store"));
        assert_eq!(header(&headers, "x-application-version"), Some("4.0.20.3014"));
    }
    for accept in [
        "*/*",
        "application/*",
        "text/html, */*;q=0.1",
        "",
        "application/json",
    ] {
        let (status, headers, _) = status_with(accept).await;
        assert_eq!(status, StatusCode::OK, "{accept}");
        assert_eq!(
            header(&headers, "content-type"),
            Some("application/json; charset=utf-8")
        );
    }
    // A write is done all the same, and named nowhere.
    let html = [
        ("X-Api-Key", KEY),
        ("Accept", "text/html"),
        ("Content-Type", "application/json"),
    ];
    let (status, headers, _) = ask(&app, "POST", "/api/v3/tag", &html, r#"{"label": "probe406"}"#).await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    assert_eq!(header(&headers, "location"), None);
    let (_, _, body) = ask(&app, "GET", "/api/v3/tag", &[("X-Api-Key", KEY)], "").await;
    assert!(String::from_utf8(body).unwrap().contains("probe406"));
    // A 404 of the action is negotiated; `/ping` too; the bootstrap is not.
    let (status, _, _) = ask(
        &app,
        "GET",
        "/api/v3/series/999999",
        &[("X-Api-Key", KEY), ("Accept", "text/html")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        ask(&app, "GET", "/ping", &[("Accept", "text/html")], "").await.0,
        StatusCode::NOT_ACCEPTABLE
    );

    let radarr = app_for(Personality::Radarr, "");
    for accept in ["text/json", "text/*"] {
        let (status, headers, _) = ask(
            &radarr,
            "GET",
            "/api/v3/system/status",
            &[("X-Api-Key", KEY), ("Accept", accept)],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{accept}");
        assert_eq!(header(&headers, "content-type"), Some("text/json; charset=utf-8"));
    }
    let (status, _, _) = ask(
        &radarr,
        "GET",
        "/api/v3/system/status",
        &[("X-Api-Key", KEY), ("Accept", "text/html")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

/// Row 1.15: the routes outside the API are matched without case.
#[tokio::test]
async fn routes_outside_the_api_are_matched_without_case() {
    let app = app();
    let (status, headers, _) = ask(&app, "GET", "/LOGIN", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), Some("text/html"));
    assert_eq!(ask(&app, "GET", "/Ping", &[], "").await.0, StatusCode::OK);
    let (status, headers, _) = ask(&app, "GET", "/LOGOUT", &[], "").await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(header(&headers, "location"), Some("/"));
    let negotiate = format!("/SIGNALR/messages/negotiate?access_token={KEY}");
    assert_eq!(ask(&app, "POST", &negotiate, &[], "").await.0, StatusCode::OK);
    assert_eq!(
        ask(&app, "POST", "/SignalR/messages/negotiate", &[], "").await.0,
        StatusCode::UNAUTHORIZED
    );
    let feed = format!("/FEED/v3/calendar/sonarr.ICS?apikey={KEY}");
    let (status, headers, _) = ask(&app, "GET", &feed, &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), Some("text/calendar"));
    assert_eq!(
        ask(&app, "GET", "/CONTENT/manifest.json", &[], "").await.0,
        StatusCode::OK
    );
    // What follows `/content/` keeps its case: a file in another is not there.
    assert_eq!(
        ask(&app, "GET", "/content/STYLES.CSS", &[], "").await.0,
        StatusCode::NOT_FOUND
    );
    let (status, _, _) = ask(
        &app,
        "POST",
        "/LOGIN",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "username=unarr&password=fixture",
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
}

/// Row 3.9: an XHR is told 401, the login page still named; the header and
/// the query parameter count, in that case only.
#[tokio::test]
async fn an_xhr_is_told_401_instead_of_the_login_redirect() {
    let app = app();
    let (status, headers, _) = ask(
        &app,
        "GET",
        "/series",
        &[("X-Requested-With", "XMLHttpRequest")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(header(&headers, "location"), Some("/login?returnUrl=%2Fseries"));
    let (status, headers, _) = ask(&app, "GET", "/?X-Requested-With=XMLHttpRequest", &[], "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        header(&headers, "location"),
        Some("/login?returnUrl=%2F%3FX-Requested-With%3DXMLHttpRequest")
    );
    let (status, _, _) = ask(&app, "GET", "/", &[("X-Requested-With", "xmlhttprequest")], "").await;
    assert_eq!(status, StatusCode::FOUND);
}

/// Row 3.8: with no authentication, the form signs nobody in and says
/// nothing in the log; a logout says nothing either.
#[tokio::test]
async fn the_login_form_fails_when_nothing_is_to_be_signed_into() {
    use tracing_subscriber::layer::SubscriberExt;
    let logs = LogBuffer::new();
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
    let app = without_auth(Personality::Sonarr, "");
    let form = [("Content-Type", "application/x-www-form-urlencoded")];
    for body in ["username=unarr&password=fixture", "username=unarr&password=nope"] {
        let (status, headers, _) = ask(&app, "POST", "/login", &form, body).await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            header(&headers, "location"),
            Some("/login?returnUrl=&loginFailed=true")
        );
        assert_eq!(header(&headers, "set-cookie"), None);
        assert_eq!(
            header(&headers, "cache-control"),
            Some("max-age=31536000, public")
        );
    }
    let (status, headers, _) = ask(&app, "GET", "/logout", &[], "").await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(header(&headers, "cache-control"), Some("no-cache,no-store"));
    assert_eq!(header(&headers, "expires"), Some("Thu, 01 Jan 1970 00:00:00 GMT"));
    assert_eq!(header(&headers, "last-modified"), Some(BUILT_SONARR));
    assert!(
        logs.entries().iter().all(|e| e.logger != "Auth"),
        "nothing about signing in"
    );
}

/// Row 3.5: the cookie's name loses the instance name's diacritics before
/// what is not a letter or a digit.
#[tokio::test]
async fn the_cookie_name_keeps_the_letters_under_the_diacritics() {
    let app = app_named(Personality::Sonarr, "", Some("Sonarr Ünï Çafé"));
    let cookie = session(&app).await;
    assert!(cookie.starts_with("SonarrUniCafeAuth="), "{cookie}");
}

/// Row 1.21: until the command runtime has started, what is authenticated
/// is answered 503, in JSON under `/api`, in plain text elsewhere.
#[tokio::test]
async fn while_starting_an_authenticated_request_is_503() {
    let (app, state) = app_and_state(Personality::Sonarr, "", |_| {});
    state.set_starting(true);
    let (status, headers, body) = ask(&app, "GET", "/api/v3/system/status", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header(&headers, "content-type"), Some("application/json"));
    assert_eq!(header(&headers, "x-application-version"), Some("4.0.20.3014"));
    assert_eq!(
        String::from_utf8(body).unwrap(),
        "{\n  \"errorMessage\": \"Sonarr is starting up, please try again later\"\n}"
    );
    // The key is asked for first.
    assert_eq!(
        ask(&app, "GET", "/api/v3/system/status", &[], "").await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, headers, body) = ask(&app, "GET", "/login", &[], "").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header(&headers, "content-type"), Some("text/plain"));
    assert_eq!(body, b"Sonarr is starting up, please try again later");
    assert_eq!(ask(&app, "GET", "/series", &[], "").await.0, StatusCode::FOUND);
    state.set_starting(false);
    assert_eq!(
        ask(&app, "GET", "/api/v3/system/status", &[("X-Api-Key", KEY)], "")
            .await
            .0,
        StatusCode::OK
    );
}

/// Row 1.7: `LoggingMiddleware`'s lines, the key cleansed by the targets.
#[tokio::test]
async fn each_request_is_logged_as_the_originals_log_it() {
    use tracing_subscriber::layer::SubscriberExt;
    let logs = LogBuffer::new();
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
    let app = app();
    let mut request = Request::builder()
        .uri("/api/v3/series/999999?marker=probe")
        .header("X-Api-Key", KEY)
        .header("User-Agent", "probe-agent/1.0")
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))))
        .body(Body::empty())
        .unwrap();
    request
        .headers_mut()
        .insert("X-Forwarded-For", "10.1.2.3".parse().unwrap());
    app.clone().oneshot(request).await.unwrap();
    ask(&app, "GET", "/unarr-page", &[], "").await;
    let lines: Vec<String> = logs
        .entries()
        .into_iter()
        .rev()
        .filter(|e| e.logger == "Http" || e.logger == "Api")
        .map(|e| format!("{}|{}|{}", e.level, e.logger, e.message))
        .map(|line| {
            line.split(" (").next().unwrap().to_owned()
                + if line.contains(" (from ") { " (from)" } else { "" }
        })
        .collect();
    let numbers: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split_once("Req: ").or_else(|| l.split_once("Res: ")))
        .filter_map(|(_, rest)| rest.split(' ').next())
        .collect();
    assert_eq!(numbers[0], numbers[1], "{lines:?}");
    let first: u64 = numbers[0].parse().unwrap();
    let second = first + 1;
    assert_eq!(
        lines,
        [
            format!("trace|Http|Req: {first} [GET] /api/v3/series/999999?marker=probe (from)"),
            format!("trace|Http|Res: {first} [GET] /api/v3/series/999999?marker=probe: 404.NotFound"),
            "debug|Api|[GET] /api/v3/series/999999?marker=probe: 404.NotFound".to_owned(),
            format!("trace|Http|Req: {second} [GET] /unarr-page (from)"),
            format!("trace|Http|Res: {second} [GET] /unarr-page: 302.Found"),
        ]
    );
    let from = logs
        .entries()
        .into_iter()
        .find(|e| e.logger == "Http" && e.message.contains("999999") && e.message.starts_with("Req"))
        .unwrap()
        .message;
    assert!(from.ends_with("(from 10.1.2.3 probe-agent/1.0)"), "{from}");
}

/// Rows 2.8, 2.9: the cover proxy's name says the type, and a name that is
/// not an image's is a 404 before the hash is looked at.
#[tokio::test]
async fn the_cover_proxy_reads_the_name_first() {
    let app = without_auth(Personality::Sonarr, "");
    let (status, headers, body) = ask(
        &app,
        "GET",
        "/MediaCoverProxy/0123456789abcdef/poster.jpg",
        &[],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header(&headers, "cache-control"), Some("no-cache,no-store"));
    let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["message"], "Url no longer in cache");
    for name in ["poster.txt", "poster.JPG", ".jpg"] {
        let path = format!("/MediaCoverProxy/0123456789abcdef/{name}");
        assert_eq!(
            ask(&app, "GET", &path, &[], "").await.0,
            StatusCode::NOT_FOUND,
            "{name}"
        );
    }
}

/// Row 1.34: `Server: Kestrel` on every answer, as both originals send it:
/// the API, a page, a static file, a refused key, a refused host.
#[tokio::test]
async fn every_answer_names_kestrel_as_the_originals_do() {
    let app = app();
    for (path, headers) in [
        ("/api/v3/system/status", vec![("X-Api-Key", KEY)]),
        ("/api/v3/system/status", vec![]),
        ("/series", vec![]),
        ("/Content/styles.css", vec![]),
        ("/ping", vec![]),
    ] {
        let (_, answer, _) = ask(&app, "GET", path, &headers, "").await;
        assert_eq!(header(&answer, "server"), Some("Kestrel"), "{path}");
    }
    let app = app_edited(Personality::Sonarr, "", |host| {
        host.allowed_hosts = "tv.example.com".into()
    });
    let (status, answer, _) = ask(&app, "GET", "/ping", &[("Host", "evil.example")], "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(header(&answer, "server"), Some("Kestrel"));
}

/// A cover is read from the cover folder and nowhere else, however the
/// path escapes its way out.
#[tokio::test]
async fn covers_never_leave_their_folder() {
    let app = without_auth(Personality::Sonarr, "");
    let secret = format!("unarr_cover_escape_{}.txt", std::process::id());
    let outside = covers_dir(Personality::Sonarr).parent().unwrap().join(&secret);
    std::fs::write(&outside, b"secret").unwrap();
    for path in [
        format!("/MediaCover/%2e%2e/{secret}"),
        format!("/MediaCover/%2E%2E%2F{secret}"),
        format!("/MediaCover/6/..%2f..%2f{secret}"),
        format!("/MediaCover/6/%2e%2e/%2e%2e/{secret}"),
        format!("/MediaCover/6%5c..%5c..%5c{secret}"),
        "/MediaCover/%2fetc%2fpasswd".to_owned(),
        "/MediaCover/%252e%252e/x".to_owned(),
        "/MediaCover/6/poster.jpg%00.png".to_owned(),
    ] {
        let (status, _, body) = ask(&app, "GET", &path, &[], "").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(!body.windows(6).any(|w| w == b"secret"), "{path}");
    }
    std::fs::remove_file(outside).unwrap();
}

/// `UserService.FindUser`: a blank name or password finds nobody, even a
/// user stored blank; a `config.xml` with both elements empty adds none.
#[tokio::test]
async fn blank_credentials_let_nobody_in() {
    let (app, state) = app_and_state(Personality::Sonarr, "", |_| {});
    let blank = unarr_core::config::LegacyUser {
        username: " ".into(),
        password: String::new(),
    };
    let added = state
        .db()
        .call(move |conn| unarr_api::ui::add_legacy_user(conn, &blank))
        .await
        .unwrap();
    assert!(!added);
    // A user stored blank, as an imported database could hold one: the
    // legacy hash of an empty password.
    state
        .db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO users (id, identifier, username, password) VALUES (99, 'blank', '', ?1)",
                ["e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let form = [("Content-Type", "application/x-www-form-urlencoded")];
    let (status, headers, _) = ask(&app, "POST", "/login", &form, "username=&password=").await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(header(&headers, "location")
        .unwrap()
        .ends_with("loginFailed=true"));
    assert_eq!(header(&headers, "set-cookie"), None);

    let basic = app_edited(Personality::Sonarr, "", |host| {
        host.authentication_method = unarr_core::config::AuthenticationMethod::Basic;
    });
    let (status, _, _) = ask(&basic, "GET", "/", &[("Authorization", "Basic Og==")], "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A backup is never kept by a cache, unlike the originals' (it holds the
/// database and the key); nor is an answer of the exception handler, a
/// cache breaker in its query or not.
#[tokio::test]
async fn backups_and_failures_are_never_kept() {
    let app = without_auth(Personality::Sonarr, "");
    let (status, headers, _) = ask(&app, "GET", "/backup/manual/nope.zip", &[], "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header(&headers, "cache-control"), Some("no-cache, no-store"));
    // The fixture's tag 1 is in use: its removal is refused with an exception.
    let (status, headers, _) = ask(&app, "DELETE", "/api/v3/tag/1?h=1", &[("X-Api-Key", KEY)], "").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(header(&headers, "cache-control"), Some("no-cache,no-store"));
    assert_eq!(header(&headers, "last-modified"), None);
}
