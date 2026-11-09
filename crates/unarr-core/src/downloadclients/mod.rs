//! Download clients: the programs Unarr hands releases to and watches, after
//! the originals' `IDownloadClient` family. Four are driven in this phase
//! (SABnzbd, NZBGet, qBittorrent, Transmission); the other implementations'
//! settings keep round-tripping through the API untouched.
//!
//! Each client is a thin proxy over its HTTP API plus the item mapping the
//! originals do in `GetItems`: a [`DownloadClientItem`] reads the same
//! whichever program produced it, so the queue, completed download handling
//! and failed download handling see one shape.

pub mod download;
pub mod nzbget;
pub mod qbittorrent;
pub mod remote_path;
pub mod sabnzbd;
pub mod service;
pub mod settings;
pub mod transmission;
mod version;

use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use crate::decision::{RemoteEpisode, SeedConfiguration};
use crate::model::{Protocol, Provider, RemotePathMapping};
use crate::validation::ValidationFailure;

pub use download::{download, DownloadError};
pub use service::{DownloadClientService, DownloadClientUnavailable};
pub use settings::{
    ClientSettings, NzbgetSettings, QBittorrentSettings, SabnzbdSettings, TransmissionSettings,
};

/// The name the clients' messages call the application. The texts are the
/// originals' (the Sonarr UI shows them verbatim), so the name is theirs.
pub const APP_NAME: &str = "Sonarr";

/// The implementations this phase drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientKind {
    Sabnzbd,
    Nzbget,
    QBittorrent,
    Transmission,
}

impl ClientKind {
    pub fn from_implementation(implementation: &str) -> Option<ClientKind> {
        match implementation {
            "Sabnzbd" => Some(ClientKind::Sabnzbd),
            "Nzbget" => Some(ClientKind::Nzbget),
            "QBittorrent" => Some(ClientKind::QBittorrent),
            "Transmission" => Some(ClientKind::Transmission),
            _ => None,
        }
    }

    /// The `implementation` the API names.
    pub fn implementation(self) -> &'static str {
        match self {
            ClientKind::Sabnzbd => "Sabnzbd",
            ClientKind::Nzbget => "Nzbget",
            ClientKind::QBittorrent => "QBittorrent",
            ClientKind::Transmission => "Transmission",
        }
    }

    /// The originals' `Name`, as validation messages spell it.
    pub fn display_name(self) -> &'static str {
        match self {
            ClientKind::Sabnzbd => "SABnzbd",
            ClientKind::Nzbget => "NZBGet",
            ClientKind::QBittorrent => "qBittorrent",
            ClientKind::Transmission => "Transmission",
        }
    }

    pub fn protocol(self) -> Protocol {
        match self {
            ClientKind::Sabnzbd | ClientKind::Nzbget => Protocol::Usenet,
            ClientKind::QBittorrent | ClientKind::Transmission => Protocol::Torrent,
        }
    }
}

/// `DownloadClientDefinition`: a stored client, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadClientDefinition {
    pub id: i64,
    pub name: String,
    pub kind: ClientKind,
    pub enable: bool,
    /// Lower goes first; the originals default to 1.
    pub priority: i64,
    pub remove_completed_downloads: bool,
    pub remove_failed_downloads: bool,
    pub tags: Vec<i64>,
    pub settings: ClientSettings,
}

impl DownloadClientDefinition {
    /// From a stored provider; `None` for implementations this phase does
    /// not drive.
    pub fn from_provider(p: &Provider) -> Option<DownloadClientDefinition> {
        let kind = ClientKind::from_implementation(&p.implementation)?;
        let a = &p.attributes;
        Some(DownloadClientDefinition {
            id: p.id,
            name: p.name.clone(),
            kind,
            enable: a["enable"].as_bool().unwrap_or(false),
            priority: a["priority"].as_i64().unwrap_or(1),
            remove_completed_downloads: a["removeCompletedDownloads"].as_bool().unwrap_or(true),
            remove_failed_downloads: a["removeFailedDownloads"].as_bool().unwrap_or(true),
            tags: p.tags.clone(),
            settings: ClientSettings::from_value(kind, &p.settings),
        })
    }

    pub fn protocol(&self) -> Protocol {
        self.kind.protocol()
    }

