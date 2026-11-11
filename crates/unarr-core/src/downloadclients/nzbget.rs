//! NZBGet, after the originals' `Nzbget` and `NzbgetProxy`: JSON-RPC over
//! `jsonrpc` with basic authentication; downloads are tagged with a `drone`
//! parameter so their ids survive NZBGet renumbering them.

use std::collections::BTreeMap;

use base64::Engine;
use chrono::Duration;
use serde::Deserialize;
use serde_json::{json, Value};

use super::settings::NzbgetSettings;
use super::version::Version;
use super::GrabRequest;
use super::{
    build_base_url, combine_path, download, http_error_message, is_localhost, remote_path, transport_message,
    Client, ClientError, ClientResult, DownloadClientInfo, DownloadClientItem, DownloadItemStatus,
};
use crate::validation::ValidationFailure;

const NAME: &str = "NZBGet";
const SUCCESS_STATUS: [&str; 2] = ["SUCCESS", "NONE"];
const DELETE_FAILED_STATUS: [&str; 5] = ["HEALTH", "DUPE", "SCAN", "COPY", "BAD"];

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct NzbgetParameter {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct NzbgetQueueItem {
    /// NZBGet writes `NZBID` and `NZBName`; the originals' JSON reader is
    /// case-insensitive, serde is not.
    #[serde(rename = "NZBID", alias = "NzbId")]
    pub nzb_id: i64,
    #[serde(rename = "NZBName", alias = "NzbName")]
    pub nzb_name: String,
    pub category: String,
    pub file_size_lo: u32,
    pub file_size_hi: u32,
    pub remaining_size_lo: u32,
    pub remaining_size_hi: u32,
    pub paused_size_lo: u32,
    pub paused_size_hi: u32,
    pub active_downloads: i64,
    pub parameters: Vec<NzbgetParameter>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct NzbgetHistoryItem {
    #[serde(rename = "ID", alias = "Id")]
    pub id: i64,
    pub name: String,
    pub category: String,
    pub file_size_lo: u32,
    pub file_size_hi: u32,
    pub par_status: String,
    pub unpack_status: String,
    pub move_status: String,
    pub script_status: String,
    pub delete_status: String,
    pub mark_status: String,
    pub dest_dir: String,
    pub final_dir: String,
    pub parameters: Vec<NzbgetParameter>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct NzbgetGlobalStatus {
    pub download_rate: i64,
    pub download_paused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NzbgetCategory {
    pub name: String,
    pub dest_dir: String,
}

fn make_i64(hi: u32, lo: u32) -> i64 {
    ((hi as i64) << 32) | lo as i64
}

fn drone_id(parameters: &[NzbgetParameter]) -> Option<String> {
    parameters
        .iter()
        .find(|p| p.name == "drone")
        .map(|p| match &p.value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
}

pub struct Nzbget<'a> {
    client: &'a Client<'a>,
    settings: &'a NzbgetSettings,
}

impl<'a> Nzbget<'a> {
    pub fn new(client: &'a Client<'a>, settings: &'a NzbgetSettings) -> Nzbget<'a> {
        Nzbget { client, settings }
    }

    pub fn base_url(&self, relative: &str) -> String {
        let s = self.settings;
        combine_path(&build_base_url(s.use_ssl, &s.host, s.port, &s.url_base), relative)
    }

    // --- proxy

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> ClientResult<T> {
        let url = self.base_url("jsonrpc");
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params, "id": 1});
        let mut request = self
            .client
            .context
            .http
            .post(&url)
            .header(reqwest::header::ACCEPT, "application/json-rpc, application/json")
            .json(&body);
        let s = self.settings;
        if !s.username.is_empty() || !s.password.is_empty() {
            request = request.basic_auth(&s.username, Some(&s.password));
        }
        // Sonarr's proxy wraps a connection failure; Radarr's catches the
        // older exception type only, so the runtime's own message gets out.
        let unavailable = |e: &reqwest::Error| {
            if s.app == super::RADARR {
                ClientError::Unavailable(transport_message(e))
            } else {
                ClientError::Unavailable(format!("Unable to connect to NzbGet. {}", transport_message(e)))
            }
        };
        let response = request.send().await.map_err(|e| unavailable(&e))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClientError::Authentication(
                "Authentication failed for NzbGet, please check your settings".into(),
            ));
        }
        if !status.is_success() {
            return Err(ClientError::Other(format!(
                "Unable to connect to NzbGet. {}",
                http_error_message(status, "POST", &url)
            )));
        }
        let content = super::read_text(response, NAME, unavailable).await?;
        let v: Value = serde_json::from_str(&content)
            .map_err(|e| ClientError::Other(format!("Unexpected response from nzbget: {e}")))?;
        if !v["error"].is_null() {
            let error = &v["error"];
            let text = match error {
                Value::Object(_) => format!(
                    "{}: {}",
                    error["code"].as_i64().unwrap_or(0),
                    error["message"].as_str().unwrap_or("")
                ),
                other => other.to_string(),
            };
            return Err(ClientError::Other(format!(
                "Error response received from nzbget: {text}"
            )));
        }
        serde_json::from_value(v["result"].clone())
            .map_err(|e| ClientError::Other(format!("Unexpected result from nzbget {method}: {e}")))
    }

    pub async fn version(&self) -> ClientResult<String> {
        self.call("version", vec![]).await
    }

    pub async fn config(&self) -> ClientResult<BTreeMap<String, String>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Item {
            name: String,
            #[serde(default)]
            value: String,
        }
        let items: Vec<Item> = self.call("config", vec![]).await?;
        Ok(items.into_iter().map(|i| (i.name, i.value)).collect())
    }

    pub async fn global_status(&self) -> ClientResult<NzbgetGlobalStatus> {
        self.call("status", vec![]).await
    }

    pub async fn queue(&self) -> ClientResult<Vec<NzbgetQueueItem>> {
        self.call("listgroups", vec![]).await
    }

    pub async fn history(&self) -> ClientResult<Vec<NzbgetHistoryItem>> {
        self.call("history", vec![]).await
    }

    async fn edit_queue(&self, command: &str, id: i64) -> ClientResult<bool> {
        self.call("editqueue", vec![json!(command), json!(0), json!(""), json!(id)])
            .await
    }

    /// `DownloadNzb` for NZBGet 16+: `append` with a `drone` parameter, the
    /// drone id being what the download is known by from then on.
    pub async fn download_nzb(
        &self,
        nzb: &[u8],
        title: &str,
        category: &str,
        priority: i64,
        add_paused: bool,
    ) -> ClientResult<Option<String>> {
        let version = self.version().await?;
        let major: i64 = version
            .split(['.', '-'])
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if major < 16 {
            return Err(ClientError::Other(format!(
                "NZBGet {version} is not supported; version 16 or newer is required"
            )));
        }
        let drone = random_hex();
        let content = base64::engine::general_purpose::STANDARD.encode(nzb);
        let id: i64 = self
            .call(
                "append",
                vec![
                    json!(title),
                    json!(content),
                    json!(category),
                    json!(priority),
                    json!(false),
                    json!(add_paused),
                    json!(""),
                    json!(0),
                    json!("all"),
                    json!(["drone", drone]),
                ],
            )
            .await?;
        Ok((id > 0).then_some(drone))
    }

    /// `RemoveItem`: by drone id, or by NZBGet's own id for downloads
    /// something else added.
    pub async fn remove_item(&self, id: &str) -> ClientResult<()> {
        let queue = self.queue().await?;
        let history = self.history().await?;
        let (queue_item, history_item) = match id.parse::<i64>().ok().filter(|_| id.len() < 10) {
            Some(nzb_id) => (
                queue.iter().find(|q| q.nzb_id == nzb_id),
                history.iter().find(|h| h.id == nzb_id),
            ),
            None => (
                queue
                    .iter()
                    .find(|q| drone_id(&q.parameters).as_deref() == Some(id)),
                history
                    .iter()
                    .find(|h| drone_id(&h.parameters).as_deref() == Some(id)),
            ),
        };
        if let Some(q) = queue_item {
            if !self.edit_queue("GroupFinalDelete", q.nzb_id).await? {
                tracing::warn!(title = %q.nzb_name, id = q.nzb_id, "failed to remove item from nzbget queue");
            }
        } else if let Some(h) = history_item {
            if !self.edit_queue("HistoryDelete", h.id).await? {
                tracing::warn!(title = %h.name, id = h.id, "failed to remove item from nzbget history");
            }
        } else {
            tracing::warn!(id, "unable to remove item from nzbget: unknown id");
        }
        Ok(())
    }

    /// `RetryDownload`: back from history to the queue.
    pub async fn retry_download(&self, id: &str) -> ClientResult<()> {
        let history = self.history().await?;
        match history
            .iter()
            .find(|h| drone_id(&h.parameters).as_deref() == Some(id))
        {
            Some(h) => {
                if !self.edit_queue("HistoryRedownload", h.id).await? {
                    tracing::warn!(title = %h.name, id = h.id, "failed to return item to queue from history");
                }
            }
            None => tracing::warn!(id, "unable to return item to queue: unknown id"),
        }
        Ok(())
    }

    // --- client

    pub async fn add_nzb(
        &self,
        request: &GrabRequest<'_>,
        filename: &str,
        nzb: Vec<u8>,
    ) -> ClientResult<String> {
        let priority = self.settings.priority(request.recent);
        self.download_nzb(
            &nzb,
            filename,
            &self.settings.category,
            priority,
            self.settings.add_paused,
        )
        .await?
        .ok_or_else(|| ClientError::Rejected(format!("{NAME} rejected the NZB for an unknown reason")))
    }

    async fn queue_items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let global = self.global_status().await?;
        let queue = self.queue().await?;
        let mut out = Vec::new();
        let mut total_remaining: i64 = 0;
        for q in queue {
            let total = make_i64(q.file_size_hi, q.file_size_lo);
            let paused = make_i64(q.paused_size_hi, q.paused_size_lo);
            let remaining = make_i64(q.remaining_size_hi, q.remaining_size_lo);
            let id = drone_id(&q.parameters).unwrap_or_else(|| q.nzb_id.to_string());
            let mut item =
                DownloadClientItem::new(self.client.definition.item_client_info(false), id, q.nzb_name);
            item.total_size = total;
            item.category = Some(q.category);
            item.can_move_files = true;
            item.can_be_removed = true;
            if global.download_paused || (remaining == paused && remaining != 0) {
                item.status = DownloadItemStatus::Paused;
                item.remaining_size = remaining;
            } else {
                item.status = if q.active_downloads == 0 && remaining != 0 {
                    DownloadItemStatus::Queued
                } else {
                    DownloadItemStatus::Downloading
                };
                item.remaining_size = remaining - paused;
                if global.download_rate != 0 {
                    // `i64::MIN / -1` would overflow; a rate below zero is garbage anyway.
                    let seconds = (total_remaining.saturating_add(item.remaining_size))
                        .checked_div(global.download_rate)
                        .unwrap_or(0);
                    item.remaining_time = Some(super::time_left(seconds));
                    total_remaining = total_remaining.saturating_add(item.remaining_size);
                }
            }
            out.push(item);
        }
        Ok(out)
    }

    async fn history_items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let history = self.history().await?;
        let mut out = Vec::new();
        for h in history.into_iter().take(self.client.context.history_limit) {
            let dir = if h.final_dir.trim().is_empty() {
                &h.dest_dir
            } else {
                &h.final_dir
            };
            let id = drone_id(&h.parameters).unwrap_or_else(|| h.id.to_string());
            let mut item =
                DownloadClientItem::new(self.client.definition.item_client_info(false), id, h.name.clone());
            item.total_size = make_i64(h.file_size_hi, h.file_size_lo);
            item.output_path = Some(self.client.context.remap(&self.settings.host, dir));
            item.category = Some(h.category.clone());
            item.message = Some(format!(
                "PAR Status: {} - Unpack Status: {} - Move Status: {} - Script Status: {} - Delete Status: {} - Mark Status: {}",
                h.par_status, h.unpack_status, h.move_status, h.script_status, h.delete_status, h.mark_status
            ));
            item.status = DownloadItemStatus::Completed;
            item.remaining_time = Some(Duration::zero());
            item.can_move_files = true;
            item.can_be_removed = true;
            if h.delete_status == "MANUAL" {
                if h.mark_status == "BAD" {
                    item.status = DownloadItemStatus::Failed;
                    out.push(item);
                }
                continue;
            }
            let success = |s: &str| SUCCESS_STATUS.contains(&s);
            if !success(&h.par_status) {
                item.status = DownloadItemStatus::Failed;
            }
            if h.unpack_status == "SPACE" {
                item.status = DownloadItemStatus::Warning;
            } else if !success(&h.unpack_status) {
                item.status = DownloadItemStatus::Failed;
            }
            if !success(&h.move_status) {
                item.status = DownloadItemStatus::Warning;
            }
            if !success(&h.script_status) {
                item.status = DownloadItemStatus::Failed;
            }
            if !success(&h.delete_status) && !h.delete_status.trim().is_empty() {
                item.status = if DELETE_FAILED_STATUS.contains(&h.delete_status.as_str()) {
                    DownloadItemStatus::Failed
                } else {
                    DownloadItemStatus::Warning
                };
            }
            out.push(item);
        }
        Ok(out)
    }

    pub async fn items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let mut items = self.queue_items().await?;
        items.extend(self.history_items().await?);
        Ok(items
            .into_iter()
            .filter(|i| i.category.as_deref() == Some(self.settings.category.as_str()))
            .collect())
    }

    pub async fn remove(&self, item: &DownloadClientItem, delete_data: bool) -> ClientResult<()> {
        if delete_data {
            download::delete_item_data(item, &self.client.context.root_folders);
        }
        self.remove_item(&item.download_id).await
    }

    /// `GetCategories`: `Category{i}.Name` with its destination.
    pub fn categories(config: &BTreeMap<String, String>) -> Vec<NzbgetCategory> {
        let get = |key: &str| config.get(key).cloned();
        let mut out = Vec::new();
        for i in 1..100 {
            let Some(name) = get(&format!("Category{i}.Name")) else {
                break;
            };
            let mut dest_dir = get(&format!("Category{i}.DestDir")).unwrap_or_default();
            if dest_dir.trim().is_empty() {
                let main_dir = get("MainDir").unwrap_or_default();
                dest_dir = get("DestDir")
                    .unwrap_or_default()
                    .replace("${MainDir}", &main_dir);
                if get("AppendCategoryDir").unwrap_or_else(|| "yes".into()) == "yes" {
                    dest_dir = remote_path::join(&dest_dir, &name);
                }
            }
            out.push(NzbgetCategory { name, dest_dir });
        }
        out
    }

    pub async fn status(&self) -> ClientResult<DownloadClientInfo> {
        let config = self.config().await?;
        let category = Self::categories(&config)
            .into_iter()
            .find(|c| c.name == self.settings.category);
        Ok(DownloadClientInfo {
            is_localhost: is_localhost(&self.settings.host),
            output_root_folders: category
                .map(|c| vec![self.client.context.remap(&self.settings.host, &c.dest_dir)])
                .unwrap_or_default(),
            ..Default::default()
        })
    }

    pub async fn test(&self, failures: &mut Vec<ValidationFailure>) -> ClientResult<()> {
        failures.extend(self.test_connection().await);
        failures.extend(self.test_category().await?);
        failures.extend(self.test_settings().await?);
        Ok(())
    }

    async fn test_connection(&self) -> Option<ValidationFailure> {
        let outcome = async {
            let raw = self.version().await?;
            let version = raw.split('-').next().unwrap_or("").to_string();
            let parsed = Version::parse(&version).ok_or_else(|| {
                ClientError::Other(format!(
                    "Version string portion was too short or too long: {version}"
                ))
            })?;
            Ok::<_, ClientError>((version, parsed))
        }
        .await;
        match outcome {
            Ok((version, parsed)) => (parsed < Version::new(12, 0)).then(|| {
                ValidationFailure::error(
                    "",
                    &format!("{NAME} version should be at least 12.0. Version reported is {version}"),
                )
            }),
            Err(e) => {
                if e.message().to_ascii_lowercase().contains("authentication failed") {
                    Some(ValidationFailure::error("Username", "Authentication Failure"))
                } else {
                    tracing::error!(error = %e, "unable to connect to NZBGet");
                    Some(ValidationFailure::error(
                        "Host",
                        &format!("Unable to connect to {NAME}"),
                    ))
                }
            }
        }
    }

    async fn test_category(&self) -> ClientResult<Option<ValidationFailure>> {
        let config = self.config().await?;
        let tv = self.settings.category.trim();
        if !tv.is_empty() && !Self::categories(&config).iter().any(|c| c.name == tv) {
            let mut f =
                ValidationFailure::error(self.settings.app.category_property, "Category does not exist")
                    .with_detail(&format!(
                        "The category you entered doesn't exist in {NAME}. Create it in {NAME} first."
                    ));
            f.info_link = Some(self.base_url(""));
            return Ok(Some(f));
        }
        Ok(None)
    }

    async fn test_settings(&self) -> ClientResult<Option<ValidationFailure>> {
        let app = self.settings.app.name;
        let config = self.config().await?;
        let keep = config.get("KeepHistory").cloned().unwrap_or_else(|| "7".into());
        let value: Option<i64> = keep
            .chars()
            .all(|c| c.is_ascii_digit())
            .then(|| keep.parse().ok())
            .flatten();
        let failure = match value {
            None | Some(0) => Some(
                ValidationFailure::error("", "NzbGet setting KeepHistory should be greater than 0").with_detail(
                    &format!("NzbGet setting KeepHistory is set to 0. Which prevents {app} from seeing completed downloads."),
                ),
            ),
            Some(v) if v > 25000 => Some(
                ValidationFailure::error("", "NzbGet setting KeepHistory should be less than 25000")
                    .with_detail("NzbGet setting KeepHistory is set too high."),
            ),
            _ => None,
        };
        Ok(failure.map(|mut f| {
            f.info_link = Some(self.base_url(""));
            f
        }))
    }
}

