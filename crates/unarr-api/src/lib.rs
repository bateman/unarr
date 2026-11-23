//! Compatible HTTP surface. One router per personality, mounted under the
//! original API prefix, plus `/signalr` so the official web UIs can connect.
//!
//! Every response reproduces the JSON shape of the original application:
//! camelCase, the same field names and nesting, `null` fields omitted. The
//! contract tests under `tests/` replay responses recorded from the real
//! application against this router.

pub mod auth;
mod blocking;
pub mod cleanse;
pub mod compat;
pub mod error;
pub mod forwarded;
pub mod fs;
pub mod hosts;
pub mod http;
pub mod json;
pub mod logbuf;
pub mod logfile;
pub mod mediainfo;
pub mod query;
pub mod relay;
pub mod resources;
pub mod routes;
pub mod schemas;
pub mod state;
pub mod ui;

use axum::response::IntoResponse;
use axum::Router;
use tower_http::trace::TraceLayer;

pub use logbuf::LogBuffer;
pub use logfile::{LogFiles, ProcessLog};
pub use state::{AppState, Lifecycle, RuntimeInfo, Settings};

/// The request as the client sent it, before [`normalize_path`] and the
/// router rewrote its path: what a redirect hands back.
#[derive(Clone)]
pub struct ReceivedUri(pub axum::http::Uri);

/// Marks a request for a path outside the URL base. The originals route
/// it as if the base were not there and authenticate it so, then
/// `UrlBaseMiddleware` sends it into the base with a 307.
#[derive(Clone)]
struct OutsideUrlBase;

/// The URL base the router was built with. A base saved from the UI
/// changes the settings at once, but the routes stay where they were until
/// the restart (`docs/audit/7-triage.md`, row 1.35): what decides whether a
/// path is inside the base, and where a 307 sends it, is this one, or a
/// request inside the new base would be sent into it again and again.
#[derive(Clone)]
pub(crate) struct RoutedUrlBase(pub String);

/// [`RoutedUrlBase`], or the settings' for a request that did not come
/// through the router (a handler called on its own in a test).
pub(crate) fn routed_url_base(state: &AppState, request: &axum::extract::Request) -> String {
    request
        .extensions()
        .get::<RoutedUrlBase>()
        .map(|b| b.0.clone())
        .unwrap_or_else(|| state.url_base().to_owned())
}

/// The URI the client asked for.
pub(crate) fn asked_uri(request: &axum::extract::Request) -> axum::http::Uri {
    request
        .extensions()
        .get::<ReceivedUri>()
        .map(|r| r.0.clone())
        .or_else(|| {
            request
                .extensions()
                .get::<axum::extract::OriginalUri>()
                .map(|o| o.0.clone())
        })
        .unwrap_or_else(|| request.uri().clone())
}

/// Where the URL base is in the length of `base` at the start of `path`,
/// in any case, as a whole segment.
fn url_base_len(path: &str, base: &str) -> Option<usize> {
    let n = base.len();
    (!base.is_empty()
        && path.len() >= n
        && path.is_char_boundary(n)
        && path[..n].eq_ignore_ascii_case(base)
        && (path.len() == n || path.as_bytes()[n] == b'/'))
        .then_some(n)
}

/// The URL base as the client wrote it: the originals' `PathBase`, which
/// the login redirect starts from. Empty outside the base.
pub(crate) fn url_base_as_asked(state: &AppState, request: &axum::extract::Request) -> String {
    if request.extensions().get::<OutsideUrlBase>().is_some() {
        return String::new();
    }
    let base = routed_url_base(state, request);
    let asked = asked_uri(request);
    match url_base_len(asked.path(), &base) {
        Some(n) => asked.path()[..n].to_owned(),
        None => base,
    }
}