    /// `DownloadClientItemClientInfo.FromDownloadClient`.
    pub fn item_client_info(&self, has_post_import_category: bool) -> ItemClientInfo {
        ItemClientInfo {
            protocol: self.protocol(),
            kind: self.kind,
            id: self.id,
            name: self.name.clone(),
            remove_completed_downloads: self.remove_completed_downloads,
            has_post_import_category,
        }
    }
}

/// `DownloadItemStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DownloadItemStatus {
    #[default]
    Queued,
    Paused,
    Downloading,
    Completed,
    Failed,
    Warning,
}

impl DownloadItemStatus {
    /// The camel-case name the queue resource uses.
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadItemStatus::Queued => "queued",
            DownloadItemStatus::Paused => "paused",
            DownloadItemStatus::Downloading => "downloading",
            DownloadItemStatus::Completed => "completed",
            DownloadItemStatus::Failed => "failed",
            DownloadItemStatus::Warning => "warning",
        }
    }
}

/// `DownloadClientItemClientInfo`: which client an item came from.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemClientInfo {
    pub protocol: Protocol,
    pub kind: ClientKind,
    pub id: i64,
    pub name: String,
    pub remove_completed_downloads: bool,
    pub has_post_import_category: bool,
}

/// `DownloadClientItem`: one download as the client reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadClientItem {
    pub client: ItemClientInfo,
    /// The id the client knows the download by (nzo id, drone id, info hash).
    pub download_id: String,
    pub category: Option<String>,
    pub title: String,
    pub total_size: i64,
    pub remaining_size: i64,
    pub remaining_time: Option<Duration>,
    pub seed_ratio: Option<f64>,
    /// Where the files are, remapped to a local path; `None` until known.
    pub output_path: Option<String>,
    pub message: Option<String>,
    pub status: DownloadItemStatus,
    pub is_encrypted: bool,
    pub can_move_files: bool,
    pub can_be_removed: bool,
    /// `Removed`: Unarr already asked the client to drop the item.
    pub removed: bool,
}

impl DownloadClientItem {
    pub fn new(
        client: ItemClientInfo,
        download_id: impl Into<String>,
        title: impl Into<String>,
    ) -> DownloadClientItem {
        DownloadClientItem {
            client,
            download_id: download_id.into(),
            category: None,
            title: title.into(),
            total_size: 0,
            remaining_size: 0,
            remaining_time: None,
            seed_ratio: None,
            output_path: None,
            message: None,
            status: DownloadItemStatus::Queued,
            is_encrypted: false,
            can_move_files: false,
            can_be_removed: false,
            removed: false,
        }
    }
}

/// `DownloadClientInfo`: what `GetStatus` says about a client.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DownloadClientInfo {
    pub is_localhost: bool,
    pub sorting_mode: Option<String>,
    pub removes_completed_downloads: bool,
    pub output_root_folders: Vec<String>,
}

/// The originals' `DownloadClientException` family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// `DownloadClientAuthenticationException`.
    Authentication(String),
    /// `DownloadClientUnavailableException`: the program could not be reached.
    Unavailable(String),
    /// `DownloadClientRejectedReleaseException`: it refused the release.
    Rejected(String),
    /// `NotSupportedException`: the client cannot take this kind of release.
    NotSupported(String),
    /// Any other `DownloadClientException`.
    Other(String),
}

