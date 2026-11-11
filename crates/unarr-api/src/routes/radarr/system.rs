//! What Radarr answers about itself where it differs from the shared
//! handlers, and what it answers while it runs no command queue of its own.

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

/// `GET /command`: Radarr queues nothing until the acquisition milestone
/// gives it its commands.
pub async fn commands() -> Json<Vec<Value>> {
    Json(Vec::new())
}

/// `GET /queue`: empty, in the originals' paging.
pub async fn queue(uri: axum::http::Uri) -> Json<Value> {
    let p = crate::query::Params::from_uri(&uri);
    Json(json!({
        "page": p.i64("page").unwrap_or(1).max(1),
        "pageSize": p.i64("pageSize").unwrap_or(10).max(1),
        "sortKey": p.get("sortKey").unwrap_or("timeleft"),
        "sortDirection": if p.descending().unwrap_or(true) { "descending" } else { "ascending" },
        "totalRecords": 0,
        "records": [],
    }))
}

pub async fn queue_status() -> Json<Value> {
    Json(json!({
        "totalCount": 0,
        "count": 0,
        "unknownCount": 0,
        "errors": false,
        "warnings": false,
        "unknownErrors": false,
        "unknownWarnings": false,
    }))
}

pub async fn queue_details() -> Json<Vec<Value>> {
    Json(Vec::new())
}
