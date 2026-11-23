//! Provider answers as Radarr 7 gives them. The handlers are the shared
//! ones; what differs is the answer to a test that passed (`NoContent`),
//! and to a bulk update (`Ok` where Sonarr says `Accepted`).

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::error::ApiResult;
use crate::routes::{
    downloadclient_writes, importlist_writes, indexer_writes, metadata_writes, notification_writes,
    settings_writes,
};
use crate::state::AppState;

/// An action of Radarr's that returns a string, as its framework
/// negotiates it (asked of the real Radarr 6.4.4): the text itself as
/// `text/plain` for a client that sends no `Accept`, one with `*/*` in it
/// (a browser, the UI) or one that asks for plain text; the text as a JSON
/// string for a client that asks for JSON alone (Prowlarr); 406 for one
/// that asks for neither.
pub fn string_answer(headers: &HeaderMap, text: &str) -> Response {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or("");
    let accepts = |media: &str| accept.split(',').any(|part| part.trim().starts_with(media));
    if accept.is_empty() || accepts("*/*") || accepts("text/plain") || accepts("text/*") {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            text.to_string(),
        )
            .into_response()
    } else if accepts("application/json") || accepts("application/*") {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            Value::String(text.to_string()).to_string(),
        )
            .into_response()
    } else {
        StatusCode::NOT_ACCEPTABLE.into_response()
    }
}

/// Radarr 7's `Test` action answers `TypedResults.NoContent()`, where
/// Radarr 6 returned the string `{}`. Neither Prowlarr nor the UI reads the
/// body; both read the status.
fn passed(_headers: &HeaderMap) -> Response {
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
pub async fn notification_test(
    state: State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<Response> {
    let Json(_) = notification_writes::test(state, uri, body).await?;
    Ok(passed(&headers))
}

/// `POST /importlist/test`.
pub async fn import_list_test(
    state: State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<Response> {
    let Json(_) = importlist_writes::test(state, uri, body).await?;
    Ok(passed(&headers))
}

/// `POST /metadata/test`.
pub async fn metadata_test(
    state: State<AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<Response> {
    let Json(_) = metadata_writes::test(state, body).await?;
    Ok(passed(&headers))
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

/// `GET /localization`: the strings, which Radarr returns as a string.
pub async fn localization(headers: HeaderMap) -> Response {
    string_answer(&headers, &crate::routes::system::radarr_localization())
}

/// `POST /indexer/test`.
pub async fn indexer_test(
    state: State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<Response> {
    let Json(_) = indexer_writes::test(state, uri, body).await?;
    Ok(passed(&headers))
}

/// `POST /downloadclient/test`.
pub async fn download_client_test(
    state: State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<Response> {
    let Json(_) = downloadclient_writes::test(state, uri, body).await?;
    Ok(passed(&headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(accept: Option<&str>) -> (StatusCode, String) {
        let mut headers = HeaderMap::new();
        if let Some(accept) = accept {
            headers.insert(header::ACCEPT, accept.parse().unwrap());
        }
        let response = string_answer(&headers, "{}");
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (response.status(), content_type)
    }

    /// As the real Radarr 6.4.4 answered `POST /indexer/test` for each.
    #[test]
    fn strings_are_negotiated_as_the_original_does() {
        let plain = "text/plain; charset=utf-8".to_string();
        let json = "application/json; charset=utf-8".to_string();
        assert_eq!(answer(None), (StatusCode::OK, plain.clone()));
        assert_eq!(answer(Some("*/*")), (StatusCode::OK, plain.clone()));
        assert_eq!(
            answer(Some("application/json, text/javascript, */*; q=0.01")),
            (StatusCode::OK, plain.clone())
        );
        assert_eq!(answer(Some("text/plain")), (StatusCode::OK, plain));
        assert_eq!(answer(Some("application/json")), (StatusCode::OK, json));
        assert_eq!(answer(Some("application/xml")).0, StatusCode::NOT_ACCEPTABLE);
    }
}