impl ClientError {
    pub fn message(&self) -> &str {
        match self {
            ClientError::Authentication(m)
            | ClientError::Unavailable(m)
            | ClientError::Rejected(m)
            | ClientError::NotSupported(m)
            | ClientError::Other(m) => m,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ClientError {}

pub type ClientResult<T> = Result<T, ClientError>;

/// What every client needs besides its settings: the HTTP client, the
/// remote path mappings, how much history to read and the clock.
#[derive(Clone)]
pub struct ClientContext {
    pub http: reqwest::Client,
    pub mappings: Vec<RemotePathMapping>,
    /// `DownloadClientHistoryLimit`: how many history items to read.
    pub history_limit: usize,
    /// The library's root folders, which a download's files are never in.
    pub root_folders: Vec<String>,
    pub now: DateTime<Utc>,
}

impl ClientContext {
    pub fn new(http: reqwest::Client, now: DateTime<Utc>) -> ClientContext {
        ClientContext {
            http,
            mappings: Vec::new(),
            history_limit: 60,
            root_folders: Vec::new(),
            now,
        }
    }

    /// `RemapRemoteToLocal` for the client at `host`.
    pub fn remap(&self, host: &str, remote: &str) -> String {
        remote_path::remap_remote_to_local(&self.mappings, host, remote)
    }
}

/// A download client bound to its definition: the originals' concrete
/// `IDownloadClient` for one stored definition.
pub struct Client<'a> {
    pub definition: &'a DownloadClientDefinition,
    pub context: &'a ClientContext,
}

impl<'a> Client<'a> {
    pub fn new(definition: &'a DownloadClientDefinition, context: &'a ClientContext) -> Client<'a> {
        Client { definition, context }
    }

    pub fn kind(&self) -> ClientKind {
        self.definition.kind
    }

    pub fn protocol(&self) -> Protocol {
        self.definition.protocol()
    }

    /// `AddFromNzbFile`: the download id the client reports.
    pub async fn add_nzb(
        &self,
        remote: &RemoteEpisode,
        filename: &str,
        nzb: Vec<u8>,
    ) -> ClientResult<String> {
        match &self.definition.settings {
            ClientSettings::Sabnzbd(s) => {
                sabnzbd::Sabnzbd::new(self, s)
                    .add_nzb(remote, filename, nzb)
                    .await
            }
            ClientSettings::Nzbget(s) => nzbget::Nzbget::new(self, s).add_nzb(remote, filename, nzb).await,
            _ => Err(ClientError::NotSupported(format!(
                "{} does not take NZBs",
                self.kind().display_name()
            ))),
        }
    }

    /// `AddFromMagnetLink`: the info hash the client reports.
    pub async fn add_magnet(&self, remote: &RemoteEpisode, hash: &str, magnet: &str) -> ClientResult<String> {
        match &self.definition.settings {
            ClientSettings::QBittorrent(s) => {
                qbittorrent::QBittorrent::new(self, s)
                    .add_magnet(remote, hash, magnet)
                    .await
            }
            ClientSettings::Transmission(s) => {
                transmission::Transmission::new(self, s)
                    .add_magnet(remote, hash, magnet)
                    .await
            }
            _ => Err(ClientError::NotSupported(format!(
                "{} does not take magnet links",
                self.kind().display_name()
            ))),
        }
    }

    /// `AddFromTorrentFile`: the info hash the client reports.
    pub async fn add_torrent(
        &self,
        remote: &RemoteEpisode,
        hash: &str,
        filename: &str,
        torrent: Vec<u8>,
    ) -> ClientResult<String> {
        match &self.definition.settings {
            ClientSettings::QBittorrent(s) => {
                qbittorrent::QBittorrent::new(self, s)
                    .add_torrent(remote, hash, filename, torrent)
                    .await
            }
            ClientSettings::Transmission(s) => {
                transmission::Transmission::new(self, s)
                    .add_torrent(remote, hash, filename, torrent)
                    .await
            }
            _ => Err(ClientError::NotSupported(format!(
                "{} does not take torrent files",
                self.kind().display_name()
            ))),
        }
    }

    /// `GetItems`: everything in the client's category, queue and history.
    pub async fn items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        match &self.definition.settings {
            ClientSettings::Sabnzbd(s) => sabnzbd::Sabnzbd::new(self, s).items().await,
            ClientSettings::Nzbget(s) => nzbget::Nzbget::new(self, s).items().await,
            ClientSettings::QBittorrent(s) => qbittorrent::QBittorrent::new(self, s).items().await,
            ClientSettings::Transmission(s) => transmission::Transmission::new(self, s).items().await,
        }
    }

    /// `GetImportItem`: the item with its output path filled in where the
    /// listing could not say.
    pub async fn import_item(&self, item: &DownloadClientItem) -> ClientResult<DownloadClientItem> {
        match &self.definition.settings {
            ClientSettings::QBittorrent(s) => qbittorrent::QBittorrent::new(self, s).import_item(item).await,
            _ => Ok(item.clone()),
        }
    }

