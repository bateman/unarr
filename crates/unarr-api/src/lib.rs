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

/// The originals route API paths case-insensitively (Bazarr asks for
/// `episodeFile`) and take a path with one slash after it for the same
/// path: lower-case the API part and drop that slash before matching.
async fn normalize_path(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let uri = request.uri().clone();
    let path = http::without_trailing_slash(uri.path(), &state.settings().url_base);
    let lower = path.to_ascii_lowercase();
    // Only the API part: a URL base such as /Sonarr keeps its case.
    let new_path = match lower.find("/api/v") {
        Some(i) => format!("{}{}", &path[..i], &lower[i..]),
        None => path.to_owned(),
    };
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
    let (mut parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, PUT_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let body_id = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("id").and_then(serde_json::Value::as_i64))
        .filter(|id| *id > 0);
    let id = match (body_id, route_id) {
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

/// What a `PUT` body may weigh: the default limit of the JSON extractor.
const PUT_BODY_LIMIT: usize = 2 * 1024 * 1024;

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

/// Radarr's 404 problem details name RFC 9110 where Sonarr's name RFC
/// 7231, and say so as `application/problem+json`: the handlers both share
/// answer Sonarr's, replaced here.
async fn radarr_problem_details(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let response = next.run(request).await;
    if response.extensions().get::<error::NotFoundProblem>().is_some() {
        return error::not_found_problem(
            error::NOT_FOUND_TYPE_RFC9110,
            "application/problem+json; charset=utf-8",
        );
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
        .route_layer(axum::middleware::from_fn(reject_put_without_id))
        .layer(axum::middleware::from_fn_with_state(state.clone(), http::finish))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ))
        .layer(http::compression())
        .route_layer(axum::middleware::from_fn(http::cors));

    // Pages and bootstrap the browser loads: behind the UI policy.
    let pages = Router::new()
        .route("/", axum::routing::get(ui::spa))
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
            Router::new()
                .fallback_service(tower_http::services::ServeDir::new(covers.clone()))
                .layer(axum::middleware::map_request(
                    move |request: axum::extract::Request| {
                        let covers = covers.clone();
                        async move { ui::full_cover_when_resized_is_missing(&covers, request) }
                    },
                ))
        })
        .fallback(axum::routing::get(ui::spa))
        // `layer`, not `route_layer`: the guard must cover the fallback too,
        // or deep links such as /settings would skip the login.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ui::require_ui_auth,
        ))
        .layer(http::compression());

    // Open to anonymous browsers: static content, the login form, SignalR
    // (which checks its own token).
    let open = Router::new()
        .route("/favicon.ico", axum::routing::get(ui::favicon))
        .route(
            "/ping",
            axum::routing::get(routes::system::ping)
                .layer(axum::middleware::map_response(http::with_json_charset)),
        )
        .route("/Content/{*path}", axum::routing::get(ui::content))
        .route("/content/{*path}", axum::routing::get(ui::content))
        .route("/login", axum::routing::get(ui::login_page).post(ui::login))
        .route("/logout", axum::routing::get(ui::logout))
        .layer(http::compression())
        // SignalR's negotiation is not compressed, and not to be kept.
        .merge(routes::ui_router(&state).layer(axum::middleware::map_response(http::no_cache)));

    // The calendar feed: opened with the API key, as the API is.
    let feed = routes::feed_router(&state)
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), http::finish))
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

    let url_base = state.settings().url_base.clone();
    let app = if url_base.is_empty() {
        app
    } else {
        Router::new().nest(&url_base, app)
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
            normalize_path,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            hosts::require_allowed_host,
        ))
}
