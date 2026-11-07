//! The official web UI, embedded in the binary, and the browser-facing
//! authentication around it (forms login with a signed cookie, basic auth,
//! local-address bypass), as the originals do it.
//!
//! The UI bundle is copied into `crates/unarr-api/ui/sonarr` by `make ui`
//! (GPL-3.0, like Unarr) and embedded by `rust-embed`; source maps are left
//! out. Without the bundle the daemon still runs and `/` says how to get it.

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Form;
use base64::Engine;
use rust_embed::Embed;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use unarr_core::config::{AuthenticationMethod, AuthenticationRequired};
use unarr_core::model::User;
use unarr_core::store;

use crate::auth::key_matches;
use crate::state::AppState;

#[derive(Embed)]
#[folder = "ui/sonarr/"]
#[exclude = "*.map"]
#[exclude = ".gitkeep"]
struct Assets;

/// Whether `make ui` has been run for this build.
pub fn available() -> bool {
    Assets::get("index.html").is_some()
}

fn serve_asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = file.metadata.mimetype();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    // Hashed bundle names never change content; everything else may.
    let cache = if path.contains('-') && (path.ends_with(".js") || path.ends_with(".css")) {
        "max-age=31536000, public"
    } else {
        "no-cache"
    };
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Some((headers, Body::from(file.data.into_owned())).into_response())
}

/// `index.html` with the URL base applied to asset links and to the
/// `window.Sonarr` bootstrap, as the originals' `IndexHtmlMapper` does.
fn index_html(url_base: &str) -> Option<String> {
    let file = Assets::get("index.html")?;
    let text = String::from_utf8_lossy(&file.data).into_owned();
    Some(rewrite_html(&text, url_base).replace("__URL_BASE__", url_base))
}

fn login_html(url_base: &str, theme: &str) -> Option<String> {
    let file = Assets::get("login.html")?;
    let text = String::from_utf8_lossy(&file.data).into_owned();
    Some(
        rewrite_html(&text, url_base)
            .replace("_THEME_", theme)
            .replace("__URL_BASE__", url_base),
    )
}

/// Prefix root-relative `href`/`src` attributes with the URL base.
fn rewrite_html(html: &str, url_base: &str) -> String {
    if url_base.is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() + 256);
    let mut rest = html;
    while let Some(pos) = rest.find("=\"/") {
        let attr_start = rest[..pos]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let attr = &rest[attr_start..pos];
        out.push_str(&rest[..pos + 2]);
        if attr == "href" || attr == "src" {
            out.push_str(url_base);
        }
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

fn html(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store"),
        ],
        body,
    )
        .into_response()
}

fn no_ui_page() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Unarr is running, but this build has no web UI embedded.\nRun `make ui` and rebuild to include the official frontend.\n",
    )
        .into_response()
}

// --- pages

/// Everything under `/` that is not an API, content, cover or SignalR path:
/// the single-page application, or one of the bundle files at the root.
pub async fn spa(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if !path.is_empty() && path.contains('.') && !path.contains("..") {
        if let Some(r) = serve_asset(path) {
            return r;
        }
        return StatusCode::NOT_FOUND.into_response();
    }
    match index_html(&state.settings().url_base) {
        Some(page) => html(page),
        None => no_ui_page(),
    }
}

