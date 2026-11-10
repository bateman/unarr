//! API key authentication, as the originals do it: the key travels in the
//! `X-Api-Key` header or the `apikey` query parameter (the query parameter is
//! checked first), and a missing or wrong key gets an empty 401.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// The API key presented by a request, if any.
pub fn presented_key(uri: &Uri, headers: &HeaderMap) -> Option<String> {
    if let Some(query) = uri.query() {
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if k.eq_ignore_ascii_case("apikey") {
                return Some(percent_decode(v));
            }
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Decode `%XX` escapes and `+` as the originals' query parser does.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match s
                    .get(i + 1..i + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Constant-time comparison, so timing does not leak the key.
pub fn key_matches(expected: &str, presented: &str) -> bool {
    let a = expected.as_bytes();
    let b = presented.as_bytes();
    // No key is no key: a blank one configured by mistake opens nothing.
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Middleware guarding the API routes.
pub async fn require_api_key(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let presented = presented_key(request.uri(), request.headers());
    match presented {
        Some(key) if key_matches(&state.settings().host.api_key, &key) => next.run(request).await,
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_key_in_query_or_header() {
        let headers = HeaderMap::new();
        let uri: Uri = "/api/v3/system/status?apikey=abc".parse().unwrap();
        assert_eq!(presented_key(&uri, &headers).as_deref(), Some("abc"));
        let uri: Uri = "/api/v3/system/status?x=1&ApiKey=a%20b".parse().unwrap();
        assert_eq!(presented_key(&uri, &headers).as_deref(), Some("a b"));
        let uri: Uri = "/api/v3/system/status".parse().unwrap();
        assert_eq!(presented_key(&uri, &headers), None);
        let mut headers = HeaderMap::new();
        headers.insert("X-Api-Key", "hdr".parse().unwrap());
        assert_eq!(presented_key(&uri, &headers).as_deref(), Some("hdr"));
        assert!(key_matches("abc", "abc"));
        assert!(!key_matches("abc", "abd"));
        assert!(!key_matches("abc", "ab"));
        assert!(!key_matches("", ""));
    }
}
