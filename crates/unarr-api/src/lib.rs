//! Compatible HTTP surface. One router per personality, mounted under the
//! original API prefix, plus `/signalr` so the official web UIs can connect.
//!
//! Every response reproduces the JSON shape of the original application:
//! camelCase, the same field names and nesting, `null` fields omitted. The
//! contract tests under `tests/` replay responses recorded from the real
//! application against this router.

pub mod auth;
mod blocking;
pub mod compat;
pub mod error;
pub mod fs;
pub mod hosts;
pub mod json;
pub mod logbuf;
pub mod mediainfo;
pub mod query;
pub mod resources;
pub mod routes;
pub mod schemas;
pub mod state;
pub mod ui;

use axum::Router;
use tower_http::trace::TraceLayer;

pub use logbuf::LogBuffer;
pub use state::{AppState, RuntimeInfo, Settings};

/// The originals route API paths case-insensitively (Bazarr asks for
/// `episodeFile`); lower-case the path before matching.
async fn lowercase_api_path(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let uri = request.uri().clone();
    let path = uri.path();
    let lower = path.to_ascii_lowercase();
    // Only the API part: a URL base such as /Sonarr keeps its case.
    if let Some(i) = lower.find("/api/v") {
        if path[i..].chars().any(|c| c.is_ascii_uppercase()) {
            let new_path = format!("{}{}", &path[..i], &lower[i..]);
            let lowered = match uri.query() {
                Some(q) => format!("{new_path}?{q}"),
                None => new_path,
            };
            if let Ok(new_uri) = lowered.parse() {
                *request.uri_mut() = new_uri;
            }
        }
    }
    next.run(request).await
}

/// Build the router for one personality.
pub fn router(state: AppState) -> Router {
    let api = routes::api_router(&state).route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::require_api_key,
    ));

    // Pages and bootstrap the browser loads: behind the UI policy.
    let pages = Router::new()
        .route("/", axum::routing::get(ui::spa))
        .route(
            "/initialize.json",
            axum::routing::get(routes::system::initialize_json),
        )
        .nest_service(
            "/MediaCover",
            tower_http::services::ServeDir::new(state.runtime().media_cover_dir.clone()),
        )
        .fallback(axum::routing::get(ui::spa))
        // `layer`, not `route_layer`: the guard must cover the fallback too,
        // or deep links such as /settings would skip the login.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ui::require_ui_auth,
        ));

    // Open to anonymous browsers: static content, the login form, SignalR
    // (which checks its own token).
    let open = Router::new()
        .route("/favicon.ico", axum::routing::get(ui::favicon))
        .route("/Content/{*path}", axum::routing::get(ui::content))
        .route("/content/{*path}", axum::routing::get(ui::content))
        .route("/login", axum::routing::get(ui::login_page).post(ui::login))
        .route("/logout", axum::routing::get(ui::logout))
        .merge(routes::ui_router(&state));

    let app = Router::new()
        .nest(state.personality().api_prefix(), api)
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
        .layer(axum::middleware::from_fn(lowercase_api_path))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            hosts::require_allowed_host,
        ))
}
