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
    let (as_asked, query) = path_in_app(&state, &request);
    let accept = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let response = next.run(request).await;
    // The path as asked: a slash after it is the same action.
    let to_command = without_trailing_slash(&path).ends_with("/command");
    let response = framework_answer(response, state.personality(), to_command).await;
    // Content negotiation: the API's JSON, not the feed's calendar.
    let mut response = if is_api(&path) {
        negotiate(response, accept.as_deref(), state.personality())
    } else {
        response
    };
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
        // The exception handler's answer is never one to keep.
        if !thrown && cacheable(&as_asked, query.as_deref()) {
            // A cover is a static file to the originals, found or not: kept
            // for a year, dated (the file's, or the build's), and a missing
            // one is said in `problem+json` by Sonarr too. A cache breaker
            // in the query (`?h=`) makes any answer here one to keep, the
            // feed's too.
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_FOREVER));
            if !headers.contains_key(header::LAST_MODIFIED) {
                headers.insert(header::LAST_MODIFIED, build_date(state.personality()));
            }
            if not_found && path.contains("/mediacover") {
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
            if let Some(value) = requested_headers(headers) {
                answer.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
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

// --- Kestrel

/// `Server: Kestrel`, which the originals' web server puts on every answer
/// (recorded on both): kept as theirs by the maintainer's decision
/// (`docs/audit/7-triage.md`, row 1.34).
pub async fn server_header(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::SERVER, HeaderValue::from_static("Kestrel"));
    response
}

// --- CacheHeaderMiddleware

/// `BuildInfo.BuildDateTime.ToString("r")`: when the release whose UI is
/// embedded was built, which the originals give as `Last-Modified` to
/// whatever they let a client keep.
pub fn build_date(personality: Personality) -> HeaderValue {
    let date = http_date(crate::compat::emulated_build_time(personality));
    HeaderValue::from_str(&date).unwrap_or(HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT"))
}

/// An RFC 3339 time as .NET's `"r"` format writes it; empty when unreadable.
fn http_date(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|at| {
            at.with_timezone(&chrono::Utc)
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string()
        })
        .unwrap_or_default()
}

/// `StartsWithSegments`, without case.
fn starts_with_segment(path: &str, segment: &str) -> bool {
    let n = segment.len();
    path.len() >= n
        && path.is_char_boundary(n)
        && path[..n].eq_ignore_ascii_case(segment)
        && (path.len() == n || path.as_bytes()[n] == b'/')
}

/// `CacheableSpecification`: what a client may keep for a year. `path` is
/// the request's without the URL base, in the case it was asked;
/// the comparisons are the original's, some of them with case.
/// `/ping` is not among its exceptions: Unarr leaves its lower-case
/// spelling uncached by choice (`docs/DESIGN.md`, section 11).
pub fn cacheable(path: &str, query: Option<&str>) -> bool {
    // A cache breaker in the query: any path, the API's too.
    let breaker = query.is_some_and(|q| {
        q.split('&').any(|pair| {
            pair.split('=')
                .next()
                .is_some_and(|k| k.eq_ignore_ascii_case("h"))
        })
    });
    if breaker {
        return true;
    }
    let lower = path.to_ascii_lowercase();
    if starts_with_segment(path, "/api") {
        return lower.contains("/mediacover");
    }
    if starts_with_segment(path, "/signalr") {
        return false;
    }
    if path.ends_with("/index.js") || path.ends_with("/initialize.json") {
        return false;
    }
    if lower.starts_with("/feed") {
        return false;
    }
    if (lower.starts_with("/logfile") || lower.starts_with("/updatelogfile")) && lower.ends_with(".txt") {
        return false;
    }
    // Unarr's one addition: a backup holds the database and the API key,
    // and a shared cache in front that keeps what is `public` would hand
    // it to anyone who asks for its name (`docs/DESIGN.md`, section 22).
    if starts_with_segment(path, "/backup") {
        return false;
    }
    true
}

/// Marks an answer that signed a session in or out: ASP.NET's cookie
/// handler then forbids caching in its own spelling, and leaves the
/// `Last-Modified` the cache headers had set.
#[derive(Debug, Clone, Copy)]
pub struct SignedInOrOut;

/// The headers the cookie handler writes when it signs in or out.
pub fn signed_in_or_out(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_CACHE_THROWN));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::EXPIRES,
        HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT"),
    );
    response.extensions_mut().insert(SignedInOrOut);
}

