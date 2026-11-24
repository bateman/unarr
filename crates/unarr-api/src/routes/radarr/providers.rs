//! Provider answers as Radarr 7 gives them. The handlers are the shared
//! ones; what differs is the answer to a test that passed (`NoContent`),
//! and to a bulk update (`Ok` where Sonarr says `Accepted`).

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::error::ApiResult;
use crate::routes::{
    downloadclient_writes, importlist_writes, indexer_writes, metadata_writes, notification_writes,
    settings_writes,
};
use crate::state::AppState;

/// Radarr 7's `Test` action answers `TypedResults.NoContent()`, where
/// Radarr 6 returned the string `{}`. Neither Prowlarr nor the UI reads the
/// body; both read the status.
fn passed() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// A shared handler's `Accepted` as Radarr 7's `TypedResults.Ok`, for the
/// actions whose status that change moved; any other answer is left alone.
fn ok_instead_of_accepted(mut response: Response) -> Response {
    if response.status() == StatusCode::ACCEPTED {
        *response.status_mut() = StatusCode::OK;
    }
    response
}

/// `POST /notification/test`.
pub async fn notification_test(state: State<AppState>, uri: Uri, body: Json<Value>) -> ApiResult<Response> {
    let Json(_) = notification_writes::test(state, uri, body).await?;
    Ok(passed())
}

/// `POST /importlist/test`.
pub async fn import_list_test(state: State<AppState>, uri: Uri, body: Json<Value>) -> ApiResult<Response> {
    let Json(_) = importlist_writes::test(state, uri, body).await?;
    Ok(passed())
}

/// `POST /metadata/test`.
pub async fn metadata_test(state: State<AppState>, body: Json<Value>) -> ApiResult<Response> {
    let Json(_) = metadata_writes::test(state, body).await?;
    Ok(passed())
}

/// `PUT /indexer/bulk`.
pub async fn indexer_bulk_update(state: State<AppState>, body: Json<Value>) -> ApiResult<Response> {
    indexer_writes::bulk_update(state, body)
        .await
        .map(ok_instead_of_accepted)
}

/// `PUT /downloadclient/bulk`.
pub async fn download_client_bulk_update(state: State<AppState>, body: Json<Value>) -> ApiResult<Response> {
    downloadclient_writes::bulk_update(state, body)
        .await
        .map(ok_instead_of_accepted)
}

/// `PUT /importlist/bulk`.
pub async fn import_list_bulk_update(state: State<AppState>, body: Json<Value>) -> ApiResult<Response> {
    importlist_writes::bulk_update(state, body)
        .await
        .map(ok_instead_of_accepted)
}

/// `PUT /remotepathmapping/{id}`.
pub async fn remote_path_mapping_update(
    state: State<AppState>,
    id: axum::extract::Path<i64>,
    body: Json<Value>,
) -> ApiResult<Response> {
    settings_writes::remote_path_mapping_update(state, id, body)
        .await
        .map(ok_instead_of_accepted)
}

/// `PUT /customformat/bulk`.
pub async fn custom_format_bulk_update(state: State<AppState>, body: Json<Value>) -> ApiResult<Response> {
    settings_writes::custom_format_bulk_update(state, body)
        .await
        .map(ok_instead_of_accepted)
}

/// `GET /localization`: the strings. Radarr 7 answers them with
/// `TypedResults.Json` (`[Produces("application/json")]`), where Radarr 6
/// returned them as a string, served as `text/plain`. An `IResult` is not
/// negotiated: JSON whatever the `Accept`.
pub async fn localization() -> Response {
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        crate::routes::system::radarr_localization(),
    )
        .into_response();
    response.extensions_mut().insert(crate::http::NotNegotiated);
    response
}

/// `POST /indexer/test`.
pub async fn indexer_test(state: State<AppState>, uri: Uri, body: Json<Value>) -> ApiResult<Response> {
    let Json(_) = indexer_writes::test(state, uri, body).await?;
    Ok(passed())
}

/// `POST /downloadclient/test`.
pub async fn download_client_test(
    state: State<AppState>,
    uri: Uri,
    body: Json<Value>,
) -> ApiResult<Response> {
    let Json(_) = downloadclient_writes::test(state, uri, body).await?;
    Ok(passed())
}
