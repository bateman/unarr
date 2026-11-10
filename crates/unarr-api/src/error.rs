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
    /// `NzbDroneClientException(NotFound, message)`: a 404 with the message
    /// and the exception's description.
    NotFoundMessage(String),
    /// `NzbDroneClientException(InternalServerError, message)`.
    ServerMessage(String),
    /// `NzbDroneClientException(Conflict, message)`.
    ConflictMessage(String),
    /// `ModelNotFoundException`: a 404 with the message and a description.
    ModelNotFound(String),
    /// `NotFoundException` thrown by a controller: `{"message": "NotFound"}`.
    NotFoundException,
    /// `BadRequestException`: `{"message": "BadRequest: ...", "content": "..."}`.
    BadRequest(String),
    /// `ValidationException`: a 400 listing the failures.
    Validation(Vec<unarr_core::validation::ValidationFailure>),
    /// A `ValidationException` a service raised with bare
    /// `ValidationFailure`s: only what is set shows.
    ValidationPlain(Vec<unarr_core::validation::ValidationFailure>),
    /// An exception the originals let escape: a 500 with its message and
    /// a description that names it.
    ServerException {
        message: String,
        exception: String,
    },
    /// An `NzbDroneClientException` subclass with `BadRequest`: a 400 with
    /// its message and a description that names it.
    ClientException {
        message: String,
        exception: String,
    },
    /// `MethodNotAllowedException`: a 405 with message and content.
    MethodNotAllowed(String),
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
                Json(json!({ "message": format!("BadRequest: {message}"), "content": message })),
            )
                .into_response(),
            ApiError::NotFoundMessage(message) => {
                (StatusCode::NOT_FOUND, Json(client_exception(&message))).into_response()
            }
            ApiError::ServerMessage(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(client_exception(&message)),
            )
                .into_response(),
            ApiError::ConflictMessage(message) => {
                (StatusCode::CONFLICT, Json(client_exception(&message))).into_response()
            }
            ApiError::ModelNotFound(message) => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": message,
                    "description": format!("NzbDrone.Core.Datastore.ModelNotFoundException: {message}"),
                })),
            )
                .into_response(),
            ApiError::NotFoundException => {
                (StatusCode::NOT_FOUND, Json(json!({"message": "NotFound"}))).into_response()
            }
            ApiError::Validation(failures) => {
                let body: Vec<serde_json::Value> = failures.iter().map(|f| f.to_json()).collect();
                (StatusCode::BAD_REQUEST, Json(body)).into_response()
            }
            ApiError::ValidationPlain(failures) => {
                let body: Vec<serde_json::Value> = failures.iter().map(|f| f.to_plain_json()).collect();
                (StatusCode::BAD_REQUEST, Json(body)).into_response()
            }
            ApiError::ServerException { message, exception } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "message": message,
                    "description": format!("{exception}: {message}"),
                })),
            )
                .into_response(),
            ApiError::ClientException { message, exception } => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": message,
                    "description": format!("{exception}: {message}"),
                })),
            )
                .into_response(),
            ApiError::MethodNotAllowed(message) => (
                StatusCode::METHOD_NOT_ALLOWED,
                Json(json!({ "message": format!("MethodNotAllowed: {message}"), "content": message })),
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

/// The body of an `NzbDroneClientException`: the message, and a description
/// that the originals fill with the exception and its stack.
fn client_exception(message: &str) -> serde_json::Value {
    json!({
        "message": message,
        "description": format!("NzbDrone.Core.Exceptions.NzbDroneClientException: {message}"),
    })
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