/// What `CacheHeaderMiddleware` leaves on an answer once the action has
/// written its own: it sets its headers first, so an action that set
/// `Cache-Control` (a page, the bootstrap, a sign-in) keeps it. An answer of
/// the exception handler starts from nothing and forbids caching.
pub fn apply_cache_headers(mut response: Response, cacheable: bool, personality: Personality) -> Response {
    if response.extensions().get::<Thrown>().is_some() {
        let headers = response.headers_mut();
        headers.remove(header::LAST_MODIFIED);
        forbid_caching(headers, NO_CACHE_THROWN);
        return response;
    }
    let signed = response.extensions().get::<SignedInOrOut>().is_some();
    let headers = response.headers_mut();
    if headers.contains_key(header::CACHE_CONTROL) {
        if signed && cacheable && !headers.contains_key(header::LAST_MODIFIED) {
            headers.insert(header::LAST_MODIFIED, build_date(personality));
        }
        return response;
    }
    if cacheable {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_FOREVER));
        headers.insert(header::LAST_MODIFIED, build_date(personality));
    } else {
        headers.remove(header::LAST_MODIFIED);
        forbid_caching(headers, NO_CACHE);
    }
    response
}

/// The path of the request without the URL base, as the client asked it,
/// and its query: what the original's `HttpRequest.Path` is past
/// `UsePathBase`.
pub(crate) fn path_in_app(state: &AppState, request: &Request) -> (String, Option<String>) {
    let asked = crate::asked_uri(request);
    let base = crate::routed_url_base(state, request);
    let path = asked.path();
    let n = base.len();
    let inside = !base.is_empty()
        && path.len() >= n
        && path.is_char_boundary(n)
        && path[..n].eq_ignore_ascii_case(&base)
        && (path.len() == n || path.as_bytes()[n] == b'/');
    let path = if inside { &path[n..] } else { path };
    (path.to_owned(), asked.query().map(str::to_owned))
}

/// `CacheHeaderMiddleware` outside the API, which [`finish`] covers: placed
/// after the authentication, so a 401 carries none of this.
pub async fn cache_headers(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let (path, query) = path_in_app(&state, &request);
    // Unarr's one exception: `/ping` as health checks ask it.
    if request.method() == Method::OPTIONS || path == "/ping" {
        return next.run(request).await;
    }
    let cacheable = cacheable(&path, query.as_deref());
    let response = next.run(request).await;
    apply_cache_headers(response, cacheable, state.personality())
}

// --- FileExtensionContentTypeProvider

/// The type ASP.NET's `FileExtensionContentTypeProvider` gives a file by its
/// extension, for the extensions the bundled UI, the covers and the backups
/// have; `application/octet-stream` for the rest. Radarr runs on a newer
/// runtime, whose table says `text/javascript` for scripts.
pub fn content_type(personality: Personality, path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "application/octet-stream";
    };
    match extension.to_ascii_lowercase().as_str() {
        "js" if personality == Personality::Radarr => "text/javascript",
        "js" => "application/javascript",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "text/xml",
        // `StaticResourceMapperBase` names the charset of plain text.
        "txt" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "application/font-woff",
        "woff2" => "font/woff2",
        "ttf" => "application/x-font-ttf",
        "eot" => "application/vnd.ms-fontobject",
        "zip" => "application/x-zip-compressed",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

// --- ReturnHttpNotAcceptable

const JSON_UTF8: &str = "application/json; charset=utf-8";
const TEXT_JSON_UTF8: &str = "text/json; charset=utf-8";

/// MVC's content negotiation for a JSON result, with
/// `ReturnHttpNotAcceptable`: the type it is written as for the request's
/// `Accept`, or `None` for the 406. An `Accept` with `*/*` in it is not
/// heeded at all (`RespectBrowserAcceptHeader` is off). Sonarr's formatter
/// writes `application/json` alone; Radarr's, `text/json` too.
pub fn negotiated_json(accept: Option<&str>, personality: Personality) -> Option<&'static str> {
    let Some(accept) = accept.map(str::trim).filter(|a| !a.is_empty()) else {
        return Some(JSON_UTF8);
    };
    let mut ranges: Vec<(f32, String)> = accept
        .split(',')
        .filter_map(|part| {
            let mut pieces = part.split(';');
            let media = pieces.next()?.trim().to_ascii_lowercase();
            let q = pieces
                .filter_map(|p| {
                    let p = p.trim();
                    p.strip_prefix("q=").or_else(|| p.strip_prefix("Q="))
                })
                .find_map(|q| q.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            (!media.is_empty()).then_some((q, media))
        })
        .collect();
    if ranges.iter().any(|(_, media)| media == "*/*") {
        return Some(JSON_UTF8);
    }
    // A total order, `q=nan` included: `sort_by` may panic on one that is not.
    ranges.sort_by(|a, b| b.0.total_cmp(&a.0));
    let text_json = personality == Personality::Radarr;
    ranges.iter().find_map(|(_, media)| match media.as_str() {
        "application/json" | "application/*" => Some(JSON_UTF8),
        "text/json" | "text/*" if text_json => Some(TEXT_JSON_UTF8),
        _ => None,
    })
}

/// [`negotiated_json`] applied to what an action answered: a JSON result
/// the client does not take becomes an empty 406, its other headers kept.
/// The action has run: a write is done all the same. What the exception
/// handler writes is not negotiated.
pub fn negotiate(mut response: Response, accept: Option<&str>, personality: Personality) -> Response {
    if response.extensions().get::<Thrown>().is_some() {
        return response;
    }
    let essence = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
    let json = essence.as_deref() == Some("application/json");
    if !json && essence.as_deref() != Some("application/problem+json") {
        return response;
    }
    match negotiated_json(accept, personality) {
        Some(written_as) => {
            if json && written_as != JSON_UTF8 {
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static(written_as));
            }
            response
        }
        None => {
            let (mut parts, _) = response.into_parts();
            parts.status = StatusCode::NOT_ACCEPTABLE;
            parts.headers.remove(header::CONTENT_TYPE);
            parts.headers.remove(header::CONTENT_LENGTH);
            // `CreatedAtAction` names the new resource as it is formatted.
            parts.headers.remove(header::LOCATION);
            Response::from_parts(parts, axum::body::Body::empty())
        }
    }
}

