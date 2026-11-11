//! What Radarr answers about itself where it differs from the shared
//! handlers.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::error::ApiResult;
use crate::state::AppState;

/// `SystemResource`, with the one key Radarr has and Sonarr does not.
pub async fn status(state: State<AppState>) -> ApiResult<Json<Value>> {
    let is_docker = state.runtime().is_docker;
    let Json(resource) = crate::routes::system::status(state).await?;
    let mut out = serde_json::to_value(resource).map_err(anyhow::Error::from)?;
    out["isContainerized"] = json!(is_docker);
    Ok(Json(out))
}

/// `GET /api`: the API versions this application speaks.
pub async fn api_root() -> Json<Value> {
    Json(json!({"current": "v3", "deprecated": []}))
}
