//! The official web UI, embedded in the binary, and the browser-facing
//! authentication around it (forms login with a signed cookie, basic auth,
//! local-address bypass), as the originals do it.
//!
//! The UI bundles are copied into `crates/unarr-api/ui/sonarr` and
//! `ui/radarr` by `make ui` (GPL-3.0, like Unarr) and embedded by
//! `rust-embed`, one per personality; source maps are left out. Without a
//! bundle the daemon still runs and `/` says how to get it.

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
use unarr_core::Personality;

use crate::auth::key_matches;
use crate::state::AppState;

#[derive(Embed)]
#[folder = "ui/sonarr/"]
#[exclude = "*.map"]
#[exclude = ".gitkeep"]
struct SonarrAssets;

#[derive(Embed)]
#[folder = "ui/radarr/"]
#[exclude = "*.map"]
#[exclude = ".gitkeep"]
struct RadarrAssets;

/// A file of the frontend a personality serves. Lidarr's is not embedded
/// yet.
fn asset(personality: Personality, path: &str) -> Option<rust_embed::EmbeddedFile> {
    match personality {
        Personality::Sonarr => SonarrAssets::get(path),
        Personality::Radarr => RadarrAssets::get(path),
        Personality::Lidarr => None,
    }
}

/// Whether `make ui` has been run for this build.
pub fn available(personality: Personality) -> bool {
    asset(personality, "index.html").is_some()
}

