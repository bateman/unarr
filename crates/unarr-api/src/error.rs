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
    /// `ModelConflictException`: a 409 with the message and a description.
    ModelConflict(String),
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
    /// An exception the originals answer 503 for (the metadata server
    /// could not be asked): its message and a description that names it.
    Unavailable {
        message: String,
        exception: String,
    },
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Everything but `NotFound` is an exception in the originals, and
        // the exception handler writes its answer.
        let thrown = !matches!(self, ApiError::NotFound);
        let mut response = self.response();
        if thrown {
            response.extensions_mut().insert(Thrown);
        }
        response
    }
}

impl ApiError {
    fn response(self) -> Response {
        match self {
            ApiError::NotFound => {
                let mut response =
                    not_found_problem(NOT_FOUND_TYPE_RFC7231, "application/json; charset=utf-8");
                // Marked, so a personality whose original names another
                // document can say so (`lib.rs`).
                response.extensions_mut().insert(NotFoundProblem);
                response
            }
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
            ApiError::ModelConflict(message) => (
                StatusCode::CONFLICT,
                Json(json!({
                    "message": message,
                    "description": format!("NzbDrone.Core.Datastore.ModelConflictException: {message}"),
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
            ApiError::Unavailable { message, exception } => (
                StatusCode::SERVICE_UNAVAILABLE,
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

/// The answer to a request whose handling panicked: what the originals'
/// exception handler writes for an exception nothing caught, a 500 with the
/// message and a description that says what was raised and where
/// (recorded from Sonarr 4.0.20 and Radarr 6.4.4 for an
/// `InvalidOperationException` in a controller; theirs holds the .NET
/// stack, this one the place of the panic).
pub fn panicked(panic: &unarr_core::panics::Panicked) -> Response {
    let mut response = (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "message": panic.message,
            "description": panic.to_string(),
        })),
    )
        .into_response();
    response.extensions_mut().insert(Thrown);
    response
}

/// Marks an answer the originals' exception handler writes: it starts from
/// an empty response, so what the pipeline had set is gone (`http.rs`).
#[derive(Debug, Clone, Copy)]
pub struct Thrown;

/// Marks the response of [`ApiError::NotFound`].
#[derive(Debug, Clone, Copy)]
pub struct NotFoundProblem;

/// The document ASP.NET's problem details point at for a 404: Sonarr's
/// runtime names RFC 7231, Radarr's newer one RFC 9110.
pub const NOT_FOUND_TYPE_RFC7231: &str = "https://tools.ietf.org/html/rfc7231#section-6.5.4";
pub const NOT_FOUND_TYPE_RFC9110: &str = "https://tools.ietf.org/html/rfc9110#section-15.5.5";

/// ASP.NET's problem details for a resource that is not there. Sonarr's
/// runtime sends them as `application/json`, Radarr's as
/// `application/problem+json`.
pub fn not_found_problem(document: &str, content_type: &'static str) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(axum::http::header::CONTENT_TYPE, content_type)],
        // Indented, its members in ASP.NET's order.
        format!(
            "{{\n  \"type\": {},\n  \"title\": \"Not Found\",\n  \"status\": 404,\n  \"traceId\": {}\n}}",
            json!(document),
            json!(trace_id()),
        ),
    )
        .into_response()
}

/// ASP.NET's validation problem details: what model binding answers when
/// it cannot read a body or a query, `errors` naming each field. `document`
/// is the RFC section the personality's runtime points at.
pub fn validation_problem(document: &str, content_type: &'static str, errors: serde_json::Value) -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(axum::http::header::CONTENT_TYPE, content_type)],
        json!({
            "type": document,
            "title": "One or more validation errors occurred.",
            "status": 400,
            "errors": errors,
            "traceId": trace_id(),
        })
        .to_string(),
    )
        .into_response()
}

/// The section a 400 of Radarr's runtime names; Sonarr's names RFC 7231.
pub const BAD_REQUEST_TYPE_RFC9110: &str = "https://tools.ietf.org/html/rfc9110#section-15.5.1";
pub const BAD_REQUEST_TYPE_RFC7231: &str = "https://tools.ietf.org/html/rfc7231#section-6.5.1";

/// The body of an `NzbDroneClientException`: the message, and a description
/// that the originals fill with the exception and its stack.
fn client_exception(message: &str) -> serde_json::Value {
    json!({
        "message": message,
        "description": format!("NzbDrone.Core.Exceptions.NzbDroneClientException: {message}"),
    })
}

/// A W3C trace-context style id, as ASP.NET emits in problem details.
pub(crate) fn trace_id() -> String {
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