/// `Guid.NewGuid().ToString().Replace("-", "")`: 32 hex characters, or
/// the next pinned id when a replay set some.
fn random_hex() -> String {
    if let Some(pinned) = PINNED_IDS.lock().unwrap().pop_front() {
        return pinned;
    }
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

static PINNED_IDS: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

/// Pin the drone ids the next grabs get, in order, so that a replay of a
/// recording sees the ids the recording did (like its pinned clock).
pub fn pin_drone_ids(ids: &[&str]) {
    let mut pinned = PINNED_IDS.lock().unwrap();
    pinned.clear();
    pinned.extend(ids.iter().map(|s| s.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `XmlRpc.cpp` writes `NZBID`, `NZBName` and `ID`; a JSON reader that
    /// matched case-insensitively would not notice, serde does.
    #[test]
    fn reads_nzbgets_own_field_names() {
        let group: NzbgetQueueItem = serde_json::from_str(
            r#"{"NZBID": 42, "NZBName": "Show.S01E02.720p-GRP", "Category": "tv", "FileSizeLo": 10,
                "FileSizeHi": 0, "RemainingSizeLo": 5, "RemainingSizeHi": 0, "PausedSizeLo": 0,
                "PausedSizeHi": 0, "ActiveDownloads": 1, "Parameters": [{"Name": "drone", "Value": "abc"}]}"#,
        )
        .unwrap();
        assert_eq!(
            (group.nzb_id, group.nzb_name.as_str()),
            (42, "Show.S01E02.720p-GRP")
        );
        assert_eq!(drone_id(&group.parameters).as_deref(), Some("abc"));
        let item: NzbgetHistoryItem = serde_json::from_str(
            r#"{"ID": 41, "Name": "Other.S02E05-XYZ", "Category": "tv", "ParStatus": "SUCCESS",
                "UnpackStatus": "SUCCESS", "MoveStatus": "SUCCESS", "ScriptStatus": "NONE",
                "DeleteStatus": "NONE", "MarkStatus": "NONE", "DestDir": "/downloads/tv/Other",
                "FinalDir": "", "Parameters": []}"#,
        )
        .unwrap();
        assert_eq!((item.id, item.name.as_str()), (41, "Other.S02E05-XYZ"));
    }

    #[test]
    fn categories_follow_the_main_dir() {
        let config: BTreeMap<String, String> = [
            ("MainDir", "/downloads"),
            ("DestDir", "${MainDir}/completed"),
            ("AppendCategoryDir", "yes"),
            ("Category1.Name", "tv"),
            ("Category1.DestDir", ""),
            ("Category2.Name", "movies"),
            ("Category2.DestDir", "/movies"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let cats = Nzbget::categories(&config);
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].dest_dir, "/downloads/completed/tv");
        assert_eq!(cats[1].dest_dir, "/movies");
        assert_eq!(make_i64(1, 2), (1 << 32) + 2);
        assert_eq!(random_hex().len(), 32);
    }
}
