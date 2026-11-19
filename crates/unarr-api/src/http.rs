//! What the originals answer outside the body, where both do the same: the
//! headers ASP.NET's pipeline puts on an API answer, CORS, compression, and
//! the answers of the framework itself to a request no action takes.
//!
//! Each rule here was measured on Sonarr 4.0.20 and Radarr 6.4.4 running on
//! the fixtures (`scripts/http-audit.py`, `docs/audit/2-http.md`).

use axum::extract::{OriginalUri, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use unarr_core::Personality;

use crate::error::Thrown;
use crate::state::AppState;

/// `CacheHeaderMiddleware` for what must not be cached.
const NO_CACHE: &str = "no-cache, no-store";
/// The same, as ASP.NET's exception handler spells it when it answers for
/// an exception a controller threw.
const NO_CACHE_THROWN: &str = "no-cache,no-store";
/// `CacheHeaderMiddleware` for what `CacheableSpecification` lets a client keep.
const CACHE_FOREVER: &str = "max-age=31536000, public";

/// A method no route takes. `get()` also answers `HEAD` in axum; the
/// originals' actions answer it with 405, so `HEAD` is asked under this
/// name and the router says which methods the route does take.
const NOT_HEAD: &[u8] = b"HEAD-AS-ASKED";

/// A path lower-cased and without the URL base the router was built with.
fn app_path(state: &AppState, request: &Request, path: &str) -> String {
    let path = path.to_ascii_lowercase();
    let base = crate::routed_url_base(state, request).to_ascii_lowercase();
    match path.strip_prefix(&base) {
        Some(rest) if !base.is_empty() && (rest.is_empty() || rest.starts_with('/')) => rest.to_owned(),
        _ => path,
    }
}

/// The path the client asked for: the router hands a nested route only
/// what follows its prefix.
fn asked_path(state: &AppState, request: &Request) -> String {
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map(|uri| uri.0.path())
        .unwrap_or(request.uri().path());
    app_path(state, request, path)
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// The methods of a route, as the originals count them. Outside the router,
/// which picks the handler by method before any of its layers runs and
/// writes `Allow` after the last one has.
///
/// - `HEAD` is a method like another on the API and the feed: 405 where
///   only `GET` is declared (`/ping` declares both).
/// - `Allow` goes with a 405 only, and lists what was declared.
pub async fn methods(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    if request.method() == Method::HEAD {
        let path = app_path(&state, &request, request.uri().path());
        if is_api(&path) || path.starts_with("/feed/") {
            if let Ok(asked) = Method::from_bytes(NOT_HEAD) {
                *request.method_mut() = asked;
            }
        }
    }
    let mut response = next.run(request).await;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED {
        allow_as_the_originals(response.headers_mut());
    } else {
        response.headers_mut().remove(header::ALLOW);
    }
    response
}

/// `no-cache, no-store` with the two headers older caches read.
fn forbid_caching(headers: &mut HeaderMap, cache_control: &'static str) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache_control));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::EXPIRES, HeaderValue::from_static("-1"));
}

/// MVC's JSON formatter names the charset; axum's `Json` does not.
pub fn json_charset(headers: &mut HeaderMap) {
    if headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v == "application/json")
    {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
    }
}

/// [`json_charset`] for a route outside the API (`/ping`).
pub async fn with_json_charset(mut response: Response) -> Response {
    json_charset(response.headers_mut());
    response
}

/// `Allow` as ASP.NET writes it: no `HEAD` that nobody declared, the
/// methods in alphabetical order, a comma and a space between them.
fn allow_as_the_originals(headers: &mut HeaderMap) {
    let Some(allow) = headers.get(header::ALLOW).and_then(|v| v.to_str().ok()) else {
        return;
    };
    let mut methods: Vec<&str> = allow
        .split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty() && *m != "HEAD")
        .collect();
    methods.sort_unstable();
    methods.dedup();
    if let Ok(value) = HeaderValue::from_str(&methods.join(", ")) {
        headers.insert(header::ALLOW, value);
    }
}