/// `/ping`, which `PingController` answers through MVC: its JSON charset,
/// and the 406 for an `Accept` without JSON.
pub async fn ping_answer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let accept = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut response = next.run(request).await;
    json_charset(response.headers_mut());
    negotiate(response, accept.as_deref(), state.personality())
}

// --- LoggingMiddleware

/// .NET's name for a status (`HttpStatusCode`), as the request log writes
/// it after the number; the number alone for one it has no name for.
fn status_name(status: StatusCode) -> String {
    let name = match status.as_u16() {
        100 => "Continue",
        101 => "SwitchingProtocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "NoContent",
        206 => "PartialContent",
        301 => "MovedPermanently",
        302 => "Found",
        303 => "SeeOther",
        304 => "NotModified",
        307 => "TemporaryRedirect",
        308 => "PermanentRedirect",
        400 => "BadRequest",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "NotFound",
        405 => "MethodNotAllowed",
        406 => "NotAcceptable",
        408 => "RequestTimeout",
        409 => "Conflict",
        411 => "LengthRequired",
        413 => "RequestEntityTooLarge",
        415 => "UnsupportedMediaType",
        422 => "UnprocessableEntity",
        429 => "TooManyRequests",
        500 => "InternalServerError",
        501 => "NotImplemented",
        502 => "BadGateway",
        503 => "ServiceUnavailable",
        _ => return status.as_u16().to_string(),
    };
    name.to_owned()
}

/// `LoggingMiddleware`: each request and its answer under the `Http`
/// logger at trace, with a number that pairs them, the path and query as
/// sent (the URL base included), the client's address and its user agent;
/// a request under `/api` again under the `Api` logger at debug. The log
/// files and the console cleanse the key from the query, as the originals'
/// targets do. Placed after the host filter, as theirs is.
pub async fn log_requests(State(state): State<AppState>, request: Request, next: Next) -> Response {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // Nothing is prepared for a log that is off, as it is at `info`.
    let api = starts_with_segment(request.uri().path(), "/api");
    let traced = tracing::enabled!(target: "Http", tracing::Level::TRACE);
    if !traced && !(api && tracing::enabled!(target: "Api", tracing::Level::DEBUG)) {
        return next.run(request).await;
    }
    let id = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let method = request.method().clone();
    let uri = request.uri();
    let path = match uri.query() {
        Some(query) if !query.is_empty() => format!("{}?{query}", uri.path()),
        _ => uri.path().to_owned(),
    };
    let remote = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    let headers = request.headers();
    let address = crate::forwarded::client(remote, headers, &state.settings().trusted_networks)
        .address
        .or(remote)
        .map(|a| a.to_string())
        .unwrap_or_default();
    let agent = headers
        .get(header::USER_AGENT)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default();
    let origin = if agent.trim().is_empty() {
        address
    } else {
        format!("{address} {agent}")
    };
    tracing::trace!(target: "Http", "Req: {id} [{method}] {path} (from {origin})");
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    let ms = started.elapsed().as_millis();
    let status = response.status();
    let name = status_name(status);
    let code = status.as_u16();
    tracing::trace!(target: "Http", "Res: {id} [{method}] {path}: {code}.{name} ({ms} ms)");
    if api {
        tracing::debug!(target: "Api", "[{method}] {path}: {code}.{name} ({ms} ms)");
    }
    response
}