fn serve_asset(personality: Personality, path: &str) -> Option<Response> {
    let file = asset(personality, path)?;
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
fn index_html(personality: Personality, url_base: &str) -> Option<String> {
    let file = asset(personality, "index.html")?;
    let text = String::from_utf8_lossy(&file.data).into_owned();
    Some(rewrite_html(&text, url_base).replace("__URL_BASE__", url_base))
}

fn login_html(personality: Personality, url_base: &str, theme: &str) -> Option<String> {
    let file = asset(personality, "login.html")?;
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

/// A page, as `StaticResourceController` sends it: no charset on the
/// content type (the pages name theirs), and never cached.
fn html(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html"),
            (header::CACHE_CONTROL, "no-cache, no-store"),
            (header::PRAGMA, "no-cache"),
            (header::EXPIRES, "-1"),
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
        if let Some(r) = serve_asset(state.personality(), path) {
            return r;
        }
        return StatusCode::NOT_FOUND.into_response();
    }
    match index_html(state.personality(), &state.settings().url_base) {
        Some(page) => html(page),
        None => no_ui_page(),
    }
}

pub async fn content(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    // Case-insensitive prefix, as the originals route it.
    let rel = path.get("content/".len()..).unwrap_or("");
    if rel.contains("..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_asset(state.personality(), &format!("Content/{rel}"))
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

pub async fn favicon(State(state): State<AppState>) -> Response {
    let personality = state.personality();
    serve_asset(personality, "Content/Images/Icons/favicon.ico")
        .or_else(|| serve_asset(personality, "Content/Images/Icons/favicon-32x32.png"))
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
    match login_html(state.personality(), &s.url_base, &s.host.theme) {
        Some(page) => html(page),
        None => no_ui_page(),
    }
}

pub async fn login(
    State(state): State<AppState>,
    remote: Option<axum::Extension<ConnectInfo<std::net::SocketAddr>>>,
    uri: Uri,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
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
    let secure = over_https(&state, remote.map(|c| c.0 .0.ip()), &headers);
    let cookie = issue_cookie(&state, &user.identifier, persistent, secure);
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
        "{name}=; Path={}; Max-Age=0; HttpOnly; SameSite=Lax",
        cookie_path(&state)
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

/// `<InstanceName>Auth`, as it was when the first session needed it.
fn cookie_name(state: &AppState) -> String {
    state.auth_cookie().to_owned()
}

/// The key the cookies are signed with.
fn signing_key(state: &AppState) -> Vec<u8> {
    Sha256::digest([b"unarr-cookie:".as_slice(), state.session_secret()].concat()).to_vec()
}

fn sign(key: &[u8], payload: &str) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// `ExpireTimeSpan`: seven days, counted again from each use
/// (`SlidingExpiration`).
const SESSION_SECONDS: i64 = 7 * 86_400;

/// A valid session, as its cookie says.
struct Session {
    identifier: String,
    /// "Remember Me": the cookie outlives the browser.
    persistent: bool,
    expires: i64,
}

/// The cookie's path: the URL base, or the root.
fn cookie_path(state: &AppState) -> String {
    let base = state.settings().url_base.clone();
    if base.is_empty() {
        "/".to_string()
    } else {
        base
    }
}

/// The session cookie for `identifier`, good for seven days from now.
/// Without "Remember Me" it carries no lifetime and goes with the browser,
/// though the session inside still ends after seven idle days. `secure`
/// when the request came over HTTPS (`CookieSecurePolicy.SameAsRequest`).
fn issue_cookie(state: &AppState, identifier: &str, persistent: bool, secure: bool) -> String {
    let expires = state.now().timestamp() + SESSION_SECONDS;
    let payload = format!("{identifier}|{}|{expires}", u8::from(persistent));
    let token = format!("{payload}|{}", sign(&signing_key(state), &payload));
    let mut cookie = format!(
        "{}={}; Path={}; HttpOnly; SameSite=Lax",
        cookie_name(state),
        token,
        cookie_path(state)
    );
    if secure {
        cookie.push_str("; Secure");
    }
    if persistent {
        cookie.push_str(&format!("; Max-Age={SESSION_SECONDS}"));
    }
    cookie
}

/// The session the request's cookie holds, when it is signed by this
/// installation and not expired.
fn session(state: &AppState, headers: &HeaderMap) -> Option<Session> {
    let name = cookie_name(state);
    let cookies = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())?;
    let token = cookies
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix(&format!("{name}=")))?;
    let (payload, signature) = token.rsplit_once('|')?;
    if !key_matches(&sign(&signing_key(state), payload), signature) {
        return None;
    }
    let (rest, expires) = payload.rsplit_once('|')?;
    let expires: i64 = expires.parse().ok()?;
    if expires <= state.now().timestamp() {
        return None;
    }
    let (identifier, persistent) = rest.rsplit_once('|')?;
    Some(Session {
        identifier: identifier.to_string(),
        persistent: persistent == "1",
        expires,
    })
}

/// Whether the request reached the proxy in front over HTTPS: the peer is
/// one `TrustedNetworks` believes and it says so.
fn over_https(state: &AppState, remote: Option<std::net::IpAddr>, headers: &HeaderMap) -> bool {
    remote.is_some_and(|peer| state.settings().trusted_networks.contains(peer))
        && headers
            .get_all("x-forwarded-proto")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .next_back()
            .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

/// `scheme://host` as the client names this server: the `Host` header, or
/// what a proxy `TrustedNetworks` believes says it was asked for. `None`
/// for a request that names no host (HTTP/1.0).
fn origin(state: &AppState, remote: Option<std::net::IpAddr>, headers: &HeaderMap) -> Option<String> {
    let trusted = remote.is_some_and(|peer| state.settings().trusted_networks.contains(peer));
    let forwarded = headers
        .get_all("x-forwarded-host")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .next_back()
        .filter(|_| trusted);
    let host = forwarded
        .or_else(|| headers.get(header::HOST).and_then(|v| v.to_str().ok()))
        .map(str::trim)
        // A host name, with a port or without: nothing that could start
        // another header or another URL.
        .filter(|h| {
            !h.is_empty()
                && h.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']' | b'_')
                })
        })?;
    let scheme = if over_https(state, remote, headers) {
        "https"
    } else {
        "http"
    };
    Some(format!("{scheme}://{host}"))
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

use unarr_core::outbound::{is_cgnat_address, is_local_address};

/// Guard for the pages and bootstrap the browser loads: with forms or basic
/// authentication a valid session is required (the API key does not open
/// pages, as in the originals) unless the client is a local address the
/// configuration exempts.
pub async fn require_ui_auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let settings = state.settings();
    let host = &settings.host;
    let needs_auth = matches!(
        host.authentication_method,
        AuthenticationMethod::Forms | AuthenticationMethod::Basic
    );
    if !needs_auth {
        return next.run(request).await;
    }
    let headers = request.headers();
    // The client behind the proxies `TrustedNetworks` lets speak for it; a
    // forwarded address nobody trusted vouched for leaves it unknown
    // (`IsClientAddressKnown`), and an unknown client is never exempt.
    let remote = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    let client = crate::forwarded::client(remote, headers, &settings.trusted_networks);
    let known = !client.forwarded_left;
    let exempt = match host.authentication_required {
        AuthenticationRequired::Enabled => false,
        AuthenticationRequired::DisabledForLocalAddresses => {
            known
                && client.address.is_some_and(|ip| {
                    is_local_address(ip) || (host.trust_cgnat_ip_addresses && is_cgnat_address(ip))
                })
        }
        AuthenticationRequired::DisabledForLocalhost => {
            known && client.address.is_some_and(|ip| ip.is_loopback())
        }
    };
    // Exempt clients skip the credential check, which is slow for Basic.
    if exempt {
        return next.run(request).await;
    }
    let mut renewed = None;
    let session_ok = match host.authentication_method {
        AuthenticationMethod::Forms => match session(&state, headers) {
            Some(session) => {
                // `SlidingExpiration`: past half its life, the session
                // starts a new one.
                if session.expires - state.now().timestamp() < SESSION_SECONDS / 2 {
                    let secure = over_https(&state, remote, headers);
                    renewed = Some(issue_cookie(
                        &state,
                        &session.identifier,
                        session.persistent,
                        secure,
                    ));
                }
                true
            }
            None => false,
        },
        AuthenticationMethod::Basic => basic_auth_valid(&state, headers).await,
        _ => true,
    };
    if session_ok {
        let mut response = next.run(request).await;
        if let Some(cookie) = renewed.and_then(|c| HeaderValue::from_str(&c).ok()) {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
        return response;
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
            // As ASP.NET's cookie authentication challenges: the whole
            // URL of the login page, and where to come back to as one
            // escaped value.
            found(&format!(
                "{}{}/login?returnUrl={}",
                origin(&state, remote, headers).unwrap_or_default(),
                state.settings().url_base,
                escape_data(&return_url)
            ))
        }
    }
}