/// The answer of an extractor that refused the request, which is axum's
/// (`text/plain`), as the originals' framework answers the same request.
async fn framework_answer(response: Response, personality: Personality, command: bool) -> Response {
    let refused = matches!(
        response.status(),
        StatusCode::BAD_REQUEST | StatusCode::UNSUPPORTED_MEDIA_TYPE | StatusCode::UNPROCESSABLE_ENTITY
    ) && response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/plain"))
        && response.extensions().get::<Thrown>().is_none();
    if !refused {
        return response;
    }
    let status = response.status();
    let Ok(bytes) = axum::body::to_bytes(response.into_body(), 64 * 1024).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let text = String::from_utf8_lossy(&bytes);
    // An id that is not a number: `{id:int}` does not match, so no route does.
    if text.starts_with("Invalid URL") {
        return StatusCode::NOT_FOUND.into_response();
    }
    // A body that is not announced as JSON: no formatter reads it.
    if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
        return status.into_response();
    }
    // Model binding failed: the validation problem details. The parser's
    // words are serde's, not System.Text.Json's.
    let detail = text.split_once(": ").map_or(&*text, |(_, detail)| detail);
    let empty = detail.starts_with("EOF while parsing a value at line 1 column 0");
    let (document, errors) = match personality {
        Personality::Radarr if empty => (
            crate::error::BAD_REQUEST_TYPE_RFC9110,
            json!({"": ["A non-empty request body is required."]}),
        ),
        Personality::Radarr => (crate::error::BAD_REQUEST_TYPE_RFC9110, json!({"$": [detail]})),
        Personality::Sonarr | Personality::Lidarr => {
            (crate::error::BAD_REQUEST_TYPE_RFC7231, json!({"$": [detail]}))
        }
    };
    // `POST /command` reads its body itself and says plain JSON.
    let content_type = if command {
        "application/json; charset=utf-8"
    } else {
        "application/problem+json; charset=utf-8"
    };
    crate::error::validation_problem(document, content_type, errors)
}

/// What the originals' pipeline does around an action once the request is
/// authorized: on the API, on `GET /api` and on the calendar feed. A 401
/// carries none of this, so the key is checked outside.
///
/// - `X-Application-Version` under `/api` (`VersionMiddleware`; Prowlarr
///   reads it from the indexer test), but not on the answer the exception
///   handler writes, which starts from an empty response.
/// - No caching (`CacheHeaderMiddleware`), except covers and `OPTIONS`.
/// - `charset=utf-8` on JSON.
pub async fn finish(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = asked_path(&state, &request);
    let response = next.run(request).await;
    // The path as asked: a slash after it is the same action.
    let to_command = without_trailing_slash(&path).ends_with("/command");
    let mut response = framework_answer(response, state.personality(), to_command).await;
    let thrown = response.extensions().get::<Thrown>().is_some();
    let not_found = response
        .extensions()
        .get::<crate::error::NotFoundProblem>()
        .is_some();
    let headers = response.headers_mut();
    // The exception handler serializes its answer itself: no charset.
    if !thrown {
        json_charset(headers);
    }
    let api = is_api(&path);
    if api && !thrown {
        if let Ok(value) = HeaderValue::from_str(crate::compat::emulated_version(state.personality())) {
            headers.entry("X-Application-Version").or_insert(value);
        }
    }
    if method != Method::OPTIONS {
        if api && path.contains("/mediacover") {
            // A cover is a static file to the originals, found or not: kept
            // for a year, dated (theirs with the build, ours with the
            // start), and a missing one is said in `problem+json` by
            // Sonarr too.
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_FOREVER));
            if !headers.contains_key(header::LAST_MODIFIED) {
                let started = state.runtime().start_time.format("%a, %d %b %Y %H:%M:%S GMT");
                if let Ok(value) = HeaderValue::from_str(&started.to_string()) {
                    headers.insert(header::LAST_MODIFIED, value);
                }
            }
            if not_found {
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/problem+json; charset=utf-8"),
                );
            }
        } else {
            forbid_caching(headers, if thrown { NO_CACHE_THROWN } else { NO_CACHE });
        }
    }
    response
}

/// The path as the client asked it: a nested router sees its own part.
fn whole_path(request: &Request) -> String {
    request
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| request.uri().path(), |original| original.0.path())
        .to_owned()
}

/// `SonarrErrorPipeline` for what Unarr has in place of an unhandled
/// exception: a panic while a request is handled costs that request. It is
/// logged here as the originals log theirs ("Request Failed", with the
/// message and the place of the panic) and handed to the two layers
/// below, which answer it with the originals' 500 (`error::panicked`).
async fn answer_or_panic(request: Request, next: Next) -> Result<Response, unarr_core::panics::Panicked> {
    let method = request.method().clone();
    let path = whole_path(&request);
    unarr_core::panics::catch(next.run(request))
        .await
        .inspect_err(|panic| tracing::error!("Request Failed. {method} {path}: {panic}"))
}

