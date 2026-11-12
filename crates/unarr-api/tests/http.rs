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
    if let Some(name) = instance_name {
        host.instance_name = Some(name.to_string());
    }
    let mut runtime = RuntimeInfo::detect(dir);
    runtime.media_cover_dir = covers_dir(personality);
    let state = AppState::new(
        personality,
        db,
        Settings::from_host_config(host, personality),
        runtime,
        LogBuffer::new(),
    );
    unarr_api::router(state)
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
#[tokio::test]
async fn the_root_under_a_url_base_takes_a_trailing_slash() {
    let app = app_for(Personality::Sonarr, "/sonarr");
    for path in ["/sonarr", "/sonarr/"] {
        let (status, headers, _) = ask(&app, "GET", path, &[], "").await;
        assert_eq!(status, StatusCode::FOUND, "{path}");
        assert_eq!(
            header(&headers, "location"),
            Some("/sonarr/login?returnUrl=%2F"),
            "{path}"
        );
    }
    for path in ["/sonarr/login", "/sonarr/login/"] {
        assert_eq!(ask(&app, "GET", path, &[], "").await.0, StatusCode::OK, "{path}");
    }
    // Outside the base there is nothing.
    assert_eq!(ask(&app, "GET", "/", &[], "").await.0, StatusCode::NOT_FOUND);
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
