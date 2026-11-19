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

/// A file of the bundle as `StaticResourceMapperBase` serves it: its type
/// by its extension, as ASP.NET's table has it. The cache headers are the
/// cache layer's (`http::cache_headers`).
fn serve_asset(personality: Personality, path: &str) -> Option<Response> {
    let file = asset(personality, path)?;
    let content_type = crate::http::content_type(personality, path);
    // The controller forbids caching any page it serves (`oauth.html`).
    if content_type == "text/html" {
        return Some(html(String::from_utf8_lossy(&file.data).into_owned()));
    }
    Some(
        (
            [(header::CONTENT_TYPE, content_type)],
            Body::from(file.data.into_owned()),
        )
            .into_response(),
    )
}

/// `NotFound()` of the static resource controller, which `[ApiController]`
/// turns into problem details, `problem+json` in both originals.
pub fn static_not_found(personality: Personality) -> Response {
    let document = match personality {
        Personality::Radarr => crate::error::NOT_FOUND_TYPE_RFC9110,
        Personality::Sonarr | Personality::Lidarr => crate::error::NOT_FOUND_TYPE_RFC7231,
    };
    crate::error::not_found_problem(document, "application/problem+json; charset=utf-8")
}

/// `index.html` as the originals' `IndexHtmlMapper` writes it: cache
/// breakers and the URL base on the asset links, and the URL base in the
/// `window.Sonarr` bootstrap.
fn index_html(personality: Personality, url_base: &str) -> Option<String> {
    let file = asset(personality, "index.html")?;
    let text = String::from_utf8_lossy(&file.data).into_owned();
    Some(rewrite_html(personality, &text, url_base).replace("__URL_BASE__", url_base))
}

fn login_html(personality: Personality, url_base: &str, theme: &str) -> Option<String> {
    let file = asset(personality, "login.html")?;
    let text = String::from_utf8_lossy(&file.data).into_owned();
    Some(
        rewrite_html(personality, &text, url_base)
            .replace("__URL_BASE__", url_base)
            .replace("_THEME_", theme),
    )
}

/// `HtmlMapperBase`'s pattern: an `href` or `src` whose value ends in one
/// of these extensions, and the `data-no-hash` attribute after it.
fn asset_link() -> &'static fancy_regex::Regex {
    static LINK: std::sync::OnceLock<fancy_regex::Regex> = std::sync::OnceLock::new();
    LINK.get_or_init(|| {
        fancy_regex::Regex::new(
            r#"(?i)(?:(?<attribute>href|src)=")(?<path>.*?(?<extension>css|js|png|ico|ics|svg|json))(?:")(?:\s(?<nohash>data-no-hash))?"#,
        )
        .expect("the asset link pattern compiles")
    })
}

/// `HtmlMapperBase.GetHtmlText`: each asset link gets the URL base in
/// front and, unless it says `data-no-hash` (which goes), a cache breaker
/// after it.
fn rewrite_html(personality: Personality, html: &str, url_base: &str) -> String {
    let mut out = String::with_capacity(html.len() + 512);
    let mut last = 0;
    for found in asset_link().captures_iter(html) {
        let Ok(found) = found else { break };
        let Some(whole) = found.get(0) else { continue };
        let attribute = found.name("attribute").map_or("", |m| m.as_str());
        let path = found.name("path").map_or("", |m| m.as_str());
        let url = if found.name("nohash").is_some() {
            path.to_owned()
        } else {
            with_cache_breaker(personality, path)
        };
        out.push_str(&html[last..whole.start()]);
        out.push_str(&format!("{attribute}=\"{url_base}{url}\""));
        last = whole.end();
    }
    out.push_str(&html[last..]);
    out
}

/// `CacheBreakerProvider.AddCacheBreakerToPath`: `?h=` and the MD5 of the
/// file, in base64 without its padding. Not for a calendar feed, nor for a
/// path that ends in `main`; a link to no file of the bundle is left as
/// it is.
fn with_cache_breaker(personality: Personality, path: &str) -> String {
    if path.ends_with(".ics") || path.ends_with("main") {
        return path.to_owned();
    }
    // The file the mapper of that path would serve.
    let file = match path {
        "/favicon.ico" => "Content/Images/Icons/favicon.ico",
        "/robots.txt" => "Content/robots.txt",
        other => other.trim_start_matches('/'),
    };
    let Some(file) = asset(personality, file) else {
        return path.to_owned();
    };
    use md5::Digest;
    let hash = base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(&file.data));
    format!("{path}?h={}", hash.trim_end_matches('='))
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

