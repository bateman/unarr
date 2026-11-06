//! Compatible HTTP surface. One router per personality, mounted under the
//! original API prefix, plus `/signalr` so the official web UIs can connect.

use axum::{routing::get, Json, Router};
use serde::Serialize;
use unarr_core::Personality;

/// Subset of `GET /api/v{1,3}/system/status` that Prowlarr and Bazarr read.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    app_name: &'static str,
    instance_name: &'static str,
    version: &'static str,
    is_production: bool,
    runtime_name: &'static str,
}

/// Build the router for one personality.
pub fn router(personality: Personality) -> Router {
    let status = move || async move {
        Json(SystemStatus {
            app_name: match personality {
                Personality::Sonarr => "Sonarr",
                Personality::Radarr => "Radarr",
                Personality::Lidarr => "Lidarr",
            },
            instance_name: "Unarr",
            version: env!("CARGO_PKG_VERSION"),
            is_production: true,
            runtime_name: "unarr",
        })
    };
    Router::new().route(
        &format!("{}/system/status", personality.api_prefix()),
        get(status),
    )
}
