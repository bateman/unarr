//! Plex Media Server, after the originals' `PlexServer`,
//! `PlexServerService` and `PlexServerProxy`: the TV sections whose
//! location holds the series' root folder are asked to refresh the series'
//! folder. The plex.tv side is `PlexTvService` and `PlexTvProxy`: the
//! daily ping that keeps the token alive, and the connection's actions
//! (`PlexServer.RequestAction`), the sign-in with a plex.tv PIN that
//! gives the *Auth Token*, and the list of the account's servers that
//! fills host, port and SSL from the *Server* menu (issue #16; until
//! 21 November 2026 they answered `{}` and the token was entered by hand).
//! Radarr's connection is the same code.

use crate::sync::Recover;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use super::{Notifier, TestOutcome};
use crate::model::{RootFolder, Series};
use crate::store;
use crate::validation::ValidationFailure;

/// `PlexServerSettings`.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub host: String,
    pub port: i64,
    pub use_ssl: bool,
    pub url_base: String,
    pub auth_token: String,
    pub update_library: bool,
    pub map_from: String,
    pub map_to: String,
}

impl Settings {
    pub fn from_value(v: &Value) -> Settings {
        let text = |key: &str| v[key].as_str().unwrap_or("").to_string();
        Settings {
            host: text("host"),
            port: v["port"].as_i64().unwrap_or(32400),
            use_ssl: v["useSsl"].as_bool().unwrap_or(false),
            url_base: text("urlBase"),
            auth_token: text("authToken"),
            update_library: v["updateLibrary"].as_bool().unwrap_or(true),
            map_from: text("mapFrom"),
            map_to: text("mapTo"),
        }
    }

    /// `PlexServerSettingsValidator`.
    pub fn validate(&self) -> Vec<ValidationFailure> {
        let mut f = Vec::new();
        if self.host.trim().is_empty() {
            f.push(ValidationFailure::setting_not_empty("Host", json!(self.host)));
        }
        let name_ok = !self.host.is_empty()
            && self
                .host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        let ip_ok = self.host.parse::<std::net::IpAddr>().is_ok();
        if !name_ok && !ip_ok {
            f.push(ValidationFailure::setting(
                "Host",
                "must be valid Host without http://",
                json!(self.host),
            ));
        }
        if !(1..=65535).contains(&self.port) {
            f.push(ValidationFailure::setting(
                "Port",
                &format!("'Port' must be between 1 and 65535. You entered {}.", self.port),
                json!(self.port),
            ));
        }
        if self.map_from.trim().is_empty() && !self.map_to.trim().is_empty() {
            f.push(ValidationFailure::setting_not_empty(
                "MapFrom",
                json!(self.map_from),
            ));
        }
        if self.map_to.trim().is_empty() && !self.map_from.trim().is_empty() {
            f.push(ValidationFailure::setting_not_empty("MapTo", json!(self.map_to)));
        }
        f
    }
}

/// `PlexSection`: a TV library and where its files are.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub id: i64,
    pub locations: Vec<String>,
}

/// The originals' exceptions, by how the test reports them.
#[derive(Debug)]
pub enum PlexError {
    /// `PlexAuthenticationException`.
    Authentication,
    /// `PlexException`.
    Plex(String),
    /// `PlexVersionException` and anything else.
    Other(String),
}

impl std::fmt::Display for PlexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlexError::Authentication => f.write_str("Unauthorized - AuthToken is invalid"),
            PlexError::Plex(m) | PlexError::Other(m) => f.write_str(m),
        }
    }
}

/// `PlexClientIdentifier`: the stored one, or a new one stored now.
async fn client_identifier(notifier: &Notifier) -> String {
    notifier
        .db()
        .call(|conn| {
            if let Some(id) = store::config::config(conn, "PlexClientIdentifier")? {
                return Ok(id);
            }
            let id = new_identifier();
            store::config::set_config(conn, "PlexClientIdentifier", &id)?;
            Ok(id)
        })
        .await
        .unwrap_or_else(|_| new_identifier())
}