// --- StartingUpMiddleware

/// `StartingUpMiddleware`: until the command runtime has started, an
/// authenticated request is answered 503, in JSON under `/api` and in
/// plain text elsewhere. It sits after the authentication and the URL
/// base's redirect, as in the originals.
pub async fn starting_up(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if !state.is_starting() {
        return next.run(request).await;
    }
    let (path, _) = path_in_app(&state, &request);
    let message = format!(
        "{} is starting up, please try again later",
        state.personality().app_name()
    );
    if starts_with_segment(&path, "/api") {
        let body = serde_json::to_string_pretty(&json!({"errorMessage": message})).unwrap_or_default();
        let mut response = (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response();
        // `VersionMiddleware` runs before it.
        if let Ok(value) = HeaderValue::from_str(crate::compat::emulated_version(state.personality())) {
            response.headers_mut().insert("X-Application-Version", value);
        }
        return response;
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "text/plain")],
        message,
    )
        .into_response()
}

// --- CORS for static content

/// The `AllowGet` policy on `/content`: any origin, `GET` and `OPTIONS`,
/// any header. A preflight for another method is not the policy's to
/// answer and goes on to the router, which takes no `OPTIONS` there.
pub async fn cors_allow_get(request: Request, next: Next) -> Response {
    let headers = request.headers();
    if !headers.contains_key(header::ORIGIN) {
        return next.run(request).await;
    }
    let any_origin = HeaderValue::from_static("*");
    if request.method() == Method::OPTIONS {
        if let Some(method) = headers.get(header::ACCESS_CONTROL_REQUEST_METHOD) {
            if method == "GET" || method == "OPTIONS" {
                let mut response = StatusCode::NO_CONTENT.into_response();
                let answer = response.headers_mut();
                answer.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, any_origin);
                answer.insert(
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    HeaderValue::from_static("GET,OPTIONS"),
                );
                if let Some(value) = requested_headers(headers) {
                    answer.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
                }
                return response;
            }
            return next.run(request).await;
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, any_origin);
    response
}

