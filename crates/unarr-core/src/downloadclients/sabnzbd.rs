//! SABnzbd, after the originals' `Sabnzbd` and `SabnzbdProxy`: the
//! `api?mode=...` endpoint with an API key (or the `ma_*` credentials),
//! JSON answers, errors as `{status: false, error}` or plain text.

use chrono::Duration;
use serde_json::Value;

use super::settings::SabnzbdSettings;
use super::version::Version;
use super::{
    build_base_url, combine_path, download, flag, http_error_message, int, is_localhost, remote_path, text,
    transport_message, Client, ClientError, ClientResult, DownloadClientInfo, DownloadClientItem,
    DownloadItemStatus, APP_NAME,
};
use crate::decision::RemoteEpisode;
use crate::validation::ValidationFailure;

const NAME: &str = "SABnzbd";

/// `SabnzbdDownloadStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SabStatus {
    Grabbing,
    Queued,
    Paused,
    Checking,
    Downloading,
    QuickCheck,
    Verifying,
    Repairing,
    Fetching,
    Extracting,
    Moving,
    Running,
    Completed,
    Failed,
    Deleted,
    Propagating,
}

impl SabStatus {
    fn parse(s: &str) -> Option<SabStatus> {
        Some(match s.to_ascii_lowercase().as_str() {
            "grabbing" => SabStatus::Grabbing,
            "queued" => SabStatus::Queued,
            "paused" => SabStatus::Paused,
            "checking" => SabStatus::Checking,
            "downloading" => SabStatus::Downloading,
            "quickcheck" => SabStatus::QuickCheck,
            "verifying" => SabStatus::Verifying,
            "repairing" => SabStatus::Repairing,
            "fetching" => SabStatus::Fetching,
            "extracting" => SabStatus::Extracting,
            "moving" => SabStatus::Moving,
            "running" => SabStatus::Running,
            "completed" => SabStatus::Completed,
            "failed" => SabStatus::Failed,
            "deleted" => SabStatus::Deleted,
            "propagating" => SabStatus::Propagating,
            _ => return None,
        })
    }
}

/// `SabnzbdPriority.Force`, the one priority a paused queue still serves.
const PRIORITY_FORCE: i64 = 2;

#[derive(Debug, Clone, Default)]
pub struct SabConfig {
    pub complete_dir: String,
    pub tv_categories: Vec<String>,
    pub enable_tv_sorting: bool,
    pub movie_categories: Vec<String>,
    pub enable_movie_sorting: bool,
    pub date_categories: Vec<String>,
    pub enable_date_sorting: bool,
    pub pre_check: bool,
    pub history_retention: String,
    pub history_retention_option: String,
    pub history_retention_number: i64,
    pub categories: Vec<SabCategory>,
    /// (`is_active`, `sort_cats`) of the SABnzbd 4.1+ sorters.
    pub sorters: Vec<(bool, Vec<String>)>,
}

#[derive(Debug, Clone, Default)]
pub struct SabCategory {
    pub name: String,
    pub dir: String,
    pub priority: i64,
    /// Filled by `categories()`: the complete dir plus `dir`.
    pub full_path: String,
}

#[derive(Debug, Clone, Default)]
pub struct SabQueue {
    pub default_root_folder: String,
    pub paused: bool,
    pub items: Vec<SabQueueItem>,
}

#[derive(Debug, Clone)]
pub struct SabQueueItem {
    pub status: SabStatus,
    pub time_left: Duration,
    pub size_mb: f64,
    pub title: String,
    pub priority: i64,
    pub category: String,
    pub size_left_mb: f64,
    pub id: String,
}

#[derive(Debug, Clone, Default)]
pub struct SabHistory {
    pub items: Vec<SabHistoryItem>,
}