/// `PlexTvService.Ping` and `PlexTvProxy.Ping`: tell plex.tv that the
/// token is still in use, so that it does not expire, at most once a day per
/// token. The originals call it before every library update and connection
/// test. What plex.tv answers changes nothing: a failure is logged at
/// trace, and the day counts as pinged all the same.
///
/// A blank token is not pinged. The originals' cache refuses a blank key
/// (`Ensure.That(key).IsNotNullOrWhiteSpace()`), so there the exception
/// fails the connection test and every Plex update before it is queued;
/// Unarr goes on to the server, which decides whether it needs a token
/// (docs/DESIGN.md, section 21).
pub(crate) async fn ping_plex_tv(notifier: &Notifier, token: &str) {
    if token.trim().is_empty() || !notifier.plex_tv_ping_due(token) {
        return;
    }
    let identity = notifier.identity();
    let client_id = client_identifier(notifier).await;
    let query = [
        ("X-Plex-Client-Identifier", client_id.as_str()),
        ("X-Plex-Product", identity.app_name.as_str()),
        ("X-Plex-Platform", "Windows"),
        ("X-Plex-Platform-Version", "7"),
        ("X-Plex-Device-Name", identity.app_name.as_str()),
        ("X-Plex-Version", identity.app_version.as_str()),
        ("X-Plex-Token", token),
    ];
    let Ok(url) =
        reqwest::Url::parse_with_params(&notifier.upstream_url("https://plex.tv/api/v2/ping"), query)
    else {
        return;
    };
    tracing::debug!(url = %crate::http::redact(url.as_str()), "pinging plex.tv");
    // Ten seconds, not the client's hundred: the update waits for the
    // ping, and a plex.tv that takes the connection and never answers
    // would hold the day's first import (or Test) that long. The originals
    // wait the hundred; nothing a client reads depends on it.
    let outcome = notifier
        .http()
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.without_url().to_string())
        .and_then(|r| match r.status() {
            s if s.is_success() => Ok(()),
            s => Err(format!("status {}", s.as_u16())),
        });
    if let Err(e) = outcome {
        tracing::trace!(error = %e, "Unable to ping plex.tv");
    }
}

/// The query parameters that name Unarr to plex.tv, as `BuildRequest` of
/// `PlexTvProxy` and `GetPinUrl` add them, in their order.
fn plex_tv_identity(client_id: &str, identity: &super::Identity) -> Vec<(&'static str, String)> {
    vec![
        ("X-Plex-Client-Identifier", client_id.to_string()),
        ("X-Plex-Product", identity.app_name.clone()),
        ("X-Plex-Platform", "Windows".into()),
        ("X-Plex-Platform-Version", "7".into()),
        ("X-Plex-Device-Name", identity.app_name.clone()),
        ("X-Plex-Version", identity.app_version.clone()),
    ]
}

/// `Uri.EscapeDataString`: everything but the unreserved characters of
/// RFC 3986 percent-encoded, in upper case.
fn escape_data(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `HttpUri.AddQueryParams`: `key=value` pairs, each side escaped.
fn query_string(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", escape_data(k), escape_data(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// What an action of the connection raises, as the originals' exception
/// handler answers it.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionError {
    /// An exception the original lets escape: a 500 with its message, the
    /// description naming the exception's class.
    Server {
        exception: &'static str,
        message: String,
    },
    /// `NzbDroneClientException(status, "Unable to connect to plex.tv")`
    /// of `PlexTvProxy.ProcessRequest`: plex.tv's own status when it
    /// answered with an error, 400 when it did not answer.
    PlexTv(u16),
}

impl ActionError {
    pub const PLEX_TV_MESSAGE: &'static str = "Unable to connect to plex.tv";

    fn key_not_found(key: &str) -> ActionError {
        ActionError::Server {
            exception: "System.Collections.Generic.KeyNotFoundException",
            message: format!("The given key '{key}' was not present in the dictionary."),
        }
    }

    fn bad_request(key: &str) -> ActionError {
        ActionError::Server {
            exception: "NzbDrone.Core.Exceptions.BadRequestException",
            message: format!("QueryParam {key} invalid."),
        }
    }
}

/// The action's query as the original's controller hands it over
/// (`Request.Query.ToDictionary`): looked up by the exact name, a
/// repeated parameter's values joined with commas.
struct ActionQuery<'a>(&'a [(String, String)]);

impl ActionQuery<'_> {
    /// `query[key]`: a missing key raises.
    fn get(&self, key: &str) -> Result<String, ActionError> {
        let values: Vec<&str> = self
            .0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect();
        if values.is_empty() {
            return Err(ActionError::key_not_found(key));
        }
        Ok(values.join(","))
    }

    /// `query[key]`, refused when blank (`IsNullOrWhiteSpace`).
    fn required(&self, key: &str) -> Result<String, ActionError> {
        let value = self.get(key)?;
        if value.trim().is_empty() {
            return Err(ActionError::bad_request(key));
        }
        Ok(value)
    }
}

