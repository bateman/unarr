//! Provider tests as Radarr answers them. The handlers are the shared
//! ones; what differs is the answer to a test that passed.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::error::ApiResult;
use crate::routes::{downloadclient_writes, indexer_writes};
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

/// Radarr's `Test` action returns the string `{}`, not an object. Neither
/// Prowlarr nor the UI reads it; both read the status.
fn passed(headers: &HeaderMap) -> Response {
    string_answer(headers, "{}")
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