/// The headers a preflight asks for, as a policy that allows any header
/// echoes them: one comma between them, no space.
fn requested_headers(headers: &HeaderMap) -> Option<HeaderValue> {
    let asked: Vec<&str> = headers
        .get_all(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .collect();
    if asked.is_empty() {
        return None;
    }
    HeaderValue::from_str(&asked.join(",")).ok()
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

/// `AddResponseCompression()`: Brotli or gzip, whichever the client
/// weighs more, Brotli when it weighs both alike, at the speed ASP.NET
/// compresses. `Vary: Accept-Encoding` goes on everything of a type that
/// could have been compressed, to a client that named the codings it
/// takes; [`compression_as_requested`] sees to the rest.
pub fn compression() -> tower_http::compression::CompressionLayer<Compressible> {
    tower_http::compression::CompressionLayer::new()
        // `CompressionLevel.Fastest`: gzip's level 1 and Brotli's quality 1.
        .quality(tower_http::CompressionLevel::Precise(1))
        .compress_when(Compressible)
}

/// What ASP.NET's response compression does that tower-http's does not,
/// around the whole router: no compression at all, so no `Vary`, for a
/// request without `Accept-Encoding` or one that reached the proxy in
/// front over HTTPS (`EnableForHttps` is off); `Accept-Encoding` spelled as
/// ASP.NET spells it in `Vary`.
pub async fn compression_as_requested(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let named = request
        .headers()
        .get(header::ACCEPT_ENCODING)
        .is_some_and(|v| !v.as_bytes().iter().all(u8::is_ascii_whitespace));
    let remote = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    let https = crate::forwarded::walk(remote, request.headers(), &state.settings().trusted_networks)
        .scheme
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"));
    let compressed = named && !https;
    if !compressed {
        request.headers_mut().remove(header::ACCEPT_ENCODING);
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let vary: Vec<HeaderValue> = headers.get_all(header::VARY).iter().cloned().collect();
    if vary.is_empty() {
        return response;
    }
    headers.remove(header::VARY);
    for value in vary {
        if value.as_bytes().eq_ignore_ascii_case(b"accept-encoding") {
            if compressed {
                headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
            }
        } else {
            headers.append(header::VARY, value);
        }
    }
    response
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
    fn what_may_be_kept_is_what_cacheable_specification_says() {
        for (path, query) in [
            ("/Content/styles.css", None),
            ("/Ping", None),
            ("/login", None),
            ("/MediaCover/1/poster.jpg", None),
            ("/api/v3/MediaCover/1/poster.jpg", None),
            ("/api/v3/system/status", Some("h=1")),
            ("/Initialize.json", None),
            ("/Index.js", None),
            ("/logfile/sonarr.log", None),
            ("/feed", Some("H")),
        ] {
            assert!(cacheable(path, query), "{path} {query:?}");
        }
        for (path, query) in [
            ("/api/v3/system/status", None),
            ("/API", None),
            ("/SignalR/messages", None),
            ("/x/index.js", None),
            ("/initialize.json", None),
            ("/Feed/v3/calendar/Sonarr.ics", None),
            ("/feedx", None),
            ("/LogFile/sonarr.TXT", None),
            ("/updatelogfile/x.txt", Some("hx=1")),
            (
                "/backup/manual/sonarr_backup_v4.0.20.3014_2026.11.20_10.00.00.zip",
                None,
            ),
            ("/Backup/scheduled/x.zip", None),
        ] {
            assert!(!cacheable(path, query), "{path} {query:?}");
        }
        assert_eq!(http_date("2024-02-29T08:05:09Z"), "Thu, 29 Feb 2024 08:05:09 GMT");
        assert_eq!(
            http_date("2024-02-29T10:05:09+02:00"),
            "Thu, 29 Feb 2024 08:05:09 GMT"
        );
        for personality in [Personality::Sonarr, Personality::Radarr] {
            let pinned = unarr_core::pin::of(personality).unwrap().build_time;
            assert_eq!(build_date(personality), http_date(pinned).as_str());
            assert!(!http_date(pinned).is_empty(), "{pinned}");
        }
    }

    #[test]
    fn types_are_aspnets_by_extension() {
        assert_eq!(
            content_type(Personality::Sonarr, "/index-1.js"),
            "application/javascript"
        );
        assert_eq!(
            content_type(Personality::Radarr, "/index-1.JS"),
            "text/javascript"
        );
        assert_eq!(
            content_type(Personality::Sonarr, "a.LICENSE.txt"),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            content_type(Personality::Sonarr, "backup.zip"),
            "application/x-zip-compressed"
        );
        assert_eq!(
            content_type(Personality::Sonarr, "x.woff"),
            "application/font-woff"
        );
        assert_eq!(
            content_type(Personality::Sonarr, "VERSION"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type(Personality::Sonarr, "a.map"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type(Personality::Sonarr, "dir.d/file"),
            "application/octet-stream"
        );
    }

    #[test]
    fn json_is_written_for_what_accept_takes() {
        let sonarr = |accept| negotiated_json(accept, Personality::Sonarr);
        let radarr = |accept| negotiated_json(accept, Personality::Radarr);
        assert_eq!(sonarr(None), Some(JSON_UTF8));
        assert_eq!(sonarr(Some(" ")), Some(JSON_UTF8));
        assert_eq!(sonarr(Some("text/html, */*;q=0.1")), Some(JSON_UTF8));
        assert_eq!(sonarr(Some("Application/*")), Some(JSON_UTF8));
        assert_eq!(
            sonarr(Some("text/html;q=0.9, application/json;q=0.2")),
            Some(JSON_UTF8)
        );
        assert_eq!(sonarr(Some("text/json")), None);
        assert_eq!(sonarr(Some("application/problem+json")), None);
        assert_eq!(radarr(Some("text/*")), Some(TEXT_JSON_UTF8));
        assert_eq!(radarr(Some("text/json;q=0.5, application/json")), Some(JSON_UTF8));
        assert_eq!(radarr(Some("text/html")), None);
        // `q=nan` on many types: a total order, no panic.
        let nans = vec!["text/html;q=nan"; 64].join(", ");
        assert_eq!(sonarr(Some(nans.as_str())), None);
        let with_json = format!("{nans}, application/json;q=nan, text/plain;q=0.3");
        assert_eq!(sonarr(Some(with_json.as_str())), Some(JSON_UTF8));
        assert_eq!(status_name(StatusCode::FOUND), "Found");
        assert_eq!(status_name(StatusCode::IM_A_TEAPOT), "418");
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