pub async fn content(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    // Case-insensitive prefix, as the originals route it.
    let rel = path.get("content/".len()..).unwrap_or("");
    if rel.contains("..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_asset(&format!("Content/{rel}")).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

pub async fn favicon() -> Response {
    serve_asset("Content/Images/Icons/favicon.ico")
        .or_else(|| serve_asset("Content/Images/Icons/favicon-32x32.png"))
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

// --- login

#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default, rename = "rememberMe")]
    remember_me: String,
}

pub async fn login_page(State(state): State<AppState>) -> Response {
    let s = state.settings();
    match login_html(&s.url_base, &s.host.theme) {
        Some(page) => html(page),
        None => no_ui_page(),
    }
}

pub async fn login(State(state): State<AppState>, uri: Uri, Form(form): Form<LoginForm>) -> Response {
    let url_base = state.settings().url_base.clone();
    let return_url = crate::query::Params::from_uri(&uri)
        .get("returnUrl")
        .map(str::to_string)
        .unwrap_or_default();
    let username = form.username.to_lowercase();
    let password = form.password.clone();
    let user = state
        .db()
        .call(move |conn| {
            Ok(store::providers::list_users(conn)?
                .into_iter()
                .find(|u| u.username.to_lowercase() == username))
        })
        .await
        .ok()
        .flatten();
    // PBKDF2 is slow by design: not on an async worker.
    let user = match user {
        Some(u) => crate::blocking::run(move || verify_password(&u, &password).then_some(u))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let Some(user) = user else {
        return found(&format!(
            "{url_base}/login?returnUrl={}&loginFailed=true",
            percent_encode(&return_url)
        ));
    };
    let persistent = form.remember_me == "on";
    let cookie = issue_cookie(&state, &user, persistent);
    // Only a local path: not another host (`//host`), no backslash, which
    // browsers read as a slash (`/\host`), and no control character, which
    // browsers strip (`/<tab>/host` would become `//host`).
    let local = return_url.starts_with('/')
        && !return_url.starts_with("//")
        && !return_url.contains('\\')
        && !return_url.chars().any(char::is_control);
    let target = if return_url.is_empty() || !local {
        format!("{url_base}/")
    } else if url_base.is_empty() || return_url.starts_with(&url_base) {
        return_url
    } else {
        format!("{url_base}{return_url}")
    };
    let mut response = found(&target);
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    response
}

pub async fn logout(State(state): State<AppState>) -> Response {
    let name = cookie_name(&state);
    let expired = format!(
        "{name}=; Path={}/; Max-Age=0; HttpOnly; SameSite=Lax",
        state.settings().url_base
    );
    let mut response = found(&format!("{}/", state.settings().url_base));
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&expired).unwrap());
    response
}

// --- password verification, as the originals store it

/// PBKDF2-HMAC-SHA512 with the stored salt and iteration count (32 bytes,
/// base64), or the legacy unsalted SHA-256 hex digest.
pub fn verify_password(user: &User, password: &str) -> bool {
    match &user.salt {
        Some(salt) if !salt.trim().is_empty() => {
            let Ok(salt) = base64::engine::general_purpose::STANDARD.decode(salt) else {
                return false;
            };
            let Ok(expected) = base64::engine::general_purpose::STANDARD.decode(&user.password) else {
                return false;
            };
            let iterations = user.iterations.unwrap_or(10_000).max(1) as u32;
            let mut out = vec![0u8; expected.len().max(1)];
            pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password.as_bytes(), &salt, iterations, &mut out);
            key_matches(&hex(&expected), &hex(&out))
        }
        _ => key_matches(
            &user.password.to_lowercase(),
            &hex(&Sha256::digest(password.as_bytes())),
        ),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- the session cookie

/// `<InstanceName>Auth`, letters and digits only, as the originals name it.
fn cookie_name(state: &AppState) -> String {
    let name: String = state
        .settings()
        .instance_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("{name}Auth")
}

/// HMAC key derived from the API key: rotating the key logs everyone out.
fn signing_key(state: &AppState) -> Vec<u8> {
    Sha256::digest(format!("unarr-cookie:{}", state.settings().host.api_key).as_bytes()).to_vec()
}

fn sign(key: &[u8], payload: &str) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn issue_cookie(state: &AppState, user: &User, persistent: bool) -> String {
    let ttl = if persistent { 7 * 86_400 } else { 86_400 };
    let expires = state.now().timestamp() + ttl;
    let payload = format!("{}|{}", user.identifier, expires);
    let token = format!("{payload}|{}", sign(&signing_key(state), &payload));
    let mut cookie = format!(
        "{}={}; Path={}/; HttpOnly; SameSite=Lax",
        cookie_name(state),
        token,
        state.settings().url_base
    );
    if persistent {
        cookie.push_str(&format!("; Max-Age={ttl}"));
    }
    cookie
}

fn cookie_valid(state: &AppState, headers: &HeaderMap) -> bool {
    let name = cookie_name(state);
    let Some(cookies) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(token) = cookies
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix(&format!("{name}=")))
    else {
        return false;
    };
    let mut parts = token.rsplitn(2, '|');
    let (Some(signature), Some(payload)) = (parts.next(), parts.next()) else {
        return false;
    };
    if !key_matches(&sign(&signing_key(state), payload), signature) {
        return false;
    }
    payload
        .rsplit('|')
        .next()
        .and_then(|e| e.parse::<i64>().ok())
        .is_some_and(|expires| expires > state.now().timestamp())
}

/// Basic authentication sends the password on every request, posters
/// included: the user is read on the database thread and the slow hash runs
/// on a blocking worker, never on the async ones.
async fn basic_auth_valid(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let decoded = String::from_utf8_lossy(&decoded).into_owned();
    let Some((username, password)) = decoded.split_once(':') else {
        return false;
    };
    let username = username.to_lowercase();
    let password = password.to_string();
    let user = state
        .db()
        .call(move |conn| {
            Ok(store::providers::list_users(conn)?
                .into_iter()
                .find(|u| u.username.to_lowercase() == username))
        })
        .await
        .ok()
        .flatten();
    match user {
        Some(u) => crate::blocking::run(move || verify_password(&u, &password))
            .await
            .unwrap_or(false),
        None => false,
    }
}

fn is_local_address(ip: std::net::IpAddr) -> bool {
    let ip = match ip {
        std::net::IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(std::net::IpAddr::V4)
            .unwrap_or(std::net::IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || (v6.segments()[0] & 0xffc0) == 0xfe80 || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Guard for the pages and bootstrap the browser loads: with forms or basic
/// authentication a valid session is required (the API key does not open
/// pages, as in the originals) unless the client is a local address the
/// configuration exempts.
pub async fn require_ui_auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let host = &state.settings().host;
    let needs_auth = matches!(
        host.authentication_method,
        AuthenticationMethod::Forms | AuthenticationMethod::Basic
    );
    if !needs_auth {
        return next.run(request).await;
    }
    let headers = request.headers();
    let forwarded = headers.contains_key("x-forwarded-for");
    let remote = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    let exempt = match host.authentication_required {
        AuthenticationRequired::Enabled => false,
        AuthenticationRequired::DisabledForLocalAddresses => {
            !forwarded && remote.is_some_and(is_local_address)
        }
        AuthenticationRequired::DisabledForLocalhost => {
            !forwarded && remote.is_some_and(|ip| ip.is_loopback())
        }
    };
    // Exempt clients skip the credential check, which is slow for Basic.
    if exempt {
        return next.run(request).await;
    }
    let session_ok = match host.authentication_method {
        AuthenticationMethod::Forms => cookie_valid(&state, headers),
        AuthenticationMethod::Basic => basic_auth_valid(&state, headers).await,
        _ => true,
    };
    if session_ok {
        return next.run(request).await;
    }
    match host.authentication_method {
        AuthenticationMethod::Basic => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"Unarr\"")],
        )
            .into_response(),
        _ => {
            let return_url = request
                .uri()
                .path_and_query()
                .map(|p| p.to_string())
                .unwrap_or_default();
            found(&format!(
                "{}/login?returnUrl={}",
                state.settings().url_base,
                percent_encode(&return_url)
            ))
        }
    }
}