/// Inside [`finish`], around the API's and the feed's actions, so that the
/// answer to a panic leaves as the exception handler's does there: no
/// `X-Application-Version`, no charset, `no-cache,no-store`.
pub async fn catch_panic(request: Request, next: Next) -> Response {
    match answer_or_panic(request, next).await {
        Ok(response) => response,
        Err(panic) => crate::error::panicked(&panic),
    }
}

/// Around the whole router, for what [`catch_panic`] is not around: the
/// pages, `/ping`, the login, SignalR's negotiation and the layers
/// themselves. The originals' handler is the outermost middleware too and
/// writes the same answer whatever the path; nothing after this layer
/// forbids caching it, so it does.
pub async fn catch_panic_anywhere(request: Request, next: Next) -> Response {
    match answer_or_panic(request, next).await {
        Ok(response) => response,
        Err(panic) => {
            let mut answer = crate::error::panicked(&panic);
            forbid_caching(answer.headers_mut(), NO_CACHE_THROWN);
            answer
        }
    }
}

/// No caching, for what is neither a page nor the API: SignalR's negotiation.
pub async fn no_cache(mut response: Response) -> Response {
    if response.status() != StatusCode::SWITCHING_PROTOCOLS && response.status() != StatusCode::UNAUTHORIZED {
        forbid_caching(response.headers_mut(), NO_CACHE);
    }
    response
}

/// `ApiCorsPolicy`: any origin, any method, any header, on every action of
/// the API. A preflight is answered before the key is asked for, which a
/// browser does not send with it; an answer to a request that names its
/// origin, a 401 included, may be read from any page. A route layer: a
/// path no action takes gets neither.
pub async fn cors(request: Request, next: Next) -> Response {
    let headers = request.headers();
    if !headers.contains_key(header::ORIGIN) {
        return next.run(request).await;
    }
    let any_origin = HeaderValue::from_static("*");
    if request.method() == Method::OPTIONS {
        if let Some(method) = headers.get(header::ACCESS_CONTROL_REQUEST_METHOD) {
            let mut response = StatusCode::NO_CONTENT.into_response();
            let answer = response.headers_mut();
            answer.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, any_origin);
            answer.insert(header::ACCESS_CONTROL_ALLOW_METHODS, method.clone());
            let asked: Vec<&str> = headers
                .get_all(header::ACCESS_CONTROL_REQUEST_HEADERS)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .collect();
            if let Ok(value) = HeaderValue::from_str(&asked.join(",")) {
                if !asked.is_empty() {
                    answer.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
                }
            }
            return response;
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, any_origin);
    response
}

/// The content types ASP.NET's response compression takes by default.
/// Not `application/problem+json`, nor `text/calendar`, nor any image.
#[derive(Debug, Clone, Copy)]
pub struct Compressible;

const COMPRESSIBLE: &[&str] = &[
    "text/plain",
    "text/css",
    "application/javascript",
    "text/javascript",
    "text/html",
    "application/xml",
    "text/xml",
    "application/json",
    "text/json",
    "application/wasm",
];

impl tower_http::compression::Predicate for Compressible {
    fn should_compress<B>(&self, response: &axum::http::Response<B>) -> bool
    where
        B: axum::body::HttpBody,
    {
        // Nor what the exception handler writes.
        response.extensions().get::<Thrown>().is_none()
            && response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase())
                .is_some_and(|essence| COMPRESSIBLE.contains(&essence.as_str()))
    }
}

/// `AddResponseCompression()`: gzip for a client that accepts it, at the
/// speed ASP.NET compresses, and `Vary: Accept-Encoding` on everything of
/// a type that could have been compressed. The originals prefer Brotli
/// when the client takes both; a client that takes Brotli takes gzip.
pub fn compression() -> tower_http::compression::CompressionLayer<Compressible> {
    tower_http::compression::CompressionLayer::new()
        .quality(tower_http::CompressionLevel::Fastest)
        .compress_when(Compressible)
}

/// One slash at the end of a path is no part of it for ASP.NET's routes:
/// `/api/v3/tag/` is `/api/v3/tag`. Two are. The same holds for the root
/// of the application under a URL base: `/sonarr/` is `/sonarr`, which is
/// where a `location /sonarr/` proxy and the login form's return send a
/// browser.
pub fn without_trailing_slash(path: &str) -> &str {
    match path.strip_suffix('/') {
        Some(rest) if !rest.is_empty() && !rest.ends_with('/') => rest,
        _ => path,
    }
}

