//! Transmission, after the originals' `Transmission`, `TransmissionBase`
//! and `TransmissionProxy`: the RPC endpoint with basic authentication and
//! the session id it hands out in a 409, torrents told apart by label
//! (4.0+), directory or category folder.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use super::settings::TransmissionSettings;
use super::version::Version;
use super::GrabRequest;
use super::{
    build_base_url, combine_path, is_localhost, remote_path, transport_message, Client, ClientError,
    ClientResult, DownloadClientInfo, DownloadClientItem, DownloadItemStatus,
};
use crate::validation::ValidationFailure;

const NAME: &str = "Transmission";
const SESSION_HEADER: &str = "X-Transmission-Session-Id";

/// `TransmissionTorrentStatus`.
const STATUS_STOPPED: i64 = 0;
const STATUS_CHECK_WAIT: i64 = 1;
const STATUS_CHECK: i64 = 2;
const STATUS_QUEUED: i64 = 3;
const STATUS_SEEDING_WAIT: i64 = 5;
const STATUS_SEEDING: i64 = 6;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "rpc-version")]
    pub rpc_version: Value,
    pub version: String,
    #[serde(rename = "download-dir")]
    pub download_dir: String,
    #[serde(rename = "seedRatioLimit")]
    pub seed_ratio_limit: f64,
    #[serde(rename = "seedRatioLimited")]
    pub seed_ratio_limited: bool,
    #[serde(rename = "idle-seeding-limit")]
    pub idle_seeding_limit: i64,
    #[serde(rename = "idle-seeding-limit-enabled")]
    pub idle_seeding_limit_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Torrent {
    pub id: i64,
    pub hash_string: String,
    pub name: String,
    pub download_dir: String,
    pub total_size: i64,
    pub left_until_done: i64,
    pub is_finished: bool,
    pub labels: Vec<String>,
    pub eta: i64,
    pub status: i64,
    pub seconds_downloading: i64,
    pub seconds_seeding: i64,
    pub error_string: String,
    pub downloaded_ever: i64,
    pub uploaded_ever: i64,
    pub seed_ratio_limit: f64,
    pub seed_ratio_mode: i64,
    pub seed_idle_limit: i64,
    pub seed_idle_mode: i64,
}

/// `_authSessionIdCache`: the session id per (base URL, password).
fn session_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The version number in "4.0.6 (38c164933e)": the first run of digits and
/// dots outside the parentheses.
fn version_number(raw: &str) -> Option<Version> {
    let outside = raw.split('(').next().unwrap_or(raw);
    let token: String = outside
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    Version::parse(&token)
}

pub struct Transmission<'a> {
    client: &'a Client<'a>,
    settings: &'a TransmissionSettings,
}