#[derive(Debug, Clone)]
pub struct SabHistoryItem {
    pub fail_message: String,
    pub size: i64,
    pub category: String,
    pub storage: String,
    pub status: SabStatus,
    pub id: String,
    pub title: String,
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(|s| s.as_str().map(String::from)).collect(),
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn decimal(v: &Value, key: &str) -> f64 {
    match &v[key] {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `TimeSpan.Parse` of SABnzbd's `h:mm:ss` or `d:hh:mm:ss`.
fn parse_time_left(s: &str) -> Duration {
    let parts: Vec<i64> = s.split(':').filter_map(|p| p.trim().parse().ok()).collect();
    let seconds = match parts.as_slice() {
        [h, m, s] => h.saturating_mul(3600).saturating_add(m * 60 + s),
        [d, h, m, s] => d
            .saturating_mul(86400)
            .saturating_add(h.saturating_mul(3600))
            .saturating_add(m * 60 + s),
        [m, s] => m.saturating_mul(60).saturating_add(*s),
        _ => 0,
    };
    super::time_left(seconds)
}

fn parse_priority(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or(-100),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "paused" => -2,
            "low" => -1,
            "normal" => 0,
            "high" => 1,
            "force" => 2,
            "default" => -100,
            other => other.parse().unwrap_or(-100),
        },
        _ => -100,
    }
}

impl SabConfig {
    fn from_value(v: &Value) -> SabConfig {
        let misc = &v["misc"];
        SabConfig {
            complete_dir: text(misc, "complete_dir"),
            tv_categories: strings(&misc["tv_categories"]),
            enable_tv_sorting: flag(misc, "enable_tv_sorting", false),
            movie_categories: strings(&misc["movie_categories"]),
            enable_movie_sorting: flag(misc, "enable_movie_sorting", false),
            date_categories: strings(&misc["date_categories"]),
            enable_date_sorting: flag(misc, "enable_date_sorting", false),
            pre_check: flag(misc, "pre_check", false),
            history_retention: text(misc, "history_retention"),
            history_retention_option: text(misc, "history_retention_option"),
            history_retention_number: int(misc, "history_retention_number", 0),
            categories: v["categories"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|c| SabCategory {
                            name: text(c, "name"),
                            dir: text(c, "dir"),
                            priority: int(c, "priority", 0),
                            full_path: String::new(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            sorters: v["sorters"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|s| (flag(s, "is_active", false), strings(&s["sort_cats"])))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

pub struct Sabnzbd<'a> {
    client: &'a Client<'a>,
    settings: &'a SabnzbdSettings,
}

impl<'a> Sabnzbd<'a> {
    pub fn new(client: &'a Client<'a>, settings: &'a SabnzbdSettings) -> Sabnzbd<'a> {
        Sabnzbd { client, settings }
    }

    /// `GetBaseUrl`: the client's URL, with `relative` appended.
    pub fn base_url(&self, relative: &str) -> String {
        let s = self.settings;
        combine_path(&build_base_url(s.use_ssl, &s.host, s.port, &s.url_base), relative)
    }

    // --- proxy

    async fn request(
        &self,
        mode: &str,
        params: &[(&str, String)],
        upload: Option<(String, Vec<u8>)>,
    ) -> ClientResult<String> {
        let s = self.settings;
        let mut query: Vec<(String, String)> = vec![("mode".into(), mode.into())];
        query.extend(params.iter().map(|(k, v)| (k.to_string(), v.clone())));
        if !s.api_key.trim().is_empty() {
            query.push(("apikey".into(), s.api_key.clone()));
        } else {
            query.push(("ma_username".into(), s.username.clone()));
            query.push(("ma_password".into(), s.password.clone()));
        }
        query.push(("output".into(), "json".into()));
        let url = self.base_url("api");
        let http = &self.client.context.http;
        let (method, builder) = match upload {
            Some((filename, data)) => {
                let part = reqwest::multipart::Part::bytes(data)
                    .file_name(filename)
                    .mime_str("application/x-nzb")
                    .map_err(|e| ClientError::Other(e.to_string()))?;
                let form = reqwest::multipart::Form::new().part("name", part);
                ("POST", http.post(&url).multipart(form))
            }
            None => ("GET", http.get(&url)),
        };
        let response = builder
            .query(&query)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| {
                ClientError::Unavailable(format!("Unable to connect to {NAME}, {}", transport_message(&e)))
            })?;
        let status = response.status();
        let final_url = response.url().to_string();
        let content = super::read_text(response, NAME, |e| {
            ClientError::Unavailable(format!("Unable to connect to {NAME}, {}", transport_message(e)))
        })
        .await?;
        if !status.is_success() {
            return Err(ClientError::Other(format!(
                "Unable to connect to {NAME}, {}",
                http_error_message(status, method, &final_url)
            )));
        }
        check_for_error(&content)?;
        Ok(content)
    }

    pub async fn version(&self) -> ClientResult<String> {
        let content = self.request("version", &[], None).await?;
        Ok(serde_json::from_str::<Value>(&content)
            .ok()
            .map(|v| text(&v, "version"))
            .unwrap_or_default())
    }

    pub async fn config(&self) -> ClientResult<SabConfig> {
        let content = self.request("get_config", &[], None).await?;
        let v: Value = serde_json::from_str(&content)
            .map_err(|e| ClientError::Other(format!("Unexpected {NAME} config response: {e}")))?;
        Ok(SabConfig::from_value(&v["config"]))
    }

    /// `fullstatus?skip_dashboard=1`: the complete dir SABnzbd resolved.
    pub async fn complete_dir(&self) -> ClientResult<String> {
        let content = self
            .request("fullstatus", &[("skip_dashboard", "1".into())], None)
            .await?;
        let v: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
        Ok(text(&v["status"], "completedir"))
    }

    pub async fn queue(&self, start: i64, limit: i64) -> ClientResult<SabQueue> {
        let mut params = vec![("start", start.to_string()), ("limit", limit.to_string())];
        if !self.settings.tv_category.trim().is_empty() {
            params.push(("category", self.settings.tv_category.clone()));
        }
        let content = self.request("queue", &params, None).await?;
        let v: Value = serde_json::from_str(&content)
            .map_err(|e| ClientError::Other(format!("Unexpected {NAME} queue response: {e}")))?;
        let q = &v["queue"];
        let items = q["slots"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| SabQueueItem {
                        status: SabStatus::parse(&text(s, "status")).unwrap_or(SabStatus::Downloading),
                        time_left: parse_time_left(&text(s, "timeleft")),
                        size_mb: decimal(s, "mb"),
                        title: text(s, "filename"),
                        priority: parse_priority(&s["priority"]),
                        category: text(s, "cat"),
                        size_left_mb: decimal(s, "mbleft"),
                        id: text(s, "nzo_id"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(SabQueue {
            default_root_folder: text(q, "my_home"),
            paused: flag(q, "paused", false),
            items,
        })
    }

    pub async fn history(&self, start: i64, limit: i64) -> ClientResult<SabHistory> {
        let mut params = vec![("start", start.to_string()), ("limit", limit.to_string())];
        if !self.settings.tv_category.trim().is_empty() {
            params.push(("category", self.settings.tv_category.clone()));
        }
        let content = self.request("history", &params, None).await?;
        let v: Value = serde_json::from_str(&content)
            .map_err(|e| ClientError::Other(format!("Unexpected {NAME} history response: {e}")))?;
        let items = v["history"]["slots"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| SabHistoryItem {
                        fail_message: text(s, "fail_message"),
                        size: int(s, "bytes", 0),
                        category: text(s, "category"),
                        storage: text(s, "storage"),
                        status: SabStatus::parse(&text(s, "status")).unwrap_or(SabStatus::Downloading),
                        id: text(s, "nzo_id"),
                        title: text(s, "name"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(SabHistory { items })
    }

    /// `addfile`: the nzo ids SABnzbd assigned (none when it answered
    /// something it does not call JSON).
    pub async fn add_nzb_file(
        &self,
        nzb: Vec<u8>,
        filename: &str,
        category: &str,
        priority: i64,
    ) -> ClientResult<Vec<String>> {
        let params = [("cat", category.to_string()), ("priority", priority.to_string())];
        let content = self
            .request("addfile", &params, Some((filename.to_string(), nzb)))
            .await?;
        Ok(serde_json::from_str::<Value>(&content)
            .ok()
            .map(|v| strings(&v["nzo_ids"]))
            .unwrap_or_default())
    }

    pub async fn remove_from_queue(&self, id: &str, delete_data: bool) -> ClientResult<()> {
        let params = [
            ("name", "delete".to_string()),
            ("del_files", if delete_data { "1" } else { "0" }.to_string()),
            ("value", id.to_string()),
        ];
        self.request("queue", &params, None).await.map(|_| ())
    }

    pub async fn remove_from_history(
        &self,
        id: &str,
        delete_data: bool,
        permanently: bool,
    ) -> ClientResult<()> {
        let params = [
            ("name", "delete".to_string()),
            ("del_files", if delete_data { "1" } else { "0" }.to_string()),
            ("value", id.to_string()),
            ("archive", if permanently { "0" } else { "1" }.to_string()),
        ];
        self.request("history", &params, None).await.map(|_| ())
    }

    // --- client

    pub async fn add_nzb(
        &self,
        remote: &RemoteEpisode,
        filename: &str,
        nzb: Vec<u8>,
    ) -> ClientResult<String> {
        let priority = self
            .settings
            .priority(remote.is_recent_episode(self.client.context.now));
        let ids = self
            .add_nzb_file(nzb, filename, &self.settings.tv_category, priority)
            .await?;
        ids.into_iter()
            .next()
            .ok_or_else(|| ClientError::Rejected(format!("{NAME} rejected the NZB for an unknown reason")))
    }

    async fn queue_items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let queue = self.queue(0, 0).await?;
        let mut out = Vec::new();
        for q in queue.items {
            if q.status == SabStatus::Deleted {
                continue;
            }
            let mut item =
                DownloadClientItem::new(self.client.definition.item_client_info(false), q.id, q.title);
            item.category = Some(q.category);
            item.total_size = (q.size_mb * 1024.0 * 1024.0) as i64;
            item.remaining_size = (q.size_left_mb * 1024.0 * 1024.0) as i64;
            item.remaining_time = Some(q.time_left);
            item.can_be_removed = true;
            item.can_move_files = true;
            if (queue.paused && q.priority != PRIORITY_FORCE) || q.status == SabStatus::Paused {
                item.status = DownloadItemStatus::Paused;
                item.remaining_time = None;
            } else if matches!(
                q.status,
                SabStatus::Queued | SabStatus::Grabbing | SabStatus::Propagating
            ) {
                item.status = DownloadItemStatus::Queued;
            } else {
                item.status = DownloadItemStatus::Downloading;
            }
            if let Some(rest) = item.title.strip_prefix("ENCRYPTED /") {
                item.title = rest.to_string();
                item.is_encrypted = true;
            }
            out.push(item);
        }
        Ok(out)
    }

    async fn history_items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let history = self.history(0, self.client.context.history_limit as i64).await?;
        let mut out = Vec::new();
        for h in history.items {
            if h.status == SabStatus::Deleted {
                continue;
            }
            let mut item = DownloadClientItem::new(
                self.client.definition.item_client_info(false),
                h.id,
                h.title.clone(),
            );
            item.category = Some(h.category);
            item.total_size = h.size;
            item.remaining_size = 0;
            item.remaining_time = Some(Duration::zero());
            item.message = Some(h.fail_message.clone()).filter(|m| !m.is_empty());
            item.can_be_removed = true;
            item.can_move_files = true;
            item.status = match h.status {
                SabStatus::Failed => {
                    if h.fail_message
                        .eq_ignore_ascii_case("Unpacking failed, write error or disk is full?")
                    {
                        DownloadItemStatus::Warning
                    } else {
                        DownloadItemStatus::Failed
                    }
                }
                SabStatus::Completed => DownloadItemStatus::Completed,
                // Verifying, moving...
                _ => DownloadItemStatus::Downloading,
            };
            let output = self.client.context.remap(&self.settings.host, &h.storage);
            if !output.is_empty() {
                // The job folder: the nearest parent named like the title.
                let mut chosen = output.clone();
                let mut parent = remote_path::parent(&output);
                while let Some(p) = parent {
                    if remote_path::file_name(&p) == h.title {
                        chosen = p.clone();
                    }
                    parent = remote_path::parent(&p);
                }
                item.output_path = Some(chosen);
            }
            out.push(item);
        }
        Ok(out)
    }

    pub async fn items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let mut items = self.queue_items().await?;
        items.extend(self.history_items().await?);
        let category = self.settings.tv_category.trim();
        Ok(items
            .into_iter()
            .filter(|i| {
                let c = i.category.as_deref().unwrap_or("");
                c == category || (c == "*" && category.is_empty())
            })
            .collect())
    }

    pub async fn remove(&self, item: &DownloadClientItem, delete_data: bool) -> ClientResult<()> {
        let in_queue = self
            .queue_items()
            .await?
            .iter()
            .any(|q| q.download_id == item.download_id);
        if in_queue {
            self.remove_from_queue(&item.download_id, delete_data).await
        } else {
            if delete_data && item.status == DownloadItemStatus::Completed {
                download::delete_item_data(item, &self.client.context.root_folders);
            }
            self.remove_from_history(
                &item.download_id,
                delete_data,
                item.status == DownloadItemStatus::Failed,
            )
            .await
        }
    }

    /// `GetCategories`: every category with its full path.
    pub async fn categories(&self, config: &SabConfig) -> ClientResult<Vec<SabCategory>> {
        let mut complete_dir = config.complete_dir.clone();
        if !remote_path::is_rooted(&complete_dir) {
            if self.has_version(2, 0).await? {
                complete_dir = self.complete_dir().await?;
            } else {
                let queue = self.queue(0, 1).await?;
                complete_dir = remote_path::join(&queue.default_root_folder, &complete_dir);
            }
        }
        Ok(config
            .categories
            .iter()
            .map(|c| SabCategory {
                full_path: remote_path::join(&complete_dir, c.dir.trim_end_matches('*')),
                ..c.clone()
            })
            .collect())
    }

    async fn has_version(&self, major: i64, minor: i64) -> ClientResult<bool> {
        let raw = self.version().await?;
        Ok(parse_version(&raw).is_some_and(|v| v >= Version::with_build(major, minor, 0)))
    }

    pub async fn status(&self) -> ClientResult<DownloadClientInfo> {
        let config = self.config().await?;
        let categories = self.categories(&config).await?;
        let category = categories
            .iter()
            .find(|c| c.name == self.settings.tv_category)
            .or_else(|| categories.iter().find(|c| c.name == "*"));
        let mut status = DownloadClientInfo {
            is_localhost: is_localhost(&self.settings.host),
            ..Default::default()
        };
        if let Some(category) = category {
            let tv = &self.settings.tv_category;
            if config.enable_tv_sorting && contains_category(&config.tv_categories, tv) {
                status.sorting_mode = Some("TV".into());
            } else if config.enable_movie_sorting && contains_category(&config.movie_categories, tv) {
                status.sorting_mode = Some("Movie".into());
            } else if config.enable_date_sorting && contains_category(&config.date_categories, tv) {
                status.sorting_mode = Some("Date".into());
            }
            status.output_root_folders = vec![self
                .client
                .context
                .remap(&self.settings.host, &category.full_path)];
        }
        status.removes_completed_downloads = removes_completed_downloads(&config);
        Ok(status)
    }

    pub async fn test(&self, failures: &mut Vec<ValidationFailure>) -> ClientResult<()> {
        failures.extend(self.test_connection_and_version().await);
        failures.extend(self.test_authentication().await?);
        failures.extend(self.test_global_config().await?);
        failures.extend(self.test_category().await?);
        Ok(())
    }

    async fn test_connection_and_version(&self) -> Option<ValidationFailure> {
        let raw = match self.version().await {
            Ok(raw) => raw,
            Err(e) => {
                return Some(
                    ValidationFailure::error("Host", &format!("Unable to connect to {NAME}"))
                        .with_detail(e.message()),
                )
            }
        };
        let Some(version) = parse_version(&raw) else {
            return Some(ValidationFailure::error(
                "Version",
                &format!("Unknown Version: {raw}"),
            ));
        };
        if raw.eq_ignore_ascii_case("develop") {
            return Some(
                ValidationFailure::warning("Version", "Sabnzbd develop version, assuming version 3.0.0 or higher.")
                    .with_detail(&format!(
                        "{APP_NAME} may not be able to support new features added to SABnzbd when running develop versions."
                    )),
            );
        }
        if version.major >= 1 || version.minor >= 7 {
            return None;
        }
        Some(ValidationFailure::error(
            "Version",
            &format!("{NAME} version should be at least 0.7.0. Version reported is {version}"),
        ))
    }

    async fn test_authentication(&self) -> ClientResult<Option<ValidationFailure>> {
        match self.config().await {
            Ok(_) => Ok(None),
            Err(e) => {
                let message = e.message().to_ascii_lowercase();
                if message.contains("api key incorrect") {
                    Ok(Some(ValidationFailure::error("APIKey", "API Key Incorrect")))
                } else if message.contains("api key required") {
                    Ok(Some(ValidationFailure::error("APIKey", "API Key Required")))
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn test_global_config(&self) -> ClientResult<Option<ValidationFailure>> {
        let config = self.config().await?;
        if config.pre_check && !self.has_version(1, 1).await? {
            let mut f = ValidationFailure::error("", "Disable 'Check before download' option in Sabnbzd").with_detail(
                &format!("Using 'Check before download' affects {APP_NAME} ability to track new downloads. Also Sabnzbd recommends 'Abort jobs that cannot be completed' instead since it's more effective."),
            );
            f.info_link = Some(self.base_url("config/switches/"));
            return Ok(Some(f));
        }
        Ok(None)
    }

    async fn test_category(&self) -> ClientResult<Option<ValidationFailure>> {
        let config = self.config().await?;
        let tv = self.settings.tv_category.clone();
        let categories = self.categories(&config).await?;
        let sorting = |message: &str, detail: &str| {
            let mut f = ValidationFailure::error("TvCategory", message).with_detail(detail);
            f.info_link = Some(self.base_url("config/sorting/"));
            Some(f)
        };
        match categories.iter().find(|c| c.name == tv) {
            Some(category) => {
                if category.dir.ends_with('*') {
                    let mut f = ValidationFailure::error("TvCategory", "Enable Job folders").with_detail(&format!(
                        "{APP_NAME} prefers each download to have a separate folder. With * appended to the Folder/Path Sabnzbd will not create these job folders. Go to Sabnzbd to fix it."
                    ));
                    f.info_link = Some(self.base_url("config/categories/"));
                    return Ok(Some(f));
                }
            }
            None => {
                if !tv.trim().is_empty() {
                    let mut f = ValidationFailure::error("TvCategory", "Category does not exist")
                        .with_detail(&format!(
                            "The category you entered doesn't exist in {NAME}. Create it in {NAME} first."
                        ));
                    f.info_link = Some(self.base_url("config/categories/"));
                    return Ok(Some(f));
                }
            }
        }
        if config
            .sorters
            .iter()
            .any(|(active, cats)| *active && contains_category(cats, &tv))
        {
            return Ok(sorting(
                "Disable TV Sorting",
                &format!("You must disable TV sorting for the category {APP_NAME} uses to prevent import issues. Go to Sabnzbd to fix it."),
            ));
        }
        if config.enable_tv_sorting && contains_category(&config.tv_categories, &tv) {
            return Ok(sorting(
                "Disable TV Sorting",
                &format!("You must disable TV sorting for the category {APP_NAME} uses to prevent import issues. Go to Sabnzbd to fix it."),
            ));
        }
        if config.enable_movie_sorting && contains_category(&config.movie_categories, &tv) {
            return Ok(sorting(
                "Disable Movie Sorting",
                &format!("You must disable Movie sorting for the category {APP_NAME} uses to prevent import issues. Go to Sabnzbd to fix it."),
            ));
        }
        if config.enable_date_sorting && contains_category(&config.date_categories, &tv) {
            return Ok(sorting(
                "Disable Date Sorting",
                &format!("You must disable Date sorting for the category {APP_NAME} uses to prevent import issues. Go to Sabnzbd to fix it."),
            ));
        }
        Ok(None)
    }
}

/// `CheckForError`: a JSON `{status: false, error}` or a plain `error: ...`.
fn check_for_error(content: &str) -> ClientResult<()> {
    let (failed, error) = match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(m)) => {
            let status = &m.get("status").cloned().unwrap_or(Value::Null);
            let failed = match status {
                Value::Bool(b) => !b,
                Value::String(s) => s.eq_ignore_ascii_case("false"),
                _ => false,
            };
            (failed, text(&Value::Object(m.clone()), "error"))
        }
        _ => {
            let failed = content.to_ascii_lowercase().starts_with("error");
            (failed, content.replace("error: ", ""))
        }
    };
    if failed {
        return Err(ClientError::Other(format!(
            "Error response received from {NAME}: {error}"
        )));
    }
    Ok(())
}

/// `ParseVersion`: `major.minor.patch|x`, or 3.0.0 for `develop`.
fn parse_version(raw: &str) -> Option<Version> {
    if raw.trim().is_empty() {
        return None;
    }
    let digits: Vec<&str> = raw.split('.').collect();
    if digits.len() >= 3 {
        let major = digits[0]
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .collect::<String>();
        let minor = digits[1]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        let patch: String = digits[2]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == 'x')
            .collect();
        if let (Ok(major), Ok(minor)) = (major.parse::<i64>(), minor.parse::<i64>()) {
            let patch = if patch == "x" {
                0
            } else {
                patch.parse().unwrap_or(-1)
            };
            if patch >= 0 {
                return Some(Version::with_build(major, minor, patch));
            }
        }
    }
    raw.eq_ignore_ascii_case("develop")
        .then(|| Version::with_build(3, 0, 0))
}

/// `ContainsCategory`: an empty list means every category.
fn contains_category(categories: &[String], category: &str) -> bool {
    if categories.is_empty() {
        return true;
    }
    let category = if category.trim().is_empty() {
        "Default"
    } else {
        category
    };
    categories.iter().any(|c| c == category)
}

/// `RemovesCompletedDownloads`: whether SABnzbd's history retention would
/// drop a download before completed download handling sees it.
fn removes_completed_downloads(config: &SabConfig) -> bool {
    match config.history_retention_option.as_str() {
        "all" => return false,
        "number-archive" | "number-delete" | "all-archive" | "all-delete" => return true,
        "days-archive" | "days-delete" => return config.history_retention_number < 14,
        _ => {}
    }
    let retention = config.history_retention.trim();
    if retention.is_empty() {
        return false;
    }
    if let Some(days) = retention.strip_suffix('d') {
        return days.parse::<i64>().unwrap_or(0) < 14;
    }
    retention != "0"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions_and_time_left() {
        assert_eq!(parse_version("4.4.1"), Some(Version::with_build(4, 4, 1)));
        assert_eq!(parse_version("3.7.x"), Some(Version::with_build(3, 7, 0)));
        assert_eq!(parse_version("develop"), Some(Version::with_build(3, 0, 0)));
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_time_left("0:10:00"), Duration::minutes(10));
        assert_eq!(parse_time_left("1:02:03:04"), Duration::seconds(86400 + 7384));
        // Absurd figures clamp instead of overflowing a date further on.
        assert_eq!(
            parse_time_left("9999999999:00:00"),
            super::super::time_left(i64::MAX)
        );
        assert_eq!(
            parse_time_left("9223372036854775807:00:00:00"),
            super::super::time_left(i64::MAX)
        );
    }

    #[test]
    fn detects_error_responses() {
        assert!(check_for_error(r#"{"status": false, "error": "API Key Incorrect"}"#).is_err());
        assert!(check_for_error(r#"{"status": "False", "error": "x"}"#).is_err());
        assert!(check_for_error("error: nope").is_err());
        assert!(check_for_error(r#"{"version": "4.4.1"}"#).is_ok());
        assert!(check_for_error("4.4.1").is_ok());
    }

    #[test]
    fn retention_rules() {
        let mut c = SabConfig {
            history_retention_option: "all".into(),
            ..Default::default()
        };
        assert!(!removes_completed_downloads(&c));
        c.history_retention_option = "days-delete".into();
        c.history_retention_number = 7;
        assert!(removes_completed_downloads(&c));
        c.history_retention_option = String::new();
        c.history_retention = "30d".into();
        assert!(!removes_completed_downloads(&c));
        c.history_retention = "5".into();
        assert!(removes_completed_downloads(&c));
    }
}