/// The originals route API paths case-insensitively (Bazarr asks for
/// `episodeFile`), the URL base too, and take a path with one slash after
/// it for the same path: lower-case the base and the API part and drop
/// that slash before matching.
async fn normalize_path(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let uri = request.uri().clone();
    request.extensions_mut().insert(ReceivedUri(uri.clone()));
    let base = routed_url_base(&state, &request);
    let path = http::without_trailing_slash(uri.path());
    let (path, after_base) = match url_base_len(path, &base) {
        Some(n) => (format!("{base}{}", &path[n..]), n),
        None => (path.to_owned(), 0),
    };
    let lower = path.to_ascii_lowercase();
    // The API part, under `/api` in any case: what precedes it keeps its case.
    let rest = &lower[after_base..];
    let new_path = if rest == "/api" || rest.starts_with("/api/") {
        format!("{}{}", &path[..after_base], rest)
    } else if let Some((matched, route)) = literal_route(state.personality(), rest) {
        format!("{}{route}{}", &path[..after_base], &path[after_base + matched..])
    } else {
        path
    };
    // A coding the client refuses is not the compression layer's to act on.
    let asked = request
        .headers()
        .get(axum::http::header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(asked) = asked {
        match http::acceptable_encodings(&asked) {
            Some(kept) if kept == asked => {}
            Some(kept) => {
                if let Ok(value) = kept.parse() {
                    request
                        .headers_mut()
                        .insert(axum::http::header::ACCEPT_ENCODING, value);
                }
            }
            None => {
                request.headers_mut().remove(axum::http::header::ACCEPT_ENCODING);
            }
        }
    }
    if new_path != uri.path() {
        let rewritten = match uri.query() {
            Some(q) => format!("{new_path}?{q}"),
            None => new_path,
        };
        if let Ok(new_uri) = rewritten.parse() {
            *request.uri_mut() = new_uri;
        }
    }
    next.run(request).await
}

/// The originals' routes outside the API, which ASP.NET matches without
/// case, in the case they are declared here: how much of `rest` (the
/// lower-cased path without the URL base) is the route's, and its
/// spelling. What their catch-all route hands to a mapper (`/favicon.ico`,
/// the covers, the backups) is matched by the mapper, with case or
/// without, and is left alone; `/content/` keeps the case of what follows.
fn literal_route(personality: unarr_core::Personality, rest: &str) -> Option<(usize, String)> {
    const EXACT: &[&str] = &[
        "/login",
        "/logout",
        "/ping",
        "/initialize.json",
        "/signalr/messages",
        "/signalr/messages/negotiate",
    ];
    if let Some(route) = EXACT.iter().find(|route| rest == **route) {
        return Some((rest.len(), (*route).to_owned()));
    }
    let feed = format!("/feed/v3/calendar/{}.ics", personality.app_name());
    if rest == feed.to_ascii_lowercase() {
        return Some((rest.len(), feed));
    }
    // `/content/{**path}`: the prefix alone.
    if rest.starts_with("/content/") {
        return Some(("/content".len(), "/Content".to_owned()));
    }
    None
}

/// `UrlBaseMiddleware`: a request outside the URL base, once authenticated
/// as the route asks, goes into the base with a 307, its query kept and its
/// method too. Placed in each group after the authentication and before
/// anything else, as in the originals' pipeline; an API path carries the
/// version header, which runs before it there.
async fn into_url_base(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request.extensions().get::<OutsideUrlBase>().is_none() {
        return next.run(request).await;
    }
    let asked = asked_uri(&request);
    let mut location = format!("{}{}", routed_url_base(&state, &request), asked.path());
    if let Some(query) = asked.query() {
        location.push('?');
        location.push_str(query);
    }
    let mut response = axum::http::StatusCode::TEMPORARY_REDIRECT.into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&location) {
        response.headers_mut().insert(axum::http::header::LOCATION, value);
    }
    let path = asked.path().to_ascii_lowercase();
    if path == "/api" || path.starts_with("/api/") {
        if let Ok(value) = axum::http::HeaderValue::from_str(compat::emulated_version(state.personality())) {
            response.headers_mut().insert("X-Application-Version", value);
        }
    }
    response
}