/// A 302 to `location`, as ASP.NET's cookie authentication answers.
fn found(location: &str) -> Response {
    (
        StatusCode::FOUND,
        [(
            header::LOCATION,
            HeaderValue::from_str(location).unwrap_or(HeaderValue::from_static("/")),
        )],
    )
        .into_response()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_root_links_with_the_url_base() {
        let html = r#"<link href="/Content/styles.css"><script src="/index-1.js"></script><a href="https://x/y">x</a>"#;
        assert_eq!(rewrite_html(html, ""), html);
        let out = rewrite_html(html, "/sonarr");
        assert!(out.contains(r#"href="/sonarr/Content/styles.css""#));
        assert!(out.contains(r#"src="/sonarr/index-1.js""#));
        assert!(out.contains(r#"href="https://x/y""#));
    }

    #[test]
    fn verifies_both_password_schemes() {
        // Legacy: unsalted SHA-256 hex.
        let legacy = User {
            id: 1,
            identifier: "id".into(),
            username: "admin".into(),
            password: hex(&Sha256::digest(b"secret")),
            salt: None,
            iterations: None,
        };
        assert!(verify_password(&legacy, "secret"));
        assert!(!verify_password(&legacy, "wrong"));

        // Current: PBKDF2-HMAC-SHA512, 32 bytes, base64 salt and hash.
        let salt = b"0123456789abcdef";
        let mut out = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha512>(b"secret", salt, 10_000, &mut out);
        let user = User {
            password: base64::engine::general_purpose::STANDARD.encode(out),
            salt: Some(base64::engine::general_purpose::STANDARD.encode(salt)),
            iterations: Some(10_000),
            ..legacy
        };
        assert!(verify_password(&user, "secret"));
        assert!(!verify_password(&user, "Secret"));
    }

    #[test]
    fn local_addresses() {
        assert!(is_local_address("127.0.0.1".parse().unwrap()));
        assert!(is_local_address("192.168.1.10".parse().unwrap()));
        assert!(is_local_address("::ffff:10.0.0.5".parse().unwrap()));
        assert!(is_local_address("fe80::1".parse().unwrap()));
        assert!(!is_local_address("8.8.8.8".parse().unwrap()));
    }
}