    /// `RemoveItem`, with the data when `delete_data`.
    pub async fn remove(&self, item: &DownloadClientItem, delete_data: bool) -> ClientResult<()> {
        match &self.definition.settings {
            ClientSettings::Sabnzbd(s) => sabnzbd::Sabnzbd::new(self, s).remove(item, delete_data).await,
            ClientSettings::Nzbget(s) => nzbget::Nzbget::new(self, s).remove(item, delete_data).await,
            ClientSettings::QBittorrent(s) => {
                qbittorrent::QBittorrent::new(self, s)
                    .remove(item, delete_data)
                    .await
            }
            ClientSettings::Transmission(s) => {
                transmission::Transmission::new(self, s)
                    .remove(item, delete_data)
                    .await
            }
        }
    }

    /// `MarkItemAsImported`: the post-import category, where supported.
    pub async fn mark_imported(&self, item: &DownloadClientItem) -> ClientResult<()> {
        match &self.definition.settings {
            ClientSettings::QBittorrent(s) => {
                qbittorrent::QBittorrent::new(self, s).mark_imported(item).await
            }
            ClientSettings::Transmission(s) => {
                transmission::Transmission::new(self, s).mark_imported(item).await
            }
            _ => Err(ClientError::NotSupported(format!(
                "{} does not support marking items as imported",
                self.kind().display_name()
            ))),
        }
    }

    /// `GetStatus`.
    pub async fn status(&self) -> ClientResult<DownloadClientInfo> {
        match &self.definition.settings {
            ClientSettings::Sabnzbd(s) => sabnzbd::Sabnzbd::new(self, s).status().await,
            ClientSettings::Nzbget(s) => nzbget::Nzbget::new(self, s).status().await,
            ClientSettings::QBittorrent(s) => qbittorrent::QBittorrent::new(self, s).status().await,
            ClientSettings::Transmission(s) => transmission::Transmission::new(self, s).status().await,
        }
    }

    /// `Test()`: the connection test, every failure the originals raise, an
    /// unexpected error wrapped as they wrap it.
    pub async fn test(&self) -> Vec<ValidationFailure> {
        let mut failures = Vec::new();
        let result = match &self.definition.settings {
            ClientSettings::Sabnzbd(s) => sabnzbd::Sabnzbd::new(self, s).test(&mut failures).await,
            ClientSettings::Nzbget(s) => nzbget::Nzbget::new(self, s).test(&mut failures).await,
            ClientSettings::QBittorrent(s) => {
                qbittorrent::QBittorrent::new(self, s).test(&mut failures).await
            }
            ClientSettings::Transmission(s) => {
                transmission::Transmission::new(self, s).test(&mut failures).await
            }
        };
        if let Err(e) = result {
            // The failures found before the error are kept, as the originals'
            // list is.
            failures.push(ValidationFailure::error(
                "",
                &format!("Test was aborted due to an error: {}", e.message()),
            ));
        }
        failures
    }
}

/// `http://host:port/urlBase`, as `HttpRequestBuilder.BuildBaseUrl` does.
pub fn build_base_url(use_ssl: bool, host: &str, port: i64, url_base: &str) -> String {
    let scheme = if use_ssl { "https" } else { "http" };
    let base = url_base.trim();
    let base = if base.is_empty() || base.starts_with('/') {
        base.to_string()
    } else {
        format!("/{base}")
    };
    format!("{scheme}://{host}:{port}{base}")
}

/// A client's answer read with [`crate::http::read_text`]'s ceiling;
/// `transport` words a transport failure the way the client's messages do.
pub(crate) async fn read_text(
    response: reqwest::Response,
    name: &str,
    transport: impl FnOnce(&reqwest::Error) -> ClientError,
) -> ClientResult<String> {
    match crate::http::read_text(response, crate::http::MAX_RESPONSE_BYTES).await {
        Ok(text) => Ok(text),
        Err(crate::http::BodyError::Transport(e)) => Err(transport(&e)),
        Err(too_large) => Err(ClientError::Other(format!(
            "Unable to connect to {name}, {too_large}"
        ))),
    }
}

/// Ten years: more "time left" than any download has. Past it a client's
/// figure is garbage, and adding it to a date would panic further on.
const MAX_TIME_LEFT_SECS: i64 = 10 * 365 * 24 * 3600;

/// A client's remaining time as a duration, clamped to what a date can take.
pub fn time_left(seconds: i64) -> chrono::Duration {
    chrono::Duration::seconds(seconds.clamp(0, MAX_TIME_LEFT_SECS))
}