/// SignalR checks its own token in the handler; outside the URL base the
/// originals ask for it before they redirect.
async fn signalr_into_url_base(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request.extensions().get::<OutsideUrlBase>().is_some()
        && !routes::signalr::authorized(&state, request.uri(), request.headers())
    {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    into_url_base(axum::extract::State(state), request, next).await
}

/// A method the route does not take finds the originals' "405" endpoint,
/// which no policy names, so the fallback policy asks for the API key:
/// 401 without it, then 405 inside the URL base and the 307 into it
/// outside (recorded on Sonarr 4.0.20 and Radarr 6.4.4; a cookie does not
/// do). `None` when the method is the route's.
async fn method_not_taken(
    state: &AppState,
    request: axum::extract::Request,
    next: axum::middleware::Next,
    allow: &'static str,
) -> axum::response::Response {
    let key = auth::presented_key(request.uri(), request.headers());
    if !key.is_some_and(|key| auth::key_matches(&state.settings().host.api_key, &key)) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    if request.extensions().get::<OutsideUrlBase>().is_some() {
        return into_url_base(axum::extract::State(state.clone()), request, next).await;
    }
    let (path, query) = http::path_in_app(state, &request);
    let refused = (
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, allow)],
    )
        .into_response();
    // The cache headers come after the authentication in the originals.
    http::apply_cache_headers(
        refused,
        http::cacheable(&path, query.as_deref(), state.personality()),
        state.personality(),
    )
}

/// A page's action takes `GET` alone, `HEAD` not included: the covers,
/// the bundle's files and the rest, as recorded.
async fn pages_take_get_only(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request.method() == axum::http::Method::GET {
        return next.run(request).await;
    }
    method_not_taken(&state, request, next, "GET").await
}

/// The open routes: `GET`, `HEAD` where `/ping` declares it, and the login
/// form's `POST`. `HEAD` on the login page or on `/Content` finds no
/// action, as recorded. An `OPTIONS` preflight `/Content`'s CORS policy
/// takes is answered before this.
async fn open_takes_its_methods(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = request.method();
    let path = request.uri().path();
    let login = path.ends_with("/login");
    let ping = path.ends_with("/ping");
    if method == axum::http::Method::GET
        || (method == axum::http::Method::HEAD && ping)
        || (method == axum::http::Method::POST && login)
    {
        return next.run(request).await;
    }
    let allow = if login { "GET, POST" } else { "GET" };
    method_not_taken(&state, request, next, allow).await
}

/// `[EnableCors("AllowGet")]` on the `/content` action alone.
async fn content_cors(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path().to_ascii_lowercase();
    if path.contains("/content/") {
        return http::cors_allow_get(request, next).await;
    }
    next.run(request).await
}

/// Resources whose `PUT` takes an id: `[RestPutById]` in the originals,
/// which is `HttpPut("{id:int?}")`. Sonarr's.
const SONARR_PUT_BY_ID: &[&str] = &[
    "autotagging",
    "config/downloadclient",
    "config/host",
    "config/importlist",
    "config/indexer",
    "config/mediamanagement",
    "config/naming",
    "config/ui",
    "customfilter",
    "customformat",
    "delayprofile",
    "downloadclient",
    "episode",
    "episodefile",
    "importlist",
    "importlistexclusion",
    "indexer",
    "metadata",
    "notification",
    "qualitydefinition",
    "qualityprofile",
    "releaseprofile",
    "remotepathmapping",
    "series",
    "tag",
];

/// Radarr's.
const RADARR_PUT_BY_ID: &[&str] = &[
    "autotagging",
    "collection",
    "config/downloadclient",
    "config/host",
    "config/importlist",
    "config/indexer",
    "config/mediamanagement",
    "config/metadata",
    "config/naming",
    "config/ui",
    "customfilter",
    "customformat",
    "delayprofile",
    "downloadclient",
    "exclusions",
    "importlist",
    "indexer",
    "metadata",
    "movie",
    "moviefile",
    "notification",
    "qualitydefinition",
    "qualityprofile",
    "releaseprofile",
    "remotepathmapping",
    "tag",
];