/// `-<digits>.jpg` at the end of a path, taken off: the cover a resized
/// one was made from.
fn unresized(path: &str) -> Option<String> {
    let stem = path
        .len()
        .checked_sub(4)
        .filter(|&at| path.is_char_boundary(at) && path[at..].eq_ignore_ascii_case(".jpg"))
        .map(|at| &path[..at])?;
    let (before, size) = stem.rsplit_once('-')?;
    (!size.is_empty() && size.bytes().all(|b| b.is_ascii_digit())).then(|| format!("{before}.jpg"))
}

/// `MediaCoverMapper.MapPath`: a resized cover that is not on disk, or is
/// empty, is answered with the cover it would have been made from. The UI
/// asks for `poster-<size times the screen's pixel ratio>.jpg`, and only
/// some sizes are made: on a high-density screen the series page asks for
/// `poster-1000.jpg`. `request` is the one the cover folder is about to
/// serve, its path relative to `covers`.
pub fn full_cover_when_resized_is_missing<B>(
    covers: &std::path::Path,
    mut request: Request<B>,
) -> Request<B> {
    let path = request.uri().path();
    let Some(full) = unresized(path) else {
        return request;
    };
    let relative = path.trim_start_matches('/');
    if relative.split('/').any(|part| part == "..") {
        return request;
    }
    let on_disk = std::fs::metadata(covers.join(relative))
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false);
    if on_disk {
        return request;
    }
    let uri = match request.uri().query() {
        Some(query) => format!("{full}?{query}"),
        None => full,
    };
    if let Ok(uri) = uri.parse() {
        *request.uri_mut() = uri;
    }
    request
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

/// `Uri.EscapeDataString`: everything but letters, digits and `-_.~`.
fn escape_data(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_return_url_is_one_escaped_value() {
        assert_eq!(
            escape_data("/settings/general?x=1&y=a%20b"),
            "%2Fsettings%2Fgeneral%3Fx%3D1%26y%3Da%2520b"
        );
    }

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
    fn a_missing_resized_cover_falls_back_to_the_full_one() {
        let covers = std::env::temp_dir().join(format!("unarr_cover_fallback_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&covers);
        std::fs::create_dir_all(covers.join("5")).unwrap();
        std::fs::write(covers.join("5/poster.jpg"), b"full").unwrap();
        std::fs::write(covers.join("5/poster-500.jpg"), b"resized").unwrap();
        std::fs::write(covers.join("5/poster-250.jpg"), b"").unwrap();
        let served = |uri: &str| {
            let request = axum::http::Request::builder().uri(uri).body(()).unwrap();
            full_cover_when_resized_is_missing(&covers, request)
                .uri()
                .to_string()
        };
        // On disk: served as asked.
        assert_eq!(served("/5/poster-500.jpg"), "/5/poster-500.jpg");
        assert_eq!(served("/5/poster.jpg?lastWrite=1"), "/5/poster.jpg?lastWrite=1");
        // Not made, or empty: the full cover, the query kept.
        assert_eq!(
            served("/5/poster-1000.jpg?lastWrite=1"),
            "/5/poster.jpg?lastWrite=1"
        );
        assert_eq!(served("/5/poster-250.jpg"), "/5/poster.jpg");
        assert_eq!(served("/5/fanart-720.JPG"), "/5/fanart.jpg");
        // Not a resized name, or not a jpg: left alone.
        assert_eq!(served("/5/clearlogo-70.png"), "/5/clearlogo-70.png");
        assert_eq!(served("/5/poster-big.jpg"), "/5/poster-big.jpg");
        assert_eq!(served("/5/../../etc/x-1.jpg"), "/5/../../etc/x-1.jpg");
        std::fs::remove_dir_all(&covers).unwrap();
    }
}