/// `HttpUri.CombinePath`: `base` and `path` with one slash between.
pub fn combine_path(base: &str, path: &str) -> String {
    if path.is_empty() {
        return base.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// `"host" == "127.0.0.1" || "localhost"`.
pub fn is_localhost(host: &str) -> bool {
    host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")
}

/// The innermost cause of a transport error, the way the originals' messages
/// carry the socket error ("Connection refused (127.0.0.1:1)").
pub fn transport_message(e: &reqwest::Error) -> String {
    let endpoint = e
        .url()
        .map(|u| {
            format!(
                "{}:{}",
                u.host_str().unwrap_or(""),
                u.port_or_known_default().unwrap_or(0)
            )
        })
        .unwrap_or_default();
    let mut source: &dyn std::error::Error = e;
    let mut io_kind = None;
    while let Some(next) = source.source() {
        source = next;
        if let Some(io) = source.downcast_ref::<std::io::Error>() {
            io_kind = Some(io.kind());
        }
    }
    let text = source.to_string();
    if io_kind == Some(std::io::ErrorKind::ConnectionRefused) || text.contains("Connection refused") {
        return format!("Connection refused ({endpoint})");
    }
    if text.contains("Name or service not known") || text.contains("failed to lookup address") {
        return format!("Name or service not known ({endpoint})");
    }
    if e.is_timeout() && !text.to_lowercase().contains("time") {
        format!("The request timed out: {text}")
    } else {
        text
    }
}

/// `HttpException.Message`: "HTTP request failed: [404:NotFound] [GET] at [url]".
pub fn http_error_message(status: reqwest::StatusCode, method: &str, url: &str) -> String {
    let reason = status.canonical_reason().unwrap_or("").replace(' ', "");
    format!(
        "HTTP request failed: [{}:{}] [{}] at [{}]",
        status.as_u16(),
        reason,
        method,
        crate::http::redact(url)
    )
}

/// The seed limits a torrent client is asked to apply, when the indexer set any.
pub fn seed_limits(remote: &RemoteEpisode) -> Option<&SeedConfiguration> {
    remote
        .seed_configuration
        .as_ref()
        .filter(|s| s.ratio.is_some() || s.seed_time_minutes.is_some())
}

/// Read a settings value as text, `""` when missing or null.
pub(crate) fn text(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn int(v: &Value, key: &str, default: i64) -> i64 {
    match &v[key] {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(default),
        Value::String(s) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

pub(crate) fn flag(v: &Value, key: &str, default: bool) -> bool {
    match &v[key] {
        Value::Bool(b) => *b,
        Value::String(s) => s.eq_ignore_ascii_case("true"),
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_left_is_clamped_to_what_a_date_can_take() {
        assert_eq!(time_left(600), chrono::Duration::minutes(10));
        assert_eq!(time_left(-5), chrono::Duration::zero());
        let capped = time_left(9_000_000_000_000_000);
        assert_eq!(capped, chrono::Duration::seconds(MAX_TIME_LEFT_SECS));
        // What Transmission or SABnzbd may claim, added to today, is a date.
        assert!(Utc::now().checked_add_signed(capped).is_some());
    }

    #[test]
    fn builds_base_urls_as_the_originals_do() {
        assert_eq!(
            build_base_url(false, "localhost", 8080, ""),
            "http://localhost:8080"
        );
        assert_eq!(
            build_base_url(true, "nas", 443, "sabnzbd"),
            "https://nas:443/sabnzbd"
        );
        assert_eq!(build_base_url(false, "nas", 80, "/x/"), "http://nas:80/x/");
        assert_eq!(combine_path("http://nas:80/x/", "api"), "http://nas:80/x/api");
        assert_eq!(combine_path("http://nas:80", "/jsonrpc"), "http://nas:80/jsonrpc");
        assert_eq!(combine_path("http://nas:80", ""), "http://nas:80");
    }

    #[test]
    fn client_kinds_map_to_protocols() {
        assert_eq!(
            ClientKind::from_implementation("Sabnzbd"),
            Some(ClientKind::Sabnzbd)
        );
        assert_eq!(ClientKind::from_implementation("Deluge"), None);
        assert_eq!(ClientKind::Nzbget.protocol(), Protocol::Usenet);
        assert_eq!(ClientKind::Transmission.protocol(), Protocol::Torrent);
        assert_eq!(ClientKind::QBittorrent.display_name(), "qBittorrent");
    }
}