/// `StaticResourceController`'s pattern of what never reaches a file: a
/// `..` next to a slash, a backslash or their escapes.
fn invalid_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    ["/..", "\\..", "%2f..", "%5c..", "../", "..\\", "..%2f", "..%5c"]
        .iter()
        .any(|pattern| lower.contains(pattern))
}

/// `StaticResourceMapper.CanHandle`: what is served from the bundle as it
/// is. `path` is lower-cased.
fn bundle_file(path: &str) -> bool {
    if path.starts_with("/content/manifest") || path.starts_with("/content/browserconfig") {
        return false;
    }
    path.starts_with("/content")
        || path.ends_with(".js")
        || path.ends_with(".map")
        || path.ends_with(".css")
        || (path.ends_with(".ico") && path != "/favicon.ico")
        || path.ends_with(".swf")
        || path.ends_with("oauth.html")
}

/// Everything under `/` that no route takes, as the originals' catch-all
/// action hands it to the one mapper that says it can: the login page for
/// a path that starts with `/login` (`/login.html`), `robots.txt` (from
/// `Content/`), a file of the bundle, or the single-page application for a
/// path with no dot. What none of them takes, or a file that is not there,
/// is the controller's 404 (`/index.html`, a licence file).
pub async fn spa(State(state): State<AppState>, uri: Uri) -> Response {
    let personality = state.personality();
    let path = uri.path();
    if invalid_path(path) {
        return static_not_found(personality);
    }
    if path.starts_with("/login") {
        return login_page(State(state)).await;
    }
    if path == "/robots.txt" {
        return serve_asset(personality, "Content/robots.txt")
            .unwrap_or_else(|| static_not_found(personality));
    }
    let lower = path.to_ascii_lowercase();
    if bundle_file(&lower) {
        return serve_asset(personality, path.trim_start_matches('/'))
            .unwrap_or_else(|| static_not_found(personality));
    }
    if lower.contains('.') || lower.starts_with("/content") || lower.starts_with("/mediacover") {
        return static_not_found(personality);
    }
    match index_html(personality, state.url_base()) {
        Some(page) => html(page),
        None => no_ui_page(),
    }
}

/// `/content/{**path}`: the manifest and the tile description filled in,
/// or the file of the bundle under `Content/`.
pub async fn content(State(state): State<AppState>, uri: Uri) -> Response {
    let personality = state.personality();
    let path = uri.path().trim_start_matches('/');
    // The route is matched in any case; what follows it keeps its own.
    let rel = path.get("content/".len()..).unwrap_or("");
    if invalid_path(&format!("/{rel}")) {
        return static_not_found(personality);
    }
    if let Some(name) = FILLED_IN.iter().find(|name| rel.eq_ignore_ascii_case(name)) {
        let s = state.settings();
        if let Some(r) = filled_in_asset(personality, name, state.url_base(), &s.instance_name) {
            return r;
        }
    }
    serve_asset(personality, &format!("Content/{rel}")).unwrap_or_else(|| static_not_found(personality))
}

/// The files under `/Content` the originals fill in as they serve them
/// (`ManifestMapper`, `BrowserConfig`): the web app manifest carries the
/// instance name and, like the tile description, addresses under the URL
/// base. Asked in any case, as the originals answer them.
const FILLED_IN: [&str; 2] = ["manifest.json", "browserconfig.xml"];