/// `Convert.ToInt32(string)`: white space around and a sign allowed.
fn to_int32(text: &str) -> Result<i32, ActionError> {
    let trimmed = text.trim();
    trimmed.parse::<i32>().map_err(|_| {
        let digits = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            ActionError::Server {
                exception: "System.OverflowException",
                message: "Value was either too large or too small for an Int32.".into(),
            }
        } else {
            ActionError::Server {
                exception: "System.FormatException",
                message: "Input string was not in a correct format.".into(),
            }
        }
    })
}

/// `PlexTvProxy.BuildRequest` + `ProcessRequest`: a GET of plex.tv with
/// the identifying parameters, then `extra`; the body as text. An error
/// status is plex.tv's status, no answer a 400.
async fn plex_tv_get(
    notifier: &Notifier,
    resource: &str,
    extra: &[(&str, String)],
) -> Result<String, ActionError> {
    let client_id = client_identifier(notifier).await;
    let mut query = plex_tv_identity(&client_id, notifier.identity());
    query.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
    let url = format!(
        "{}?{}",
        notifier.upstream_url(&format!("https://plex.tv{resource}")),
        query_string(&query)
    );
    tracing::debug!(url = %crate::http::redact(&url), "plex.tv request");
    let response = notifier
        .http()
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e.without_url(), "plex.tv did not answer");
            ActionError::PlexTv(400)
        })?;
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            resource,
            "plex.tv answered with an error"
        );
        return Err(ActionError::PlexTv(status.as_u16()));
    }
    crate::http::read_text(response, 4 << 20).await.map_err(|e| {
        tracing::warn!(error = %e, "plex.tv's answer could not be read");
        ActionError::PlexTv(400)
    })
}

/// `PlexTvService.GetPinUrl`: the request the browser makes to create a
/// PIN on plex.tv. Unarr does not make it: the address goes to the UI.
fn pin_url(client_id: &str, identity: &super::Identity) -> Value {
    let mut query = plex_tv_identity(client_id, identity);
    // `bool.ToString()`.
    query.push(("strong", "True".into()));
    json!({
        "url": format!("https://plex.tv/api/v2/pins?{}", query_string(&query)),
        "method": "POST",
        "headers": {"accept": "application/json"},
    })
}

/// `PlexTvService.GetSignInUrl`: the plex.tv page that signs the PIN in,
/// then sends the browser back to `callback_url`. The `#!` stands where
/// the original puts its `hashBang` segment.
fn sign_in_url(
    client_id: &str,
    identity: &super::Identity,
    callback_url: &str,
    pin_id: i32,
    pin_code: &str,
) -> Value {
    let query: Vec<(&str, String)> = vec![
        ("clientID", client_id.to_string()),
        ("forwardUrl", callback_url.to_string()),
        ("code", pin_code.to_string()),
        ("context[device][product]", identity.app_name.clone()),
        ("context[device][platform]", "Windows".into()),
        ("context[device][platformVersion]", "7".into()),
        ("context[device][version]", identity.app_version.clone()),
    ];
    json!({
        "oauthUrl": format!("https://app.plex.tv/auth/#!?{}", query_string(&query)),
        "pinId": pin_id,
    })
}

/// `PlexTvProxy.GetAuthToken`: the PIN's token once the user signed it in;
/// none before, and none when the answer is not a PIN.
async fn auth_token(notifier: &Notifier, pin_id: i32) -> Result<Option<String>, ActionError> {
    let text = plex_tv_get(notifier, &format!("/api/v2/pins/{pin_id}"), &[]).await?;
    Ok(serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["authToken"].as_str().map(str::to_string)))
}

/// One of `PlexTvResource.Connections`.
#[derive(Debug, Clone, PartialEq)]
struct Connection {
    uri: String,
    protocol: String,
    address: String,
    port: i64,
    local: bool,
}

impl Connection {
    /// `PlexTvResourceConnection.Host`: the host of the URI, or the
    /// address without one.
    fn host(&self) -> String {
        if self.uri.trim().is_empty() {
            return self.address.clone();
        }
        reqwest::Url::parse(&self.uri)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| self.address.clone())
    }
}