fn put_by_id(personality: unarr_core::Personality) -> &'static [&'static str] {
    match personality {
        unarr_core::Personality::Radarr => RADARR_PUT_BY_ID,
        unarr_core::Personality::Sonarr | unarr_core::Personality::Lidarr => SONARR_PUT_BY_ID,
    }
}

/// Split an API path into what precedes the resource, the resource and the
/// id in the route, when the resource is one of `resources`.
fn put_by_id_target<'a>(resources: &[&str], path: &'a str) -> Option<(&'a str, Option<&'a str>)> {
    let api = path.find("/api/v")?;
    let resource_start = api + 1 + path[api + 1..].find('/')? + 1;
    let resource_start = resource_start + path[resource_start..].find('/')? + 1;
    let rest = path[resource_start..].trim_end_matches('/');
    let (resource, id) = match rest.rsplit_once('/') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
            (head, Some(tail))
        }
        _ => (rest, None),
    };
    resources
        .contains(&resource)
        .then(|| (&path[..resource_start + resource.len()], id))
}

/// The id of a `PUT` is the one in the body; the one in the route only
/// fills in for a body without one (the UI saves the settings pages with no
/// id in the route at all). The routes take the id from the path, so put the
/// one that counts there; `0` stands for "none", which the API router
/// rejects as the originals' validation does.
async fn put_resource_id(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request.method() != axum::http::Method::PUT {
        return next.run(request).await;
    }
    let uri = request.uri().clone();
    let Some((base, route_id)) = put_by_id_target(put_by_id(state.personality()), uri.path()) else {
        return next.run(request).await;
    };
    // Radarr's `PUT /collection` with no id is the edit of several
    // collections (`CollectionUpdateResource`), not of one.
    if route_id.is_none()
        && state.personality() == unarr_core::Personality::Radarr
        && base.ends_with("/collection")
    {
        return next.run(request).await;
    }
    // Nothing is read for a caller without the key: the API answers 401
    // whatever the path says.
    let key = auth::presented_key(&uri, request.headers());
    if !key.is_some_and(|key| auth::key_matches(&state.settings().host.api_key, &key)) {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, API_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let id = match (body_resource_id(&bytes), route_id) {
        (Some(id), _) => id.to_string(),
        (None, Some(id)) => id.to_owned(),
        (None, None) => "0".to_owned(),
    };
    let rewritten = match uri.query() {
        Some(q) => format!("{base}/{id}?{q}"),
        None => format!("{base}/{id}"),
    };
    if let Ok(new_uri) = rewritten.parse() {
        parts.uri = new_uri;
    }
    next.run(axum::extract::Request::from_parts(
        parts,
        axum::body::Body::from(bytes),
    ))
    .await
}

/// The resource id a `PUT` body names: an object's positive `id`, read
/// alone (the rest of the body is the handler's to parse). Only an object:
/// a struct would also take `[7]` as `{"id": 7}`.
fn body_resource_id(bytes: &[u8]) -> Option<i64> {
    #[derive(serde::Deserialize)]
    struct IdOnly {
        id: Option<i64>,
    }
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    serde_json::from_slice::<IdOnly>(bytes)
        .ok()
        .and_then(|body| body.id)
        .filter(|id| *id > 0)
}

/// What the body of an API request may weigh. The originals set no limit
/// (Sonarr 4.0.20 took a `PUT` of 120 MiB), and the extractors' default of
/// 2 MiB refused a library import or a manual import of a few hundred
/// items. Raised rather than removed: a body is held whole, and again as
/// the JSON read from it, by one process that serves every personality
/// and ends on a failed allocation. 64 MiB is the ceiling already set on
/// what is read from an indexer or a client, some 25,000 series in one
/// library import. It is granted once the key is good; the login form
/// keeps the default.
pub const API_BODY_LIMIT: usize = 64 * 1024 * 1024;

/// A `PUT` with no id in the body nor in the route: the originals'
/// `Id > 0` rule answers.
async fn reject_put_without_id(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request.method() == axum::http::Method::PUT && request.uri().path().ends_with("/0") {
        let mut failure = unarr_core::validation::ValidationFailure::rule(
            "Id",
            "GreaterThanValidator",
            "'Id' must be greater than '0'.",
            serde_json::json!(0),
        );
        failure.placeholders = vec![
            ("comparisonProperty".into(), serde_json::json!("")),
            ("comparisonValue".into(), serde_json::json!(0)),
        ];
        return error::ApiError::Validation(vec![failure]).into_response();
    }
    next.run(request).await
}

/// A controller's `NotFound()` is Radarr 7's `TypedResults.NotFound()`: a
/// 404 with no body, where Sonarr, at its pin, answers ASP.NET's problem
/// details. The handlers both share answer Sonarr's, replaced here.
async fn radarr_problem_details(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let response = next.run(request).await;
    if response.extensions().get::<error::NotFoundProblem>().is_some() {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    response
}

/// Build the router for one personality.
pub fn router(state: AppState) -> Router {
    let api = routes::api_router(&state);
    let api = match state.personality() {
        unarr_core::Personality::Radarr => api.layer(axum::middleware::from_fn(radarr_problem_details)),
        unarr_core::Personality::Sonarr | unarr_core::Personality::Lidarr => api,
    };
    // From the inside out: what the pipeline does once the key is good,
    // the key (on a path no action takes too: an empty 401, not a 404),
    // compression, and CORS, which answers a preflight before the key is
    // asked for but only where there is an action.
    let api = api
        // A route with a limit of its own (the backup upload) keeps it.
        .layer(axum::extract::DefaultBodyLimit::max(API_BODY_LIMIT))
        .route_layer(axum::middleware::from_fn(reject_put_without_id))
        // A panic in an action is the exception handler's 500, which
        // `finish` then sends as the originals send theirs.
        .layer(axum::middleware::from_fn(http::catch_panic))
        .layer(axum::middleware::from_fn_with_state(state.clone(), http::finish))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            http::starting_up,
        ))
        .layer(axum::middleware::from_fn_with_state(state.clone(), into_url_base))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ))
        .layer(http::compression())
        .route_layer(axum::middleware::from_fn(http::cors));

    // Pages and bootstrap the browser loads: behind the UI policy.
    let pages =
        Router::new()
            .route("/", axum::routing::get(ui::spa))
            // Behind the login, as in the originals.
            .route("/favicon.ico", axum::routing::get(ui::favicon))
            .route(
                "/initialize.json",
                axum::routing::get(routes::system::initialize_json),
            )
            .route(
                "/MediaCoverProxy/{hash}/{file}",
                axum::routing::get(routes::series_writes::media_cover_proxy),
            )
            .route(
                "/backup/{kind}/{name}",
                axum::routing::get(routes::backup::download),
            )
            .route(
                "/logfile/{filename}",
                axum::routing::get(routes::backup::log_file_download),
            )
            .route(
                "/updatelogfile/{filename}",
                axum::routing::get(routes::backup::update_log_file_download),
            )
            .nest("/MediaCover", {
                let covers = state.runtime().media_cover_dir.clone();
                Router::new().fallback(axum::routing::get(ui::media_cover)).layer(
                    axum::middleware::map_request(move |request: axum::extract::Request| {
                        let covers = covers.clone();
                        async move { ui::full_cover_when_resized_is_missing(&covers, request) }
                    }),
                )
            })
            .fallback(axum::routing::get(ui::spa))
            // From the inside out, as in the originals' pipeline: the cache
            // headers, the answer while starting, the URL base's redirect, the
            // authentication.
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                http::cache_headers,
            ))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                http::starting_up,
            ))
            .layer(axum::middleware::from_fn_with_state(state.clone(), into_url_base))
            // `layer`, not `route_layer`: the guard must cover the fallback too,
            // or deep links such as /settings would skip the login.
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                ui::require_ui_auth,
            ))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                pages_take_get_only,
            ))
            .layer(http::compression());

    // Open to anonymous browsers: static content, the login form, SignalR
    // (which checks its own token).
    let open = Router::new()
        .route(
            "/ping",
            axum::routing::get(routes::system::ping).layer(axum::middleware::from_fn_with_state(
                state.clone(),
                http::ping_answer,
            )),
        )
        .route("/Content/{*path}", axum::routing::get(ui::content))
        .route("/login", axum::routing::get(ui::login_page).post(ui::login))
        .route("/logout", axum::routing::get(ui::logout))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            http::cache_headers,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            http::starting_up,
        ))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), into_url_base))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            open_takes_its_methods,
        ))
        // `/content`'s `AllowGet` policy, before the methods are looked at.
        .route_layer(axum::middleware::from_fn(content_cors))
        .layer(http::compression())
        // SignalR's negotiation is not compressed, and not to be kept.
        .merge(
            routes::ui_router(&state)
                .layer(axum::middleware::map_response(http::no_cache))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    http::starting_up,
                ))
                // Outside it: the originals' 307 carries no cache headers.
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    signalr_into_url_base,
                )),
        );

    // The calendar feed: opened with the API key, as the API is.
    // With the rest of `/api`, which no action takes: the key is asked
    // for, then 404 for `GET` and 405 for the rest, as in the originals.
    let feed = routes::feed_router(&state)
        .route("/api/{*rest}", axum::routing::any(routes::unknown_api_route))
        .route_layer(axum::middleware::from_fn(http::catch_panic))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), http::finish))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            http::starting_up,
        ))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), into_url_base))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ))
        .route_layer(http::compression());

    let app = Router::new()
        .nest(state.personality().api_prefix(), api)
        .merge(feed)
        .merge(open)
        .merge(pages)
        .with_state(state.clone())
        // The span carries the path only: the query string may hold the API
        // key (`apikey=`, SignalR's `access_token`).
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                tracing::debug_span!(
                    "request",
                    method = %request.method(),
                    path = %request.uri().path()
                )
            },
        ));

    let url_base = state.url_base().to_owned();
    let app = if url_base.is_empty() {
        app
    } else {
        // Outside the base, the same routes under the mark that makes each
        // group send the request into the base once it is authenticated.
        Router::new()
            .nest(&url_base, app.clone())
            .fallback_service(app.layer(axum::middleware::map_request(
                |mut request: axum::extract::Request| async move {
                    request.extensions_mut().insert(OutsideUrlBase);
                    request
                },
            )))
    };
    // A router's layers run after it has matched, so the path rewrite and the
    // host check sit on an outer router whose only route is the real one.
    Router::new()
        .fallback_service(app)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            put_resource_id,
        ))
        .layer(axum::middleware::from_fn_with_state(state.clone(), http::methods))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            http::compression_as_requested,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            normalize_path,
        ))
        // `LoggingMiddleware`, after the host filter as in the originals.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            http::log_requests,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            hosts::require_allowed_host,
        ))
        // Before anything reads it.
        .layer(axum::middleware::map_request(
            move |mut request: axum::extract::Request| {
                let base = RoutedUrlBase(url_base.clone());
                async move {
                    request.extensions_mut().insert(base);
                    request
                }
            },
        ))
        // Outermost, as the originals' exception handler is: a panic in a
        // page, in `/ping` or in a layer above costs that request only.
        .layer(axum::middleware::from_fn(http::catch_panic_anywhere))
        // Kestrel's name on every answer, the host filter's and a panic's too.
        .layer(axum::middleware::map_response(http::server_header))
}

#[cfg(test)]
mod tests {
    use super::body_resource_id;

    #[test]
    fn only_an_objects_id_names_the_resource() {
        assert_eq!(body_resource_id(br#"{"id": 7, "label": "x"}"#), Some(7));
        assert_eq!(body_resource_id(b"  {\"id\":3}"), Some(3));
        assert_eq!(body_resource_id(b"[7]"), None);
        assert_eq!(body_resource_id(b"7"), None);
        assert_eq!(body_resource_id(br#"{"id": 0}"#), None);
        assert_eq!(body_resource_id(br#"{"id": "7"}"#), None);
        assert_eq!(body_resource_id(br#"{"label": "x"}"#), None);
        assert_eq!(body_resource_id(b"not json"), None);
    }
}