fn filled_in_asset(
    personality: Personality,
    name: &str,
    url_base: &str,
    instance_name: &str,
) -> Option<Response> {
    let file = asset(personality, &format!("Content/{name}"))?;
    // The manifest is JSON: a quote or a backslash in the instance name
    // goes in escaped, or the browser cannot read the file.
    let instance_name = if name.to_ascii_lowercase().ends_with(".json") {
        let quoted = serde_json::Value::from(instance_name).to_string();
        quoted[1..quoted.len() - 1].to_string()
    } else {
        instance_name.to_string()
    };
    let text = String::from_utf8_lossy(&file.data)
        .replace("__URL_BASE__", url_base)
        .replace("__INSTANCE_NAME__", &instance_name);
    Some(
        (
            [(header::CONTENT_TYPE, crate::http::content_type(personality, name))],
            text,
        )
            .into_response(),
    )
}

pub async fn favicon(State(state): State<AppState>) -> Response {
    let personality = state.personality();
    serve_asset(personality, "Content/Images/Icons/favicon.ico")
        .or_else(|| serve_asset(personality, "Content/Images/Icons/favicon-32x32.png"))
        .unwrap_or_else(|| static_not_found(personality))
}

/// `/MediaCover/{**path}`, which `MediaCoverMapper` serves from the cover
/// folder: a path in any case finds the mapper, but the folder is named
/// in its own case; a resized cover that is not there, or is empty, is
/// the full one (`full_cover_when_resized_is_missing`, applied before);
/// its type by its extension; no range, and no validator but the cache
/// layer's.
pub async fn media_cover(State(state): State<AppState>, request: Request) -> Response {
    let personality = state.personality();
    let (asked, _) = crate::http::path_in_app(&state, &request);
    if !asked.starts_with("/MediaCover/") {
        return static_not_found(personality);
    }
    let relative = request.uri().path().trim_start_matches('/');
    let Some(relative) = percent_decoded(relative) else {
        return static_not_found(personality);
    };
    let unsafe_part =
        relative.split('/').any(|part| part.is_empty() || part == "..") || relative.contains('\\');
    if unsafe_part {
        return static_not_found(personality);
    }
    let file = state.runtime().media_cover_dir.join(&relative);
    match tokio::fs::read(&file).await {
        Ok(bytes) => (
            [(
                header::CONTENT_TYPE,
                crate::http::content_type(personality, &relative),
            )],
            bytes,
        )
            .into_response(),
        Err(_) => static_not_found(personality),
    }
}

/// A path with its `%XX` escapes decoded; `None` when they do not make
/// UTF-8 or a byte is nul.
fn percent_decoded(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    if out.contains(&0) {
        return None;
    }
    String::from_utf8(out).ok()
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
    match login_html(state.personality(), state.url_base(), &s.host.theme) {
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
    // `AuthenticationService.Login` finds no user when nothing is to be
    // signed into, and logs nothing.
    if state.settings().host.authentication_method == AuthenticationMethod::None {
        return found(&format!(
            "{url_base}/login?returnUrl={}&loginFailed=true",
            percent_encode(&return_url)
        ));
    }
    let remote = remote.map(|c| c.0 .0.ip());
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
        log_auth(&state, remote, &headers, AuthOutcome::Failure, &form.username);
        return found(&format!(
            "{url_base}/login?returnUrl={}&loginFailed=true",
            percent_encode(&return_url)
        ));
    };
    log_auth(&state, remote, &headers, AuthOutcome::Success, &form.username);
    let persistent = form.remember_me == "on";
    let secure = over_https(&state, remote, &headers);
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
    crate::http::signed_in_or_out(&mut response);
    response
}

pub async fn logout(
    State(state): State<AppState>,
    remote: Option<axum::Extension<ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
) -> Response {
    // The originals name nobody here, signed in or not, and log nothing
    // when nothing is signed into.
    if state.settings().host.authentication_method != AuthenticationMethod::None {
        log_auth(
            &state,
            remote.map(|c| c.0 .0.ip()),
            &headers,
            AuthOutcome::Logout,
            "",
        );
    }
    let name = cookie_name(&state);
    let expired = format!(
        "{name}=; Path={}; Max-Age=0; HttpOnly; SameSite=Lax",
        cookie_path(&state)
    );
    let mut response = found(&format!("{}/", state.settings().url_base));
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&expired).unwrap());
    crate::http::signed_in_or_out(&mut response);
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

