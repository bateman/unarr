//! Error responses in the originals' shape (ASP.NET problem details for 404,
//! a message object for 500).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Failure of a handler.
#[derive(Debug)]
pub enum ApiError {
    NotFound,
    BadRequest(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "type": "https://tools.ietf.org/html/rfc7231#section-6.5.4",
                    "title": "Not Found",
                    "status": 404,
                    "traceId": trace_id(),
                })),
            )
                .into_response(),
            ApiError::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "message": message, "description": message })),
            )
                .into_response(),
            ApiError::Internal(e) => {
                tracing::error!(error = ?e, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "message": e.to_string(),
                        "description": format!("{e:#}"),
                    })),
                )
                    .into_response()
            }
        }
    }
}

/// A W3C trace-context style id, as ASP.NET emits in problem details.
fn trace_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("00-{:032x}-{:016x}-00", nanos, nanos as u64 ^ 0x5bd1_e995)
}

/// Handler result type.
pub type ApiResult<T> = Result<T, ApiError>;

/// `Option` to 404 helper.
pub fn found<T>(v: Option<T>) -> ApiResult<T> {
    v.ok_or(ApiError::NotFound)
}
