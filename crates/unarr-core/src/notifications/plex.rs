//! Plex Media Server, after the originals' `PlexServer`,
//! `PlexServerService` and `PlexServerProxy`: the TV sections whose
//! location holds the series' root folder are asked to refresh the series'
//! folder. The plex.tv side (sign-in, server list, the ping on each
//! update) is not reproduced: it needs the user's plex.tv account.

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
        .map_err(|e| PlexError::Plex(format!("Unable to connect to Plex Media Server, {e}")))?;
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
        .filter(|d| d["type"].as_str() == Some("show"))
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
    let mut parts = Vec::new();
    for piece in raw.split(['.', '-']) {
        match piece.parse::<i64>() {
            Ok(n) if parts.len() < 4 => parts.push(n),
            _ => break,
        }
    }
    if parts.len() >= 3 && parts[0] == 1 && parts[1] == 3 && parts[2] == 0 {
        return Err(PlexError::Other(format!(
            "Found version {raw}, upgrade to PMS 1.3.1 to fix library updating and then restart Sonarr"
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
fn mapped_root(root: &str, settings: &Settings) -> String {
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
    let root = crate::mediafiles::approve::best_root_folder_path(root_folders, &series.path);
    let relative = crate::mediafiles::relative_path(&root, &series.path).to_string();
    let mapped = mapped_root(&root, settings);
    for section in sections {
        for location in &section.locations {
            if crate::mediafiles::path_equals(location, &mapped) {
                tracing::debug!(location, "updating matching section location");
                return update_section_path(notifier, settings, &relative, section.id, location).await;
            }
        }
    }
    tracing::debug!("unable to find matching section location, updating all TV sections");
    for section in sections {
        for location in &section.locations {
            update_section_path(notifier, settings, &relative, section.id, location).await?;
        }
    }
    Ok(())
}

/// `UpdateSectionPath`: the location plus the series' path under its root,
/// with the location's separator.
async fn update_section_path(
    notifier: &Notifier,
    settings: &Settings,
    relative: &str,
    section: i64,
    location: &str,
) -> Result<(), PlexError> {
    let separator = if location.contains('\\') { "\\" } else { "/" };
    let relative = relative.replace(['\\', '/'], separator);
    let path = format!("{location}{separator}{relative}");
    tracing::debug!(location, "updating section location");
    refresh(notifier, settings, section, &path).await
}

/// `PlexServerService.UpdateLibrary`.
pub async fn update_library(
    notifier: &Notifier,
    settings: &Settings,
    series: &[Series],
    now: DateTime<Utc>,
) -> Result<(), PlexError> {
    tracing::debug!("sending update request to Plex server");
    let cached = {
        let versions = notifier.plex_versions().lock().unwrap();
        versions
            .get(&settings.host)
            .filter(|(_, at)| *at + Duration::hours(2) > now)
            .map(|(v, _)| v.clone())
    };
    let version = match cached {
        Some(v) => v,
        None => {
            let v = version(notifier, settings).await?;
            notifier
                .plex_versions()
                .lock()
                .unwrap()
                .insert(settings.host.clone(), (v.clone(), now));
            v
        }
    };
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
    notifier.plex_versions().lock().unwrap().remove(&settings.host);
    match sections(notifier, settings).await {
        Ok(sections) if sections.is_empty() => TestOutcome::Failures(vec![ValidationFailure::error(
            "Host",
            "At least one TV library is required",
        )]),
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