/// The cookie's path: the URL base the routes are under, or the root.
fn cookie_path(state: &AppState) -> String {
    let base = state.url_base();
    if base.is_empty() {
        "/".to_string()
    } else {
        base.to_owned()
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

/// Whether the request reached the proxy in front over HTTPS: a proxy
/// `TrustedNetworks` believes says so (`X-Forwarded-Proto`).
fn over_https(state: &AppState, remote: Option<std::net::IpAddr>, headers: &HeaderMap) -> bool {
    crate::forwarded::walk(remote, headers, &state.settings().trusted_networks)
        .scheme
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"))
}

/// `scheme://host` as the client names this server: the `Host` header, or
/// what a proxy `TrustedNetworks` believes says it was asked for. `None`
/// for a request that names no host (HTTP/1.0).
fn origin(state: &AppState, remote: Option<std::net::IpAddr>, headers: &HeaderMap) -> Option<String> {
    let forwarded = crate::forwarded::walk(remote, headers, &state.settings().trusted_networks);
    let host = forwarded.host.or_else(|| {
        headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    })?;
    let host = host.trim();
    // A host name, with a port or without: nothing that could start
    // another header or another URL.
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']' | b'_'));
    if !plain {
        return None;
    }
    let scheme = if forwarded.scheme.is_some_and(|s| s.eq_ignore_ascii_case("https")) {
        "https"
    } else {
        "http"
    };
    Some(format!("{scheme}://{host}"))
}

/// `AuthenticationService`'s lines, which log-based intrusion detection
/// (fail2ban's `Auth-Failure ip <HOST>`) reads: under the `Auth` logger,
/// failures as warnings, with the client's address once the proxies that
/// are believed have spoken and the user name as it was given. Sonarr
/// 4.0.20 also logs each success, at info; Radarr 6.4.4 does not.
pub(crate) fn log_auth(
    state: &AppState,
    remote: Option<std::net::IpAddr>,
    headers: &HeaderMap,
    outcome: AuthOutcome,
    username: &str,
) {
    let client = crate::forwarded::client(remote, headers, &state.settings().trusted_networks);
    let ip = client.address.map(|a| a.to_string()).unwrap_or_default();
    // The name is the client's to choose: a line break in it would end this
    // line and start one of the caller's making, an `Auth-Failure` for an
    // address the intrusion detection would then block.
    let username: String = username.chars().filter(|c| !c.is_control()).collect();
    match outcome {
        AuthOutcome::Failure => tracing::warn!(target: "Auth", "Auth-Failure ip {ip} username '{username}'"),
        AuthOutcome::Success if state.personality() != Personality::Radarr => {
            tracing::info!(target: "Auth", "Auth-Success ip {ip} username '{username}'")
        }
        AuthOutcome::Success => {}
        AuthOutcome::Logout => tracing::info!(target: "Auth", "Auth-Logout ip {ip} username '{username}'"),
    }
}

/// What [`log_auth`] reports.
#[derive(Clone, Copy)]
pub(crate) enum AuthOutcome {
    Failure,
    Success,
    Logout,
}