/// `Accept-Encoding` without the codings it refuses. A client that sends
/// `identity;q=0` (or `*;q=0`) and nothing the server compresses to would
/// have the compression layer turn the answer into a 406 after the handler
/// has run; the originals just send it as it is.
pub fn acceptable_encodings(value: &str) -> Option<String> {
    let kept: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter(|part| {
            let mut pieces = part.split(';').map(str::trim);
            let coding = pieces.next().unwrap_or("");
            let refused = pieces.any(|p| {
                p.strip_prefix("q=")
                    .or_else(|| p.strip_prefix("Q="))
                    .and_then(|q| q.trim().parse::<f32>().ok())
                    .is_some_and(|q| q == 0.0)
            });
            !(refused && (coding.eq_ignore_ascii_case("identity") || coding == "*"))
        })
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a panicking action answers behind `layer`.
    async fn answer_of_a_panic(layer: axum::Router) -> (StatusCode, HeaderMap, serde_json::Value) {
        use tower::ServiceExt;
        unarr_core::panics::install_hook();
        let response = layer
            .oneshot(
                Request::builder()
                    .uri("/boom")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        (
            parts.status,
            parts.headers,
            serde_json::from_slice(&bytes).unwrap(),
        )
    }

    async fn boom() -> &'static str {
        let nothing: Vec<u8> = Vec::new();
        tokio::task::yield_now().await;
        if nothing.is_empty() {
            panic!("the handler went wrong");
        }
        "never"
    }

    #[tokio::test]
    async fn a_panic_in_an_action_is_the_originals_500() {
        let router = axum::Router::new()
            .route("/boom", axum::routing::get(boom))
            .route("/fine", axum::routing::get(|| async { "fine" }));
        // As recorded from both originals for an exception no one caught:
        // 500, JSON without a charset, never cached.
        let (status, headers, body) = answer_of_a_panic(
            router
                .clone()
                .layer(axum::middleware::from_fn(catch_panic_anywhere)),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache,no-store");
        assert_eq!(headers[header::PRAGMA], "no-cache");
        assert_eq!(headers[header::EXPIRES], "-1");
        assert_eq!(body["message"], "the handler went wrong");
        let description = body["description"].as_str().unwrap();
        assert!(
            description.starts_with("panicked at ") && description.ends_with(": the handler went wrong"),
            "{description}"
        );
        assert!(description.contains("http.rs:"), "{description}");

        // The inner one leaves the headers to `finish`, and marks the
        // answer as the exception handler's for it.
        let (status, headers, _) =
            answer_of_a_panic(router.clone().layer(axum::middleware::from_fn(catch_panic))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!headers.contains_key(header::CACHE_CONTROL));

        // The router answers the next request.
        use tower::ServiceExt;
        let next = router
            .layer(axum::middleware::from_fn(catch_panic_anywhere))
            .oneshot(
                Request::builder()
                    .uri("/fine")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(next.status(), StatusCode::OK);
    }

    #[test]
    fn one_trailing_slash_is_dropped() {
        assert_eq!(without_trailing_slash("/api/v3/tag/"), "/api/v3/tag");
        assert_eq!(without_trailing_slash("/api/v3/tag"), "/api/v3/tag");
        assert_eq!(without_trailing_slash("/api/v3/tag//"), "/api/v3/tag//");
        assert_eq!(without_trailing_slash("/"), "/");
        // The root of the application under a URL base too.
        assert_eq!(without_trailing_slash("/sonarr/"), "/sonarr");
        assert_eq!(without_trailing_slash("/sonarr/login/"), "/sonarr/login");
        // What a client refuses is not offered to the compression layer.
        assert_eq!(acceptable_encodings("identity;q=0"), None);
        assert_eq!(acceptable_encodings("gzip, identity; q=0"), Some("gzip".into()));
        assert_eq!(acceptable_encodings("*;q=0, br"), Some("br".into()));
        assert_eq!(
            acceptable_encodings("gzip;q=0, identity"),
            Some("gzip;q=0, identity".into())
        );
    }

    #[test]
    fn allow_lists_what_was_declared() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ALLOW, HeaderValue::from_static("GET,HEAD,PUT,POST"));
        allow_as_the_originals(&mut headers);
        assert_eq!(headers[header::ALLOW], "GET, POST, PUT");
    }

    #[test]
    fn json_gets_its_charset_and_nothing_else_does() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        json_charset(&mut headers);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json; charset=utf-8");
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/calendar"));
        json_charset(&mut headers);
        assert_eq!(headers[header::CONTENT_TYPE], "text/calendar");
    }
}