impl<'a> Transmission<'a> {
    pub fn new(client: &'a Client<'a>, settings: &'a TransmissionSettings) -> Transmission<'a> {
        Transmission { client, settings }
    }

    pub fn base_url(&self) -> String {
        let s = self.settings;
        build_base_url(s.use_ssl, &s.host, s.port, &s.url_base)
    }

    fn rpc_url(&self) -> String {
        combine_path(&self.base_url(), "rpc")
    }

    fn request(&self, method: reqwest::Method) -> reqwest::RequestBuilder {
        let s = self.settings;
        let mut builder = self
            .client
            .context
            .http
            .request(method, self.rpc_url())
            .header(reqwest::header::ACCEPT, "application/json");
        if !s.username.is_empty() || !s.password.is_empty() {
            builder = builder.basic_auth(&s.username, Some(&s.password));
        }
        builder
    }

    fn unavailable(e: &reqwest::Error) -> ClientError {
        let message = transport_message(e);
        if message.to_ascii_lowercase().contains("certificate") {
            ClientError::Unavailable(format!(
                "Unable to connect to {NAME}, certificate validation failed."
            ))
        } else {
            // A socket error is an `HttpRequestException` to the originals,
            // not a `WebException`: it reaches the test as is.
            ClientError::Other(message)
        }
    }

    /// `AuthenticateClient`: the session id, from the cache or from the
    /// 409 Transmission answers a request without one.
    async fn authenticate(&self, reauthenticate: bool) -> ClientResult<String> {
        let app = self.settings.app.name;
        let key = format!("{}:{}", self.base_url(), self.settings.password);
        if !reauthenticate {
            if let Some(id) = session_cache().lock().unwrap().get(&key) {
                return Ok(id.clone());
            }
        }
        session_cache().lock().unwrap().remove(&key);
        let response = self
            .request(reqwest::Method::GET)
            .send()
            .await
            .map_err(|e| Self::unavailable(&e))?;
        let session = match response.status() {
            reqwest::StatusCode::MOVED_PERMANENTLY => {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                return Err(ClientError::Other(format!("Remote site redirected to {location}")));
            }
            reqwest::StatusCode::FORBIDDEN => {
                return Err(ClientError::Other(format!(
                    "Failed to authenticate with {NAME}. It may be necessary to add {app}'s IP address to RPC whitelist."
                )))
            }
            reqwest::StatusCode::CONFLICT => response
                .headers()
                .get(SESSION_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(String::from)
                .ok_or_else(|| ClientError::Other("Remote host did not return a Session Id.".into()))?,
            _ => return Err(ClientError::Authentication(format!("Failed to authenticate with {NAME}."))),
        };
        tracing::debug!("Transmission authentication succeeded");
        session_cache().lock().unwrap().insert(key, session.clone());
        Ok(session)
    }

    /// `ProcessRequest`: the `arguments` of a successful answer.
    pub async fn process(&self, method: &str, arguments: Option<Value>) -> ClientResult<Value> {
        let mut body = json!({"method": method});
        if let Some(args) = arguments {
            body["arguments"] = args;
        }
        let mut session = self.authenticate(false).await?;
        for attempt in 0..2 {
            let response = self
                .request(reqwest::Method::POST)
                .header(SESSION_HEADER, session.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .await
                .map_err(|e| Self::unavailable(&e))?;
            let status = response.status();
            if status == reqwest::StatusCode::CONFLICT && attempt == 0 {
                session = self.authenticate(true).await?;
                continue;
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(ClientError::Authentication("User authentication failed.".into()));
            }
            let content = super::read_text(response, NAME, Self::unavailable).await?;
            let parsed: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
            if parsed.is_null() {
                return Err(ClientError::Other("Unexpected response".into()));
            }
            let result = parsed["result"].as_str().unwrap_or("");
            if result != "success" {
                return Err(ClientError::Other(result.to_string()));
            }
            return Ok(parsed["arguments"].clone());
        }
        unreachable!("the retry loop returns")
    }

    pub async fn config(&self) -> ClientResult<Config> {
        let args = self.process("session-get", None).await?;
        serde_json::from_value(args)
            .map_err(|e| ClientError::Other(format!("Unexpected {NAME} session: {e}")))
    }

    pub async fn client_version(&self) -> ClientResult<String> {
        Ok(self.config().await?.version)
    }

    pub async fn torrents(&self, hashes: Option<&[String]>) -> ClientResult<Vec<Torrent>> {
        let fields = [
            "id",
            "hashString",
            "name",
            "downloadDir",
            "totalSize",
            "leftUntilDone",
            "isFinished",
            "eta",
            "status",
            "secondsDownloading",
            "secondsSeeding",
            "errorString",
            "uploadedEver",
            "downloadedEver",
            "seedRatioLimit",
            "seedRatioMode",
            "seedIdleLimit",
            "seedIdleMode",
            "fileCount",
            "file-count",
            "labels",
        ];
        let mut args = json!({"fields": fields});
        if let Some(h) = hashes {
            args["ids"] = json!(h);
        }
        let result = self.process("torrent-get", Some(args)).await?;
        serde_json::from_value(result["torrents"].clone())
            .map_err(|e| ClientError::Other(format!("Unexpected {NAME} torrents: {e}")))
    }

    fn add_arguments(&self, download_dir: Option<&str>) -> Value {
        let mut args = json!({"paused": self.settings.add_paused});
        if let Some(dir) = download_dir.filter(|d| !d.trim().is_empty()) {
            args["download-dir"] = json!(dir);
        }
        if !self.settings.category.trim().is_empty() {
            args["labels"] = json!([self.settings.category]);
        }
        args
    }

    pub async fn add_from_url(&self, url: &str, download_dir: Option<&str>) -> ClientResult<()> {
        let mut args = self.add_arguments(download_dir);
        args["filename"] = json!(url);
        self.process("torrent-add", Some(args)).await.map(|_| ())
    }

    pub async fn add_from_data(&self, torrent: &[u8], download_dir: Option<&str>) -> ClientResult<()> {
        let mut args = self.add_arguments(download_dir);
        args["metainfo"] = json!(base64::engine::general_purpose::STANDARD.encode(torrent));
        self.process("torrent-add", Some(args)).await.map(|_| ())
    }

    pub async fn set_seeding(&self, hash: &str, request: &GrabRequest<'_>) -> ClientResult<()> {
        let Some(seed) = request.seed_limits() else {
            return Ok(());
        };
        let mut args = json!({"ids": [hash]});
        if let Some(ratio) = seed.ratio {
            args["seedRatioLimit"] = json!(ratio);
            args["seedRatioMode"] = json!(1);
        }
        if let Some(minutes) = seed.seed_time_minutes {
            // Transmission has no seed time limit; the idle limit stands in.
            args["seedIdleLimit"] = json!(minutes);
            args["seedIdleMode"] = json!(1);
        }
        self.process("torrent-set", Some(args)).await.map(|_| ())
    }

    pub async fn remove_torrent(&self, hash: &str, delete_data: bool) -> ClientResult<()> {
        self.process(
            "torrent-remove",
            Some(json!({"ids": [hash], "delete-local-data": delete_data})),
        )
        .await
        .map(|_| ())
    }

    pub async fn move_to_top(&self, hash: &str) -> ClientResult<()> {
        self.process("queue-move-top", Some(json!({"ids": [hash]})))
            .await
            .map(|_| ())
    }

    pub async fn set_labels(&self, hash: &str, labels: &BTreeSet<String>) -> ClientResult<()> {
        self.process("torrent-set", Some(json!({"ids": [hash], "labels": labels})))
            .await
            .map(|_| ())
    }

    // --- client

    async fn supports_labels(&self) -> ClientResult<bool> {
        let raw = self.client_version().await?;
        Ok(version_number(&raw).is_some_and(|v| v >= Version::new(4, 0)))
    }

    async fn download_directory(&self) -> ClientResult<Option<String>> {
        if !self.settings.directory.trim().is_empty() {
            return Ok(Some(self.settings.directory.clone()));
        }
        if self.settings.category.trim().is_empty() {
            return Ok(None);
        }
        let config = self.config().await?;
        Ok(Some(format!(
            "{}/{}",
            config.download_dir.trim_end_matches('/'),
            self.settings.category
        )))
    }

    async fn after_add(&self, request: &GrabRequest<'_>, hash: &str) -> ClientResult<()> {
        self.set_seeding(hash, request).await?;
        if self.settings.move_to_top(request.recent) {
            self.move_to_top(hash).await?;
        }
        Ok(())
    }

    pub async fn add_magnet(
        &self,
        request: &GrabRequest<'_>,
        hash: &str,
        magnet: &str,
    ) -> ClientResult<String> {
        let dir = self.download_directory().await?;
        self.add_from_url(magnet, dir.as_deref()).await?;
        self.after_add(request, hash).await?;
        Ok(hash.to_string())
    }

    pub async fn add_torrent(
        &self,
        request: &GrabRequest<'_>,
        hash: &str,
        _filename: &str,
        torrent: Vec<u8>,
    ) -> ClientResult<String> {
        let dir = self.download_directory().await?;
        self.add_from_data(&torrent, dir.as_deref()).await?;
        self.after_add(request, hash).await?;
        Ok(hash.to_string())
    }

    pub async fn items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let supports_labels = self.supports_labels().await?;
        let torrents = self.torrents(None).await?;
        let mut config: Option<Config> = None;
        let s = self.settings;
        let has_post_import = !s.imported_category.trim().is_empty() && supports_labels;
        let mut out = Vec::new();
        for t in torrents {
            let output_dir = t.download_dir.clone();
            if !s.category.trim().is_empty() && supports_labels && !t.labels.is_empty() {
                if !t.labels.iter().any(|l| l.eq_ignore_ascii_case(&s.category)) {
                    continue;
                }
            } else if !s.directory.trim().is_empty() {
                if !remote_path::contains(&s.directory, &output_dir) {
                    continue;
                }
            } else if !s.category.trim().is_empty() && !output_dir.split(['/', '\\']).any(|d| d == s.category)
            {
                continue;
            }
            let output_dir = self.client.context.remap(&s.host, &output_dir);
            let mut item = DownloadClientItem::new(
                self.client.definition.item_client_info(has_post_import),
                t.hash_string.to_ascii_uppercase(),
                t.name.clone(),
            );
            item.category = Some(s.category.clone());
            item.output_path = Some(remote_path::join(&output_dir, &t.name.replace(':', "_")));
            item.total_size = t.total_size;
            item.remaining_size = t.left_until_done;
            let ratio = if t.downloaded_ever <= 0 {
                0.0
            } else {
                t.uploaded_ever as f64 / t.downloaded_ever as f64
            };
            item.seed_ratio = Some(ratio);
            if t.eta >= 0 {
                item.remaining_time = Some(super::time_left(t.eta));
            }
            item.status = if !t.error_string.trim().is_empty() {
                item.message = Some(t.error_string.clone());
                DownloadItemStatus::Warning
            } else if t.total_size == 0 {
                DownloadItemStatus::Queued
            } else if (t.left_until_done == 0
                && matches!(t.status, STATUS_STOPPED | STATUS_SEEDING | STATUS_SEEDING_WAIT))
                || (t.is_finished && t.status != STATUS_CHECK && t.status != STATUS_CHECK_WAIT)
            {
                DownloadItemStatus::Completed
            } else if t.status == STATUS_QUEUED {
                DownloadItemStatus::Queued
            } else {
                DownloadItemStatus::Downloading
            };
            let reached = if item.client.remove_completed_downloads {
                if config.is_none() && (t.seed_ratio_mode == 0 || t.seed_idle_mode == 0) {
                    config = Some(self.config().await?);
                }
                has_reached_seed_limit(&t, ratio, config.as_ref())
            } else {
                false
            };
            item.can_be_removed = reached;
            item.can_move_files = reached && t.status == STATUS_STOPPED;
            out.push(item);
        }
        Ok(out)
    }

    pub async fn remove(&self, item: &DownloadClientItem, delete_data: bool) -> ClientResult<()> {
        self.remove_torrent(&item.download_id.to_ascii_lowercase(), delete_data)
            .await
    }

    pub async fn mark_imported(&self, item: &DownloadClientItem) -> ClientResult<()> {
        if !self.supports_labels().await? {
            return Err(ClientError::NotSupported(format!(
                "{NAME} does not support marking items as imported"
            )));
        }
        let s = self.settings;
        let imported = s.imported_category.trim();
        if imported.is_empty() || imported == s.category {
            return Ok(());
        }
        let hash = item.download_id.to_ascii_lowercase();
        let Some(torrent) = self
            .torrents(Some(std::slice::from_ref(&hash)))
            .await?
            .into_iter()
            .next()
        else {
            tracing::warn!(hash, "could not find torrent in Transmission");
            return Ok(());
        };
        let mut labels: BTreeSet<String> = torrent
            .labels
            .iter()
            .filter(|l| !l.eq_ignore_ascii_case(&s.category))
            .cloned()
            .collect();
        labels.insert(imported.to_string());
        if let Err(e) = self.set_labels(&hash, &labels).await {
            tracing::warn!(label = imported, title = %item.title, error = %e, "failed to set post-import torrent label");
        }
        Ok(())
    }

    pub async fn status(&self) -> ClientResult<DownloadClientInfo> {
        let s = self.settings;
        let dest_dir = if !s.directory.trim().is_empty() {
            s.directory.clone()
        } else {
            let config = self.config().await?;
            if s.category.trim().is_empty() {
                config.download_dir
            } else {
                format!("{}/{}", config.download_dir, s.category)
            }
        };
        Ok(DownloadClientInfo {
            is_localhost: is_localhost(&s.host),
            output_root_folders: vec![self.client.context.remap(&s.host, &dest_dir)],
            ..Default::default()
        })
    }

    pub async fn test(&self, failures: &mut Vec<ValidationFailure>) -> ClientResult<()> {
        failures.extend(self.test_connection().await);
        if crate::validation::has_errors(failures) {
            return Ok(());
        }
        failures.extend(self.test_get_torrents().await);
        Ok(())
    }

    async fn test_connection(&self) -> Option<ValidationFailure> {
        let app = self.settings.app.name;
        match self.client_version().await {
            Ok(raw) => {
                tracing::debug!(version = %raw, "Transmission version information");
                match version_number(&raw) {
                    Some(version) if version < Version::new(2, 40) => Some(ValidationFailure::error(
                        "",
                        &format!("{NAME} version should be at least 2.40. Version reported is {version}"),
                    )),
                    Some(_) => None,
                    None => Some(ValidationFailure::error(
                        "",
                        &format!(
                            "Unknown exception: Version string portion was too short or too long. ({raw})"
                        ),
                    )),
                }
            }
            Err(ClientError::Authentication(e)) => {
                tracing::error!(error = %e, "Transmission authentication failed");
                Some(
                    ValidationFailure::error("Username", "Authentication Failure").with_detail(&format!(
                        "Please verify your credentials. Also verify if the host running {app} isn't blocked from accessing {NAME} by WhiteList limitations in the {NAME} configuration."
                    )),
                )
            }
            Err(ClientError::Unavailable(e)) => {
                tracing::error!(error = %e, "unable to connect to Transmission");
                Some(
                    ValidationFailure::error("Host", &format!("Unable to connect to {NAME}")).with_detail(&e),
                )
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to test Transmission");
                Some(ValidationFailure::error(
                    "",
                    &format!("Unknown exception: {}", e.message()),
                ))
            }
        }
    }

    async fn test_get_torrents(&self) -> Option<ValidationFailure> {
        match self.torrents(None).await {
            Ok(_) => None,
            Err(e) => {
                tracing::error!(error = %e, "failed to get torrents");
                Some(ValidationFailure::error(
                    "",
                    &format!("Failed to get the list of torrents: {}", e.message()),
                ))
            }
        }
    }
}

/// `HasReachedSeedLimit`: per-torrent limits (mode 1) or the global ones
/// (mode 0); the global idle limit counts once the torrent stopped.
fn has_reached_seed_limit(t: &Torrent, ratio: f64, config: Option<&Config>) -> bool {
    let stopped = t.status == STATUS_STOPPED;
    let seeding = t.status == STATUS_SEEDING;
    if t.seed_ratio_mode == 1 {
        if stopped && ratio >= t.seed_ratio_limit {
            return true;
        }
    } else if t.seed_ratio_mode == 0 {
        if let Some(c) = config {
            if stopped && c.seed_ratio_limited && ratio >= c.seed_ratio_limit {
                return true;
            }
        }
    }
    if t.seed_idle_mode == 1 {
        if (stopped || seeding) && t.seconds_seeding > t.seed_idle_limit * 60 {
            return true;
        }
    } else if t.seed_idle_mode == 0 {
        if let Some(c) = config {
            if stopped && c.idle_seeding_limit_enabled {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_number() {
        assert_eq!(
            version_number("4.0.6 (38c164933e)"),
            Some(Version::with_build(4, 0, 6))
        );
        assert_eq!(
            version_number("2.94 (d8e60ee44f)"),
            Some(Version::parse("2.94").unwrap())
        );
        assert_eq!(
            version_number("Transmission 3.00"),
            Some(Version::parse("3.00").unwrap())
        );
        assert_eq!(version_number("(abc)"), None);
    }

    #[test]
    fn seed_limits() {
        let t = Torrent {
            status: STATUS_STOPPED,
            seed_ratio_mode: 1,
            seed_ratio_limit: 1.0,
            ..Default::default()
        };
        assert!(has_reached_seed_limit(&t, 1.2, None));
        assert!(!has_reached_seed_limit(&t, 0.5, None));
        let global = Config {
            seed_ratio_limited: true,
            seed_ratio_limit: 2.0,
            ..Default::default()
        };
        let t = Torrent {
            status: STATUS_STOPPED,
            ..Default::default()
        };
        assert!(has_reached_seed_limit(&t, 2.0, Some(&global)));
        assert!(!has_reached_seed_limit(&t, 2.0, None));
    }
}