/// Basic authentication sends the password on every request, posters
/// included: the user is read on the database thread and the slow hash runs
/// on a blocking worker, never on the async ones.
async fn basic_auth(state: &AppState, headers: &HeaderMap) -> Option<(String, bool)> {
    let value = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded = String::from_utf8_lossy(&decoded).into_owned();
    let (given, password) = decoded.split_once(':')?;
    let given = given.to_string();
    let username = given.to_lowercase();
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
    let valid = match user {
        Some(u) => crate::blocking::run(move || verify_password(&u, &password))
            .await
            .unwrap_or(false),
        None => false,
    };
    Some((given, valid))
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
        // Each request that carries credentials is logged, as the
        // originals log it.
        AuthenticationMethod::Basic => match basic_auth(&state, headers).await {
            Some((username, valid)) => {
                let outcome = if valid {
                    AuthOutcome::Success
                } else {
                    AuthOutcome::Failure
                };
                log_auth(&state, remote, headers, outcome, &username);
                valid
            }
            None => false,
        },
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
            [(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_str(&format!("Basic realm=\"{}\"", state.personality().app_name()))
                    .unwrap_or(HeaderValue::from_static("Basic")),
            )],
        )
            .into_response(),
        _ => {
            // As ASP.NET's cookie authentication challenges: the whole
            // URL of the login page, and where to come back to as one
            // escaped value, both as the client asked: the URL base in
            // its case, a slash after it kept, and no base at all for a
            // path outside it (the originals send it to the login page
            // outside the base, which sends it in).
            let asked = crate::asked_uri(&request);
            let return_url = asked.path_and_query().map(|p| p.to_string()).unwrap_or_default();
            let mut challenge = found(&format!(
                "{}{}/login?returnUrl={}",
                origin(&state, remote, headers).unwrap_or_default(),
                crate::url_base_as_asked(&state, &request),
                escape_data(&return_url)
            ));
            // An XHR is told 401, the login page's address still given.
            if is_ajax_request(&asked, headers) {
                *challenge.status_mut() = StatusCode::UNAUTHORIZED;
            }
            challenge
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

/// `IsAjaxRequest` of ASP.NET's cookie authentication: `X-Requested-With`
/// says `XMLHttpRequest`, in this case, as a header or in the query.
fn is_ajax_request(uri: &Uri, headers: &HeaderMap) -> bool {
    const AJAX: &str = "XMLHttpRequest";
    headers.get("X-Requested-With").is_some_and(|v| v == AJAX)
        || crate::query::Params::from_uri(uri).get("X-Requested-With") == Some(AJAX)
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
    fn asset_links_get_the_url_base_and_a_cache_breaker() {
        // A file the bundle does not have keeps no breaker; `data-no-hash`
        // keeps none and goes; a link that ends in no listed extension is
        // left alone.
        let html = r#"<link href="/Content/missing.css"><script src="/index-1.js" data-no-hash></script><a href="https://x/y">x</a>"#;
        let out = rewrite_html(Personality::Lidarr, html, "");
        assert_eq!(
            out,
            r#"<link href="/Content/missing.css"><script src="/index-1.js"></script><a href="https://x/y">x</a>"#
        );
        let out = rewrite_html(Personality::Lidarr, html, "/sonarr");
        assert!(out.contains(r#"href="/sonarr/Content/missing.css""#), "{out}");
        assert!(out.contains(r#"src="/sonarr/index-1.js">"#), "{out}");
        assert!(out.contains(r#"href="https://x/y""#), "{out}");
        assert_eq!(
            with_cache_breaker(Personality::Sonarr, "/feed/x.ics"),
            "/feed/x.ics"
        );
    }

    #[test]
    fn only_the_mappers_paths_reach_a_file() {
        for path in [
            "/content/styles.css",
            "/index-1.js",
            "/x.map",
            "/oauth.html",
            "/a.ico",
            "/content",
        ] {
            assert!(bundle_file(path), "{path}");
        }
        for path in [
            "/favicon.ico",
            "/index.html",
            "/a.js.license.txt",
            "/content/manifest.json",
            "/content/browserconfig.xml",
            "/robots.txt",
        ] {
            assert!(!bundle_file(path), "{path}");
        }
        assert!(invalid_path("/Content/../x"));
        assert!(invalid_path("/Content/..%2Fx"));
        assert!(!invalid_path("/Content/a..b"));
        assert_eq!(
            percent_decoded("1/poster%20x.jpg").as_deref(),
            Some("1/poster x.jpg")
        );
        assert_eq!(percent_decoded("1/%00.jpg"), None);
        assert_eq!(percent_decoded("1/%zz%4").as_deref(), Some("1/%zz%4"));
    }

    #[test]
    fn an_xhr_is_one_by_its_header_or_its_query_in_this_case() {
        let uri: Uri = "/series".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert!(!is_ajax_request(&uri, &headers));
        headers.insert("X-Requested-With", HeaderValue::from_static("xmlhttprequest"));
        assert!(!is_ajax_request(&uri, &headers));
        headers.insert("X-Requested-With", HeaderValue::from_static("XMLHttpRequest"));
        assert!(is_ajax_request(&uri, &headers));
        let uri: Uri = "/?x-requested-with=XMLHttpRequest".parse().unwrap();
        assert!(is_ajax_request(&uri, &HeaderMap::new()));
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
