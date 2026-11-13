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
        let versions = notifier.plex_versions().lock().unwrap();
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
                .unwrap()
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
    notifier.plex_versions().lock().unwrap().remove(&settings.host);
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