/// `PlexTvService.GetServers`: the ping, then the account's resources that
/// it owns and that provide `server`. `GetResources` keeps nothing of a
/// failure: plex.tv refusing the token, or not answering, is no server.
async fn servers(notifier: &Notifier, token: &str) -> Vec<(String, Vec<Connection>)> {
    ping_plex_tv(notifier, token).await;
    let client_id = client_identifier(notifier).await;
    let extra = [
        ("includeHttps", "1".to_string()),
        ("clientID", client_id),
        ("X-Plex-Token", token.to_string()),
    ];
    let resources = match plex_tv_get(notifier, "/api/v2/resources", &extra).await {
        Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
        Err(e) => {
            tracing::trace!(error = ?e, "Unable to ping plex.tv");
            Value::Null
        }
    };
    let Some(resources) = resources.as_array() else {
        return Vec::new();
    };
    resources
        .iter()
        .filter(|r| r["owned"].as_bool() == Some(true))
        .filter(|r| {
            r["provides"]
                .as_str()
                .is_some_and(|p| p.split(',').any(|p| p == "server"))
        })
        .map(|r| {
            let connections = r["connections"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|c| Connection {
                            uri: c["uri"].as_str().unwrap_or("").to_string(),
                            protocol: c["protocol"].as_str().unwrap_or("").to_string(),
                            address: c["address"].as_str().unwrap_or("").to_string(),
                            port: c["port"].as_i64().unwrap_or(0),
                            local: c["local"].as_bool().unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            (r["name"].as_str().unwrap_or("").to_string(), connections)
        })
        .collect()
}

/// The *Server* menu's options, as `RequestAction("servers")` builds them:
/// one per connection, whose `additionalProperties` the UI copies into
/// host, port and SSL, and for an HTTPS one a second, plain HTTP to its
/// address.
fn server_options(servers: &[(String, Vec<Connection>)]) -> Vec<Value> {
    let mut options = Vec::new();
    for (name, connections) in servers {
        for c in connections {
            let secure = c.protocol == "https";
            let host = c.host();
            let place = if c.local { "Local" } else { "Remote" };
            let hint = if secure {
                format!("{place}, Secure")
            } else {
                place.to_string()
            };
            let mut option = json!({
                "value": c.uri,
                "name": format!("{name} ({host})"),
                "order": 0,
                "hint": hint,
                "additionalProperties": {"host": host, "port": c.port, "useSsl": secure},
            });
            // A connection without a URI has a null value, left out.
            if c.uri.is_empty() {
                option.as_object_mut().map(|o| o.remove("value"));
            }
            options.push(option);
            if secure {
                options.push(json!({
                    "value": format!("http://{}:{}", c.address, c.port),
                    "name": format!("{name} ({})", c.address),
                    "order": 0,
                    "hint": place,
                    "additionalProperties": {"host": c.address, "port": c.port, "useSsl": false},
                }));
            }
        }
    }
    options
}

/// `PlexServer.RequestAction`: the sign-in's three steps and the server
/// list. The names are matched as written, as the original compares them;
/// any other answers `{}`. The settings' checks the original runs first
/// filter on properties the settings do not have, and never fail.
pub async fn action(
    notifier: &Notifier,
    settings: &Settings,
    name: &str,
    query: &[(String, String)],
) -> Result<Value, ActionError> {
    let query = ActionQuery(query);
    match name {
        "startOAuth" => {
            let client_id = client_identifier(notifier).await;
            Ok(pin_url(&client_id, notifier.identity()))
        }
        "continueOAuth" => {
            let callback_url = query.required("callbackUrl")?;
            let id = query.required("id")?;
            let code = query.required("code")?;
            let pin_id = to_int32(&id)?;
            let client_id = client_identifier(notifier).await;
            Ok(sign_in_url(
                &client_id,
                notifier.identity(),
                &callback_url,
                pin_id,
                &code,
            ))
        }
        "getOAuthToken" => {
            let pin_id = to_int32(&query.required("pinId")?)?;
            // An anonymous object whose null is left out.
            Ok(match auth_token(notifier, pin_id).await? {
                Some(token) => json!({"authToken": token}),
                None => json!({}),
            })
        }
        "servers" => {
            if settings.auth_token.trim().is_empty() {
                return Ok(json!({}));
            }
            let servers = servers(notifier, &settings.auth_token).await;
            Ok(json!({"options": server_options(&servers)}))
        }
        _ => Ok(json!({})),
    }
}

/// A GUID-shaped identifier from the system's randomness.
fn new_identifier() -> String {
    let mut bytes = [0u8; 16];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).is_err() {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes.copy_from_slice(&seed.to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// `BuildRequest` + `ProcessRequest`: a GET with the identifying query
/// parameters, the body as text.
async fn get(
    notifier: &Notifier,
    settings: &Settings,
    resource: &str,
    extra: &[(&str, &str)],
) -> Result<String, PlexError> {
    let scheme = if settings.use_ssl { "https" } else { "http" };
    let host = if settings.host.contains(':') && !settings.host.starts_with('[') {
        format!("[{}]", settings.host)
    } else {
        settings.host.clone()
    };
    let base = settings.url_base.trim().trim_end_matches('/');
    let base = if base.is_empty() || base.starts_with('/') {
        base.to_string()
    } else {
        format!("/{base}")
    };
    let url = format!("{scheme}://{host}:{}{base}/{resource}", settings.port);
    let identity = notifier.identity();
    let client_id = client_identifier(notifier).await;
    let mut query: Vec<(&str, String)> = vec![
        ("X-Plex-Client-Identifier", client_id),
        ("X-Plex-Product", identity.app_name.clone()),
        ("X-Plex-Platform", "Windows".into()),
        ("X-Plex-Platform-Version", "7".into()),
        ("X-Plex-Device-Name", identity.app_name.clone()),
        ("X-Plex-Version", identity.app_version.clone()),
    ];
    if !settings.auth_token.trim().is_empty() {
        query.push(("X-Plex-Token", settings.auth_token.clone()));
    }
    for (k, v) in extra {
        query.push((k, (*v).to_string()));
    }
    tracing::debug!(url = %crate::http::redact(&url), "plex request");
    let response = notifier
        .http()
        .get(&url)
        .query(&query)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        // Without its URL: the query carries the token, and this text goes
        // to the log and to the answer of a connection test.
        // The cause at the end of the error's chain says what went wrong
        // (refused, a name that does not resolve) and names no URL.
        .map_err(|e| {
            let e = e.without_url();
            let mut cause: &dyn std::error::Error = &e;
            while let Some(next) = cause.source() {
                cause = next;
            }
            PlexError::Plex(format!("Unable to connect to Plex Media Server, {cause}"))
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(PlexError::Authentication);
    }
    if status.is_client_error() || status.is_server_error() {
        return Err(PlexError::Plex(format!(
            "Unable to connect to Plex Media Server. Status Code: {}",
            status_name(status)
        )));
    }
    let text = crate::http::read_text(response, 4 << 20)
        .await
        .map_err(|e| PlexError::Plex(format!("Unable to connect to Plex Media Server, {e}")))?;
    check_for_error(&text)?;
    Ok(text)
}

/// `HttpStatusCode.ToString()`: the .NET name of a status.
fn status_name(status: reqwest::StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => reason.split(' ').collect::<String>(),
        None => status.as_u16().to_string(),
    }
}

/// `CheckForError`: a `MediaContainer.error` in the body.
fn check_for_error(text: &str) -> Result<(), PlexError> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Ok(());
    };
    let error = if v.get("_children").is_some() {
        v["error"].as_str()
    } else {
        v["MediaContainer"]["error"].as_str()
    };
    match error.filter(|e| !e.trim().is_empty()) {
        Some(e) => Err(PlexError::Plex(e.to_string())),
        None => Ok(()),
    }
}

/// `GetTvSections`: the sections of type `show`.
async fn sections(notifier: &Notifier, settings: &Settings) -> Result<Vec<Section>, PlexError> {
    sections_of(notifier, settings, "show").await
}

/// `GetTvSections` / `GetMovieSections`: the sections of one type (`show`
/// for series, `movie` for movies).
pub(crate) async fn sections_of(
    notifier: &Notifier,
    settings: &Settings,
    section_type: &str,
) -> Result<Vec<Section>, PlexError> {
    tracing::debug!(host = %settings.host, "getting sections from Plex host");
    let text = get(notifier, settings, "library/sections", &[]).await?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| PlexError::Plex(format!("Unable to connect to Plex Media Server, {e}")))?;
    let directories = if v.get("_children").is_some() {
        v["_children"].as_array().cloned().unwrap_or_default()
    } else {
        v["MediaContainer"]["Directory"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    Ok(directories
        .iter()
        .filter(|d| d["type"].as_str() == Some(section_type))
        .map(|d| Section {
            id: d["key"]
                .as_i64()
                .or_else(|| d["key"].as_str().and_then(|k| k.parse().ok()))
                .unwrap_or(0),
            locations: d["Location"]
                .as_array()
                .or_else(|| d["_children"].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l["path"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// `Version`: the server's version string from `/identity`.
async fn version(notifier: &Notifier, settings: &Settings) -> Result<String, PlexError> {
    tracing::debug!(host = %settings.host, "getting version from Plex host");
    let text = get(notifier, settings, "identity", &[]).await?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| PlexError::Plex(format!("Unable to connect to Plex Media Server, {e}")))?;
    let raw = if v.get("_children").is_some() {
        v["version"].as_str()
    } else {
        v["MediaContainer"]["version"].as_str()
    };
    Ok(raw.unwrap_or("").to_string())
}

/// `GetVersion` + `ValidateVersion`: the leading four numbers, with the
/// one release whose library updates were broken refused.
fn validate_version(raw: &str) -> Result<(), PlexError> {
    validate_version_for(raw, "Sonarr")
}

/// The same, naming the application the message tells to restart.
pub(crate) fn validate_version_for(raw: &str, app: &str) -> Result<(), PlexError> {
    let mut parts = Vec::new();
    for piece in raw.split(['.', '-']) {
        match piece.parse::<i64>() {
            Ok(n) if parts.len() < 4 => parts.push(n),
            _ => break,
        }
    }
    if parts.len() >= 3 && parts[0] == 1 && parts[1] == 3 && parts[2] == 0 {
        return Err(PlexError::Other(format!(
            "Found version {raw}, upgrade to PMS 1.3.1 to fix library updating and then restart {app}"
        )));
    }
    Ok(())
}

/// `Update`: ask a section to refresh one path.
async fn refresh(
    notifier: &Notifier,
    settings: &Settings,
    section: i64,
    path: &str,
) -> Result<(), PlexError> {
    get(
        notifier,
        settings,
        &format!("library/sections/{section}/refresh"),
        &[("path", path)],
    )
    .await
    .map(|_| ())
}

/// `OsPath(MapTo) + (rootFolder - OsPath(MapFrom))`.
pub(crate) fn mapped_root(root: &str, settings: &Settings) -> String {
    if settings.map_to.trim().is_empty() {
        return root.to_string();
    }
    let from = settings.map_from.trim_end_matches('/');
    let to = settings.map_to.trim_end_matches('/');
    // `OsPath` subtraction: the root itself maps to `MapTo` alone.
    let mapped = if crate::mediafiles::path_equals(root, from) {
        to.to_string()
    } else {
        crate::mediafiles::join(to, crate::mediafiles::relative_path(from, root))
    };
    tracing::trace!("mapping path from {root} to {mapped} for partial scan");
    mapped
}

/// `UpdateSections`: the matching location, or every location.
async fn update_sections(
    notifier: &Notifier,
    settings: &Settings,
    series: &Series,
    sections: &[Section],
    root_folders: &[RootFolder],
) -> Result<(), PlexError> {
    update_sections_for(notifier, settings, &series.path, sections, root_folders, "TV").await
}

/// What `UpdateSections` asks Plex for: the section and the path of every
/// refresh, the one matching location first when there is one.
pub(crate) fn section_updates(
    settings: &Settings,
    path: &str,
    sections: &[Section],
    root_folders: &[RootFolder],
    library: &str,
) -> Vec<(i64, String)> {
    let root = crate::mediafiles::approve::best_root_folder_path(root_folders, path);
    let relative = crate::mediafiles::relative_path(&root, path).to_string();
    let mapped = mapped_root(&root, settings);
    for section in sections {
        for location in &section.locations {
            if crate::mediafiles::path_equals(location, &mapped) {
                tracing::debug!(location, "updating matching section location");
                return vec![(section.id, section_path(&relative, location))];
            }
        }
    }
    tracing::debug!("unable to find matching section location, updating all {library} sections");
    sections
        .iter()
        .flat_map(|section| {
            section
                .locations
                .iter()
                .map(|location| (section.id, section_path(&relative, location)))
        })
        .collect()
}

/// `UpdateSections` for a folder of the library, whatever it holds: a
/// series' for the TV sections, a movie's for the movie sections.
pub(crate) async fn update_sections_for(
    notifier: &Notifier,
    settings: &Settings,
    path: &str,
    sections: &[Section],
    root_folders: &[RootFolder],
    library: &str,
) -> Result<(), PlexError> {
    for (section, path) in section_updates(settings, path, sections, root_folders, library) {
        tracing::debug!(path, "updating section location");
        refresh(notifier, settings, section, &path).await?;
    }
    Ok(())
}

/// `UpdateSectionPath`: the location plus the folder's path under its
/// root, with the location's separator.
fn section_path(relative: &str, location: &str) -> String {
    let separator = if location.contains('\\') { "\\" } else { "/" };
    let relative = relative.replace(['\\', '/'], separator);
    format!("{location}{separator}{relative}")
}

/// `_versionCache.Get(settings.Host, () => GetVersion(settings), 2 hours)`.
pub(crate) async fn cached_version(
    notifier: &Notifier,
    settings: &Settings,
    now: DateTime<Utc>,
) -> Result<String, PlexError> {
    let cached = {
        let versions = notifier.plex_versions().lock().recover();
        versions
            .get(&settings.host)
            .filter(|(_, at)| *at + Duration::hours(2) > now)
            .map(|(v, _)| v.clone())
    };
    match cached {
        Some(v) => Ok(v),
        None => {
            let v = version(notifier, settings).await?;
            notifier
                .plex_versions()
                .lock()
                .recover()
                .insert(settings.host.clone(), (v.clone(), now));
            Ok(v)
        }
    }
}

/// `PlexServerService.UpdateLibrary`.
pub async fn update_library(
    notifier: &Notifier,
    settings: &Settings,
    series: &[Series],
    now: DateTime<Utc>,
) -> Result<(), PlexError> {
    tracing::debug!("sending update request to Plex server");
    let version = cached_version(notifier, settings, now).await?;
    validate_version(&version)?;
    let sections = sections(notifier, settings).await?;
    let root_folders = notifier
        .db()
        .call(|conn| store::catalog::list_root_folders(conn))
        .await
        .unwrap_or_default();
    for s in series {
        update_sections(notifier, settings, s, &sections, &root_folders).await?;
    }
    tracing::debug!("finished sending update request to Plex server");
    Ok(())
}

/// `PlexServerService.Test`: the sections, with the cached version
/// forgotten; every failure is a validation failure, never an error.
pub async fn test(notifier: &Notifier, settings: &Settings, _now: DateTime<Utc>) -> TestOutcome {
    test_for(notifier, settings, "show", "At least one TV library is required").await
}

/// The same for the sections of one type, with what a server without one
/// is told.
pub(crate) async fn test_for(
    notifier: &Notifier,
    settings: &Settings,
    section_type: &str,
    no_library: &str,
) -> TestOutcome {
    // `PlexServer.Test` pings plex.tv first, the same daily ping.
    ping_plex_tv(notifier, &settings.auth_token).await;
    notifier.plex_versions().lock().recover().remove(&settings.host);
    match sections_of(notifier, settings, section_type).await {
        Ok(sections) if sections.is_empty() => {
            TestOutcome::Failures(vec![ValidationFailure::error("Host", no_library)])
        }
        Ok(_) => TestOutcome::Failures(Vec::new()),
        Err(PlexError::Authentication) => {
            tracing::error!("unable to connect to Plex Media Server: the auth token is invalid");
            TestOutcome::Failures(vec![ValidationFailure::error(
                "AuthToken",
                "Authentication Token is invalid",
            )])
        }
        Err(PlexError::Plex(m)) => TestOutcome::Failures(vec![ValidationFailure::error(
            "Host",
            &format!("Unable to connect: {m}"),
        )]),
        Err(PlexError::Other(m)) => {
            tracing::error!(error = %m, "unable to connect to Plex Media Server");
            TestOutcome::Failures(vec![ValidationFailure::error(
                "Host",
                "Unable to connect to Plex Media Server",
            )
            .with_detail(&m)])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_broken_release_is_refused() {
        assert!(validate_version("1.41.0.8992-8e6b099a8").is_ok());
        assert!(validate_version("1.3.0.3112-1751929").is_err());
        assert!(validate_version("1.3.1.3121").is_ok());
        assert!(validate_version("").is_ok());
    }

    /// What Sonarr 4.0.20 answered on 21 November 2026 to `startOAuth`
    /// and to `continueOAuth`, with its client identifier.
    #[test]
    fn the_sign_in_addresses_are_the_originals() {
        let id = "06379e6c-b6a8-4dab-9ca2-d3c11680a12b";
        let identity = crate::notifications::Identity::default();
        assert_eq!(
            pin_url(id, &identity),
            json!({
                "url": "https://plex.tv/api/v2/pins?X-Plex-Client-Identifier=06379e6c-b6a8-4dab-9ca2-d3c11680a12b&X-Plex-Product=Sonarr&X-Plex-Platform=Windows&X-Plex-Platform-Version=7&X-Plex-Device-Name=Sonarr&X-Plex-Version=4.0.20.3014&strong=True",
                "method": "POST",
                "headers": {"accept": "application/json"},
            })
        );
        assert_eq!(
            sign_in_url(
                id,
                &identity,
                "http://localhost:8989/oauth.html",
                883714515,
                "kfh6p48ikyiaiziy3a3exxbtz"
            ),
            json!({
                "oauthUrl": "https://app.plex.tv/auth/#!?clientID=06379e6c-b6a8-4dab-9ca2-d3c11680a12b&forwardUrl=http%3A%2F%2Flocalhost%3A8989%2Foauth.html&code=kfh6p48ikyiaiziy3a3exxbtz&context%5Bdevice%5D%5Bproduct%5D=Sonarr&context%5Bdevice%5D%5Bplatform%5D=Windows&context%5Bdevice%5D%5BplatformVersion%5D=7&context%5Bdevice%5D%5Bversion%5D=4.0.20.3014",
                "pinId": 883714515,
            })
        );
    }

    /// `Convert.ToInt32` as Radarr 6.4.4 raised it for `continueOAuth`.
    #[test]
    fn pin_ids_are_read_as_int32() {
        assert_eq!(to_int32(" 12 "), Ok(12));
        assert_eq!(to_int32("+7"), Ok(7));
        let class = |r: Result<i32, ActionError>| match r {
            Err(ActionError::Server { exception, .. }) => exception,
            other => panic!("{other:?}"),
        };
        assert_eq!(class(to_int32("99999999999")), "System.OverflowException");
        assert_eq!(class(to_int32("abc")), "System.FormatException");
        assert_eq!(class(to_int32("")), "System.FormatException");
    }

    #[test]
    fn an_https_connection_also_offers_plain_http() {
        let servers = vec![(
            "Home".to_string(),
            vec![
                Connection {
                    uri: "https://192-168-1-10.abc.plex.direct:32400".into(),
                    protocol: "https".into(),
                    address: "192.168.1.10".into(),
                    port: 32400,
                    local: true,
                },
                Connection {
                    uri: String::new(),
                    protocol: "http".into(),
                    address: "203.0.113.7".into(),
                    port: 32401,
                    local: false,
                },
            ],
        )];
        assert_eq!(
            server_options(&servers),
            vec![
                json!({"value": "https://192-168-1-10.abc.plex.direct:32400", "name": "Home (192-168-1-10.abc.plex.direct)", "order": 0, "hint": "Local, Secure",
                       "additionalProperties": {"host": "192-168-1-10.abc.plex.direct", "port": 32400, "useSsl": true}}),
                json!({"value": "http://192.168.1.10:32400", "name": "Home (192.168.1.10)", "order": 0, "hint": "Local",
                       "additionalProperties": {"host": "192.168.1.10", "port": 32400, "useSsl": false}}),
                json!({"name": "Home (203.0.113.7)", "order": 0, "hint": "Remote",
                       "additionalProperties": {"host": "203.0.113.7", "port": 32401, "useSsl": false}}),
            ]
        );
    }

    #[test]
    fn roots_are_mapped() {
        let s = Settings {
            map_from: "/tv".into(),
            map_to: "/data/media/tv".into(),
            ..Settings::from_value(&json!({}))
        };
        assert_eq!(mapped_root("/tv", &s), "/data/media/tv");
        assert_eq!(mapped_root("/tv/kids", &s), "/data/media/tv/kids");
        let none = Settings::from_value(&json!({}));
        assert_eq!(mapped_root("/tv", &none), "/tv");
    }
}
