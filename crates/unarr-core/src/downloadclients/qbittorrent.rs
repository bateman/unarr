//! qBittorrent, after the originals' `QBittorrent` and `QBittorrentProxyV2`
//! (Web API v2, qBittorrent 4.1+): a login cookie or, on 5.1+, an API key,
//! form posts, torrents listed per category.

use crate::sync::Recover;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

use chrono::Duration;
use serde::Deserialize;

use super::settings::{
    QBittorrentSettings, QBITTORRENT_LAYOUT_ORIGINAL, QBITTORRENT_LAYOUT_SUBFOLDER,
    QBITTORRENT_STATE_FORCE_START, QBITTORRENT_STATE_START, QBITTORRENT_STATE_STOP,
};
use super::version::Version;
use super::GrabRequest;
use super::{
    build_base_url, http_error_message, is_localhost, remote_path, transport_message, Client, ClientError,
    ClientResult, DownloadClientInfo, DownloadClientItem, DownloadItemStatus,
};
use crate::decision::SeedConfiguration;
use crate::validation::ValidationFailure;

const NAME: &str = "qBittorrent";

/// `QBittorrentPreferences`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Preferences {
    pub save_path: String,
    pub max_ratio_enabled: bool,
    pub max_ratio: f64,
    pub max_seeding_time_enabled: bool,
    pub max_seeding_time: i64,
    pub max_inactive_seeding_time_enabled: bool,
    pub max_inactive_seeding_time: i64,
    /// `QBittorrentMaxRatioAction`: Pause 0, Remove 1, EnableSuperSeeding 2, DeleteFiles 3.
    pub max_ratio_act: i64,
    #[serde(default = "yes")]
    pub queueing_enabled: bool,
    #[serde(rename = "dht")]
    pub dht_enabled: bool,
}

fn yes() -> bool {
    true
}

fn minus_two() -> f64 {
    -2.0
}

fn minus_two_i() -> i64 {
    -2
}

/// `QBittorrentTorrent`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Torrent {
    pub hash: String,
    pub name: String,
    pub size: i64,
    pub progress: f64,
    /// Seconds; qBittorrent reports 8640000 when unknown.
    pub eta: f64,
    pub state: String,
    pub label: String,
    pub category: String,
    pub save_path: String,
    pub content_path: String,
    pub ratio: f64,
    #[serde(default = "minus_two")]
    pub ratio_limit: f64,
    pub seeding_time: Option<i64>,
    #[serde(default = "minus_two_i")]
    pub seeding_time_limit: i64,
    #[serde(default = "minus_two_i")]
    pub inactive_seeding_time_limit: i64,
    pub last_activity: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TorrentProperties {
    pub save_path: String,
    pub seeding_time: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Label {
    pub name: String,
    #[serde(rename = "savePath")]
    pub save_path: String,
}

enum Body {
    None,
    Form(Vec<(String, String)>),
    /// A multipart upload (`torrents` file) plus form fields.
    Upload {
        filename: String,
        data: Vec<u8>,
        fields: Vec<(String, String)>,
    },
}

/// `_authCookieCache`: the login cookie per (base URL, user, password).
fn cookie_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct QBittorrent<'a> {
    client: &'a Client<'a>,
    settings: &'a QBittorrentSettings,
}

impl<'a> QBittorrent<'a> {
    pub fn new(client: &'a Client<'a>, settings: &'a QBittorrentSettings) -> QBittorrent<'a> {
        QBittorrent { client, settings }
    }

    pub fn base_url(&self) -> String {
        let s = self.settings;
        build_base_url(s.use_ssl, &s.host, s.port, &s.url_base)
    }

    fn uses_api_key(&self) -> bool {
        !self.settings.api_key.trim().is_empty()
    }

    fn cookie_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.base_url(),
            self.settings.username,
            self.settings.password
        )
    }

    // --- proxy

    fn url(&self, resource: &str) -> String {
        format!("{}{}", self.base_url().trim_end_matches('/'), resource)
    }

    /// The original's message over its inner `HttpException`, whose URL
    /// has the query the request was sent with.
    fn http_error(
        &self,
        message: &str,
        status: reqwest::StatusCode,
        method: &str,
        resource: &str,
        query: &[(&str, String)],
    ) -> ClientError {
        let mut url = self.url(resource);
        if !query.is_empty() {
            if let Ok(with) = reqwest::Url::parse_with_params(&url, query) {
                url = with.to_string();
            }
        }
        ClientError::Http {
            message: message.to_string(),
            cause: http_error_message(status, method, &url),
        }
    }

    /// `BuildRequest`: the request with the API key or the credentials.
    fn request(&self, method: reqwest::Method, resource: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.context.http.request(method, self.url(resource));
        let s = self.settings;
        if self.uses_api_key() {
            builder = builder.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", s.api_key));
        } else if !s.username.trim().is_empty() || !s.password.trim().is_empty() {
            builder = builder.basic_auth(&s.username, Some(&s.password));
        }
        builder
    }

    fn with_body(builder: reqwest::RequestBuilder, body: &Body) -> ClientResult<reqwest::RequestBuilder> {
        Ok(match body {
            Body::None => builder,
            Body::Form(fields) => builder.form(fields),
            Body::Upload {
                filename,
                data,
                fields,
            } => {
                let part = reqwest::multipart::Part::bytes(data.clone())
                    .file_name(filename.clone())
                    .mime_str("application/x-bittorrent")
                    .map_err(|e| ClientError::Other(e.to_string()))?;
                let mut form = reqwest::multipart::Form::new().part("torrents", part);
                for (k, v) in fields {
                    form = form.text(k.clone(), v.clone());
                }
                builder.multipart(form)
            }
        })
    }

    /// `AuthenticateClient`: the login cookie, fetched when not cached or
    /// when `reauthenticate`.
    async fn authenticate(&self, reauthenticate: bool) -> ClientResult<Option<String>> {
        let s = self.settings;
        if s.username.trim().is_empty() || s.password.trim().is_empty() {
            if reauthenticate {
                return Err(ClientError::Authentication(format!(
                    "Failed to authenticate with {NAME}."
                )));
            }
            return Ok(None);
        }
        let key = self.cookie_key();
        if !reauthenticate {
            if let Some(cookie) = cookie_cache().lock().recover().get(&key) {
                return Ok(Some(cookie.clone()));
            }
        }
        cookie_cache().lock().recover().remove(&key);
        let response = self
            .request(reqwest::Method::POST, "/api/v2/auth/login")
            .form(&[
                ("username", s.username.as_str()),
                ("password", s.password.as_str()),
            ])
            .send()
            .await
            .map_err(|_| {
                ClientError::Unavailable(format!(
                    "Failed to connect to {NAME}, please check your settings."
                ))
            })?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ClientError::Authentication(format!(
                "Failed to authenticate with {NAME}."
            )));
        }
        if !status.is_success() {
            return Err(self.http_error(
                &format!("Failed to connect to {NAME}, please check your settings."),
                status,
                "POST",
                "/api/v2/auth/login",
                &[],
            ));
        }
        let cookies: Vec<String> = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|c| c.split(';').next().map(str::trim).map(String::from))
            .collect();
        let content = crate::http::read_text(response, crate::http::MAX_RESPONSE_BYTES)
            .await
            .unwrap_or_default();
        if !content.trim().is_empty() && content != "Ok." {
            // "Fails." on a bad login.
            return Err(ClientError::Authentication(format!(
                "Failed to authenticate with {NAME}."
            )));
        }
        let cookie = cookies.join("; ");
        cookie_cache().lock().recover().insert(key, cookie.clone());
        Ok(Some(cookie))
    }

    /// The request sent, with the login dance when needed: the status and
    /// the body, whatever the status.
    async fn raw(
        &self,
        method: reqwest::Method,
        resource: &str,
        query: &[(&str, String)],
        body: Body,
    ) -> ClientResult<(reqwest::StatusCode, String)> {
        if self.uses_api_key() {
            let builder = Self::with_body(self.request(method, resource).query(query), &body)?;
            let response = builder.send().await.map_err(|_| {
                ClientError::Other(format!(
                    "Failed to connect to {NAME}, please check your settings."
                ))
            })?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                return Err(ClientError::Authentication(format!(
                    "Failed to authenticate with {NAME}."
                )));
            }
            let content = crate::http::read_text(response, crate::http::MAX_RESPONSE_BYTES)
                .await
                .unwrap_or_default();
            return Ok((status, content));
        }
        let mut cookie = self.authenticate(false).await?;
        for attempt in 0..2 {
            let mut builder = self.request(method.clone(), resource).query(query);
            if let Some(c) = &cookie {
                builder = builder.header(reqwest::header::COOKIE, c.clone());
            }
            let builder = Self::with_body(builder, &body)?;
            let response = builder.send().await.map_err(|_| {
                ClientError::Other(format!(
                    "Failed to connect to {NAME}, please check your settings."
                ))
            })?;
            let status = response.status();
            if status == reqwest::StatusCode::FORBIDDEN && attempt == 0 {
                tracing::debug!("qBittorrent asked for authentication, logging in");
                cookie = self.authenticate(true).await?;
                continue;
            }
            let content = crate::http::read_text(response, crate::http::MAX_RESPONSE_BYTES)
                .await
                .unwrap_or_default();
            return Ok((status, content));
        }
        unreachable!("the retry loop returns")
    }

    /// `ProcessRequest`: a successful answer's body.
    async fn process(
        &self,
        method: reqwest::Method,
        resource: &str,
        query: &[(&str, String)],
        body: Body,
    ) -> ClientResult<String> {
        let (status, content) = self.raw(method.clone(), resource, query, body).await?;
        if !status.is_success() {
            return Err(self.http_error(
                &format!("Failed to connect to {NAME}, check your settings."),
                status,
                method.as_str(),
                resource,
                query,
            ));
        }
        Ok(content)
    }

    async fn get(&self, resource: &str, query: &[(&str, String)]) -> ClientResult<String> {
        self.process(reqwest::Method::GET, resource, query, Body::None)
            .await
    }

    async fn post(&self, resource: &str, fields: Vec<(String, String)>) -> ClientResult<String> {
        self.process(reqwest::Method::POST, resource, &[], Body::Form(fields))
            .await
    }

    fn parse<T: serde::de::DeserializeOwned>(content: &str, what: &str) -> ClientResult<T> {
        serde_json::from_str(content)
            .map_err(|e| ClientError::Other(format!("Unexpected {NAME} {what} response: {e}")))
    }

    /// `IsApiSupported`: whether the v2 API answers (404 means a version
    /// before 4.1, 403 means it is there but wants a login).
    pub async fn is_api_supported(&self) -> ClientResult<bool> {
        let response = self
            .request(reqwest::Method::GET, "/api/v2/app/webapiVersion")
            .send()
            .await
            // The originals let the socket error through as is.
            .map_err(|e| ClientError::Other(transport_message(&e)))?;
        let resource = "/api/v2/app/webapiVersion";
        match response.status() {
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            reqwest::StatusCode::FORBIDDEN => Ok(true),
            s @ reqwest::StatusCode::UNAUTHORIZED => Err(self.http_error(
                &format!("Failed to connect to {NAME}. Check your settings and {NAME} configuration."),
                s,
                "GET",
                resource,
                &[],
            )),
            s if !s.is_success() => Err(self.http_error(
                &format!("Failed to connect to {NAME}, check your settings."),
                s,
                "GET",
                resource,
                &[],
            )),
            _ => Ok(true),
        }
    }

    pub async fn api_version(&self) -> ClientResult<Version> {
        if !self.is_api_supported().await? {
            return Err(ClientError::Other(format!(
                "Unable to determine {NAME} API version"
            )));
        }
        let content = self.get("/api/v2/app/webapiVersion", &[]).await?;
        Version::parse(&content)
            .ok_or_else(|| ClientError::Other(format!("Unexpected {NAME} API version: {content}")))
    }

    pub async fn version(&self) -> ClientResult<String> {
        Ok(self
            .get("/api/v2/app/version", &[])
            .await?
            .trim_start_matches('v')
            .to_string())
    }

    pub async fn config(&self) -> ClientResult<Preferences> {
        Self::parse(&self.get("/api/v2/app/preferences", &[]).await?, "preferences")
    }

    pub async fn torrents(&self) -> ClientResult<Vec<Torrent>> {
        let mut query = Vec::new();
        if !self.settings.category.trim().is_empty() {
            query.push(("category", self.settings.category.clone()));
        }
        Self::parse(&self.get("/api/v2/torrents/info", &query).await?, "torrents")
    }

    pub async fn properties(&self, hash: &str) -> ClientResult<TorrentProperties> {
        Self::parse(
            &self
                .get("/api/v2/torrents/properties", &[("hash", hash.into())])
                .await?,
            "torrent properties",
        )
    }

    pub async fn files(&self, hash: &str) -> ClientResult<Vec<String>> {
        #[derive(Deserialize)]
        struct File {
            name: String,
        }
        let files: Vec<File> = Self::parse(
            &self
                .get("/api/v2/torrents/files", &[("hash", hash.into())])
                .await?,
            "torrent files",
        )?;
        Ok(files.into_iter().map(|f| f.name).collect())
    }

    pub async fn is_loaded(&self, hash: &str) -> bool {
        self.properties(hash).await.is_ok()
    }

    async fn download_form_fields(
        &self,
        seed: Option<&SeedConfiguration>,
    ) -> ClientResult<Vec<(String, String)>> {
        let s = self.settings;
        let mut fields = Vec::new();
        if !s.category.trim().is_empty() {
            fields.push(("category".to_string(), s.category.clone()));
        }
        if s.initial_state == QBITTORRENT_STATE_START || s.initial_state == QBITTORRENT_STATE_STOP {
            // 5.0 renamed `paused` to `stopped`; ForceStart is a separate call.
            let name = if self.api_version().await? >= Version::with_build(2, 11, 0) {
                "stopped"
            } else {
                "paused"
            };
            fields.push((
                name.to_string(),
                (s.initial_state == QBITTORRENT_STATE_STOP).to_string(),
            ));
        }
        if s.sequential_order {
            fields.push(("sequentialDownload".into(), "true".into()));
        }
        if s.first_and_last {
            fields.push(("firstLastPiecePrio".into(), "true".into()));
        }
        if s.content_layout == QBITTORRENT_LAYOUT_ORIGINAL {
            fields.push(("contentLayout".into(), "Original".into()));
        } else if s.content_layout == QBITTORRENT_LAYOUT_SUBFOLDER {
            fields.push(("contentLayout".into(), "Subfolder".into()));
        }
        if let Some(seed) = seed {
            fields.extend(seeding_fields(seed, false));
        }
        Ok(fields)
    }

    pub async fn add_from_url(&self, url: &str, seed: Option<&SeedConfiguration>) -> ClientResult<()> {
        let mut fields = vec![("urls".to_string(), url.to_string())];
        fields.extend(self.download_form_fields(seed).await?);
        let result = self.post("/api/v2/torrents/add", fields).await?;
        if result == "Fails." {
            return Err(ClientError::Other(
                "Download client failed to add torrent by url".into(),
            ));
        }
        Ok(())
    }

    pub async fn add_from_file(
        &self,
        filename: &str,
        data: Vec<u8>,
        seed: Option<&SeedConfiguration>,
    ) -> ClientResult<()> {
        let fields = self.download_form_fields(seed).await?;
        let body = Body::Upload {
            filename: filename.to_string(),
            data,
            fields,
        };
        let result = self
            .process(reqwest::Method::POST, "/api/v2/torrents/add", &[], body)
            .await?;
        if result == "Fails." {
            return Err(ClientError::Other("Download client failed to add torrent".into()));
        }
        Ok(())
    }

    pub async fn remove_torrent(&self, hash: &str, delete_data: bool) -> ClientResult<()> {
        let mut fields = vec![("hashes".to_string(), hash.to_string())];
        if delete_data {
            fields.push(("deleteFiles".into(), "true".into()));
        }
        self.post("/api/v2/torrents/delete", fields).await.map(|_| ())
    }

    pub async fn set_label(&self, hash: &str, label: &str) -> ClientResult<()> {
        self.post(
            "/api/v2/torrents/setCategory",
            vec![("hashes".into(), hash.into()), ("category".into(), label.into())],
        )
        .await
        .map(|_| ())
    }

    pub async fn add_label(&self, label: &str) -> ClientResult<()> {
        self.post(
            "/api/v2/torrents/createCategory",
            vec![("category".into(), label.into())],
        )
        .await
        .map(|_| ())
    }

    pub async fn labels(&self) -> ClientResult<BTreeMap<String, Label>> {
        Self::parse(&self.get("/api/v2/torrents/categories", &[]).await?, "categories")
    }

    /// `SetTorrentSeedingConfiguration`; a 404 (API before 2.0.1) is ignored.
    pub async fn set_seeding(&self, hash: &str, seed: &SeedConfiguration) -> ClientResult<()> {
        let mut fields = vec![("hashes".to_string(), hash.to_string())];
        fields.extend(seeding_fields(seed, true));
        let (status, _) = self
            .raw(
                reqwest::Method::POST,
                "/api/v2/torrents/setShareLimits",
                &[],
                Body::Form(fields),
            )
            .await?;
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(self.http_error(
            &format!("Failed to connect to {NAME}, check your settings."),
            status,
            "POST",
            "/api/v2/torrents/setShareLimits",
            &[],
        ))
    }

    /// `MoveTorrentToTopInQueue`; a 409 (queueing disabled) is ignored.
    pub async fn move_to_top(&self, hash: &str) -> ClientResult<()> {
        let (status, _) = self
            .raw(
                reqwest::Method::POST,
                "/api/v2/torrents/topPrio",
                &[],
                Body::Form(vec![("hashes".into(), hash.into())]),
            )
            .await?;
        if status.is_success() || status == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }
        Err(self.http_error(
            &format!("Failed to connect to {NAME}, check your settings."),
            status,
            "POST",
            "/api/v2/torrents/topPrio",
            &[],
        ))
    }

    pub async fn set_force_start(&self, hash: &str, enabled: bool) -> ClientResult<()> {
        self.post(
            "/api/v2/torrents/setForceStart",
            vec![
                ("hashes".into(), hash.into()),
                ("value".into(), enabled.to_string()),
            ],
        )
        .await
        .map(|_| ())
    }

    // --- client

    /// `WaitForTorrent`: up to half a second for the torrent to show.
    async fn wait_for_torrent(&self, hash: &str) -> bool {
        for _ in 0..10 {
            if self.is_loaded(hash).await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        tracing::warn!(
            hash,
            "torrent not loaded within 500 ms, skipping additional parameters"
        );
        false
    }

    async fn after_add(
        &self,
        request: &GrabRequest<'_>,
        hash: &str,
        limits_set_on_add: bool,
    ) -> ClientResult<()> {
        let hash = hash.to_ascii_lowercase();
        let seed = request.seed_limits();
        let set_share_limits = seed.is_some() && !limits_set_on_add;
        let move_to_top = self.settings.move_to_top(request.recent);
        let force_start = self.settings.initial_state == QBITTORRENT_STATE_FORCE_START;
        if !(set_share_limits || move_to_top || force_start) {
            return Ok(());
        }
        if !self.wait_for_torrent(&hash).await {
            return Ok(());
        }
        if let (true, Some(seed)) = (set_share_limits, seed) {
            if let Err(e) = self.set_seeding(&hash, seed).await {
                tracing::warn!(hash, error = %e.exception(), "failed to set the torrent seed criteria");
            }
        }
        if move_to_top {
            if let Err(e) = self.move_to_top(&hash).await {
                tracing::warn!(hash, error = %e.exception(), "failed to set the torrent priority");
            }
        }
        if force_start {
            if let Err(e) = self.set_force_start(&hash, true).await {
                tracing::warn!(hash, error = %e.exception(), "failed to set ForceStart");
            }
        }
        Ok(())
    }

    /// Radarr 7: a 409 from the add, which qBittorrent answers when it has
    /// the torrent already, refuses the release rather than fail the grab
    /// as a fault of the client.
    fn conflict_rejects(&self, e: ClientError, what: &str) -> ClientError {
        let conflict = matches!(&e, ClientError::Http { cause, .. }
            if cause.starts_with("HTTP request failed: [409:"));
        if conflict && self.settings.app.qbittorrent_conflict_rejects {
            ClientError::Rejected(format!("QBittorrent rejected the {what} due to a conflict"))
        } else {
            e
        }
    }

    pub async fn add_magnet(
        &self,
        request: &GrabRequest<'_>,
        hash: &str,
        magnet: &str,
    ) -> ClientResult<String> {
        if !self.config().await?.dht_enabled && !magnet.contains("&tr=") {
            return Err(ClientError::NotSupported(
                "Magnet Links without trackers not supported if DHT is disabled".into(),
            ));
        }
        let seed = request.seed_limits();
        let on_add = seed.is_some() && self.api_version().await? >= Version::with_build(2, 8, 1);
        self.add_from_url(magnet, if on_add { seed } else { None })
            .await
            .map_err(|e| self.conflict_rejects(e, "magnet link"))?;
        self.after_add(request, hash, on_add).await?;
        Ok(hash.to_string())
    }

    pub async fn add_torrent(
        &self,
        request: &GrabRequest<'_>,
        hash: &str,
        filename: &str,
        torrent: Vec<u8>,
    ) -> ClientResult<String> {
        let seed = request.seed_limits();
        let on_add = seed.is_some() && self.api_version().await? >= Version::with_build(2, 8, 1);
        self.add_from_file(filename, torrent, if on_add { seed } else { None })
            .await
            .map_err(|e| self.conflict_rejects(e, "torrent file"))?;
        self.after_add(request, hash, on_add).await?;
        Ok(hash.to_string())
    }

    pub async fn items(&self) -> ClientResult<Vec<DownloadClientItem>> {
        let version = self.api_version().await?;
        let config = self.config().await?;
        let torrents = self.torrents().await?;
        let has_post_import = !self.settings.imported_category.trim().is_empty();
        let mut out = Vec::new();
        for t in torrents {
            let mut item = DownloadClientItem::new(
                self.client.definition.item_client_info(has_post_import),
                t.hash.to_ascii_uppercase(),
                t.name.clone(),
            );
            item.category = Some(if t.category.trim().is_empty() {
                t.label.clone()
            } else {
                t.category.clone()
            });
            item.total_size = t.size;
            item.remaining_size = (t.size as f64 * (1.0 - t.progress)) as i64;
            item.remaining_time = remaining_time(&t);
            item.seed_ratio = Some(t.ratio);
            // Removal needs the torrent paused with its ratio reached: a
            // higher per-torrent limit is not exposed by the listing.
            let removable = item.client.remove_completed_downloads
                && matches!(t.state.as_str(), "pausedUP" | "stoppedUP")
                && self.has_reached_seed_limit(&t, &config).await;
            item.can_move_files = removable;
            item.can_be_removed = removable;
            match t.state.as_str() {
                "error" => {
                    item.status = DownloadItemStatus::Warning;
                    item.message = Some(format!("{NAME} is reporting an error"));
                }
                "stoppedDL" | "pausedDL" => item.status = DownloadItemStatus::Paused,
                "queuedDL" | "checkingDL" | "checkingUP" | "checkingResumeData" => {
                    item.status = DownloadItemStatus::Queued
                }
                "pausedUP" | "stoppedUP" | "uploading" | "stalledUP" | "queuedUP" | "forcedUP" => {
                    item.status = DownloadItemStatus::Completed;
                    item.remaining_time = Some(Duration::zero());
                }
                "stalledDL" => {
                    item.status = DownloadItemStatus::Warning;
                    item.message = Some("The download is stalled with no connections".into());
                }
                "missingFiles" => {
                    item.status = DownloadItemStatus::Warning;
                    item.message = Some(format!("{NAME} is reporting missing files"));
                }
                "metaDL" | "forcedMetaDL" => {
                    if config.dht_enabled {
                        item.status = DownloadItemStatus::Queued;
                        item.message = Some(format!("{NAME} is downloading metadata"));
                    } else {
                        item.status = DownloadItemStatus::Warning;
                        item.message = Some(format!("{NAME} cannot resolve magnet link with DHT disabled"));
                    }
                }
                "forcedDL" | "moving" | "downloading" => item.status = DownloadItemStatus::Downloading,
                other => {
                    item.message = Some(format!("Unknown download state: {other}"));
                    tracing::info!(state = other, "unknown qBittorrent download state");
                    item.status = DownloadItemStatus::Downloading;
                }
            }
            if version >= Version::with_build(2, 6, 1) && item.status == DownloadItemStatus::Completed {
                if t.content_path != t.save_path {
                    item.output_path = Some(self.client.context.remap(&self.settings.host, &t.content_path));
                } else {
                    item.status = DownloadItemStatus::Warning;
                    item.message = Some("Unable to Import. Path matches client base download directory, it's possible 'Keep top-level folder' is disabled for this torrent or 'Torrent Content Layout' is NOT set to 'Original' or 'Create Subfolder'?".into());
                }
            }
            out.push(item);
        }
        Ok(out)
    }

    async fn has_reached_seed_limit(&self, t: &Torrent, config: &Preferences) -> bool {
        if t.ratio_limit >= 0.0 {
            if t.ratio_limit - t.ratio <= 0.001 {
                return true;
            }
        } else if t.ratio_limit == -2.0 && config.max_ratio_enabled && config.max_ratio - t.ratio <= 0.001 {
            return true;
        }
        self.has_reached_seeding_time_limit(t, config).await
            || has_reached_inactive_limit(t, config, self.client.context.now.timestamp())
    }

    async fn has_reached_seeding_time_limit(&self, t: &Torrent, config: &Preferences) -> bool {
        let limit = if t.seeding_time_limit >= 0 {
            t.seeding_time_limit * 60
        } else if t.seeding_time_limit == -2 && config.max_seeding_time_enabled {
            config.max_seeding_time * 60
        } else {
            return false;
        };
        let seeding_time = match t.seeding_time {
            Some(s) => s,
            // The listing does not say; the properties do.
            None => match self.properties(&t.hash).await {
                Ok(p) => p.seeding_time,
                Err(_) => return false,
            },
        };
        seeding_time >= limit
    }

    pub async fn import_item(&self, item: &DownloadClientItem) -> ClientResult<DownloadClientItem> {
        if item.output_path.is_some() {
            return Ok(item.clone());
        }
        let hash = item.download_id.to_ascii_lowercase();
        let files = self.files(&hash).await?;
        let Some(first) = files.first() else {
            tracing::debug!(title = %item.title, "no files found for torrent in qBittorrent");
            return Ok(item.clone());
        };
        let properties = self.properties(&hash).await?;
        // qBittorrent returns `/` separators even on Windows: the first
        // fragment is the torrent's folder.
        let top = first.split(['/', '\\']).next().unwrap_or(first);
        let mut result = item.clone();
        result.output_path = Some(self.client.context.remap(
            &self.settings.host,
            &remote_path::join(&properties.save_path, top),
        ));
        Ok(result)
    }

    pub async fn remove(&self, item: &DownloadClientItem, delete_data: bool) -> ClientResult<()> {
        self.remove_torrent(&item.download_id.to_ascii_lowercase(), delete_data)
            .await
    }

    pub async fn mark_imported(&self, item: &DownloadClientItem) -> ClientResult<()> {
        let imported = self.settings.imported_category.trim();
        if !imported.is_empty() && imported != self.settings.category {
            if let Err(e) = self
                .set_label(&item.download_id.to_ascii_lowercase(), imported)
                .await
            {
                tracing::warn!(label = imported, title = %item.title, error = %e.exception(), "failed to set post-import torrent label; does the label exist?");
            }
        }
        Ok(())
    }

    pub async fn status(&self) -> ClientResult<DownloadClientInfo> {
        let version = self.api_version().await?;
        let config = self.config().await?;
        let mut dest_dir = config.save_path.clone();
        let tv = self.settings.category.trim();
        if !tv.is_empty() && version >= Version::new(2, 0) {
            if let Some(label) = self.labels().await?.get(tv) {
                if !label.save_path.trim().is_empty() {
                    let mut save_path = label.save_path.clone();
                    if save_path.starts_with("//") {
                        save_path = save_path.replace('/', "\\");
                    }
                    dest_dir = if remote_path::is_rooted(&save_path) {
                        save_path
                    } else {
                        remote_path::join(&dest_dir, &save_path)
                    };
                }
            }
        }
        Ok(DownloadClientInfo {
            is_localhost: is_localhost(&self.settings.host),
            sorting_mode: None,
            removes_completed_downloads: removes_completed_downloads(&config),
            output_root_folders: vec![self.client.context.remap(&self.settings.host, &dest_dir)],
        })
    }

    pub async fn test(&self, failures: &mut Vec<ValidationFailure>) -> ClientResult<()> {
        failures.extend(self.test_connection().await);
        if crate::validation::has_errors(failures) {
            return Ok(());
        }
        failures.extend(self.test_category().await?);
        failures.extend(self.test_priority_support().await);
        failures.extend(self.test_get_torrents().await);
        Ok(())
    }

    async fn test_connection(&self) -> Option<ValidationFailure> {
        let app = self.settings.app.name;
        let outcome: ClientResult<Option<ValidationFailure>> = async {
            let version = self.api_version().await?;
            if version < Version::new(1, 5) {
                // API version 5 introduced `save_path`.
                return Ok(Some(ValidationFailure::error(
                    "Host",
                    &format!("{NAME} version should be at least 3.2.4. Version reported is {version}"),
                )));
            } else if version < Version::new(1, 6) {
                // API version 6 introduced labels.
                if !self.settings.category.trim().is_empty() {
                    return Ok(Some(
                        ValidationFailure::error("Category", "Category is not supported").with_detail(
                            "Categories are not supported until qBittorrent version 3.3.0. Please upgrade or try again with an empty Category.",
                        ),
                    ));
                }
            } else if self.settings.category.trim().is_empty() {
                return Ok(Some(
                    ValidationFailure::warning(self.settings.app.category_property, "Category is recommended").with_detail(&format!(
                        "{app} will not attempt to import completed downloads without a category."
                    )),
                ));
            }
            let config = self.config().await?;
            if removes_completed_downloads(&config) {
                return Ok(Some(
                    ValidationFailure::error(
                        "",
                        &format!("{NAME} is configured to remove torrents when they reach their Share Ratio Limit"),
                    )
                    .with_detail(&format!(
                        "{app} will be unable to perform Completed Download Handling as configured. You can fix this in {NAME} ('Tools -> Options...' in the menu) by changing 'Options -> BitTorrent -> Share Ratio Limiting' from 'Remove them' to 'Pause them'"
                    )),
                ));
            }
            Ok(None)
        }
        .await;
        match outcome {
            Ok(failure) => failure,
            Err(ClientError::Authentication(e)) => {
                tracing::error!(error = %e, "qBittorrent authentication failed");
                let property = if self.uses_api_key() { "ApiKey" } else { "Username" };
                Some(
                    ValidationFailure::error(property, "Authentication Failure").with_detail(&format!(
                        "Please verify your credentials. Also verify if the host running {app} isn't blocked from accessing {NAME} by WhiteList limitations in the {NAME} configuration."
                    )),
                )
            }
            Err(e) => {
                tracing::error!(error = %e.exception(), "unable to test qBittorrent");
                Some(
                    ValidationFailure::error("Host", &format!("Unable to connect to {NAME}"))
                        .with_detail(e.message()),
                )
            }
        }
    }

    async fn test_category(&self) -> ClientResult<Option<ValidationFailure>> {
        let app = self.settings.app.name;
        let tv = self.settings.category.trim();
        let imported = self.settings.imported_category.trim();
        if tv.is_empty() && imported.is_empty() {
            return Ok(None);
        }
        if self.api_version().await? < Version::new(2, 0) {
            return Ok(None);
        }
        let mut labels = self.labels().await?;
        for (property, label) in [
            (self.settings.app.category_property, tv),
            (self.settings.app.imported_category_property, imported),
        ] {
            if label.is_empty() || labels.contains_key(label) {
                continue;
            }
            self.add_label(label).await?;
            labels = self.labels().await?;
            if !labels.contains_key(label) {
                return Ok(Some(
                    ValidationFailure::error(property, "Configuration of category failed")
                        .with_detail(&format!("{app} was unable to add the label to {NAME}.")),
                ));
            }
        }
        Ok(None)
    }

    async fn test_priority_support(&self) -> Option<ValidationFailure> {
        let recent_default = !self.settings.move_to_top(true);
        let older_default = !self.settings.move_to_top(false);
        if recent_default && older_default {
            return None;
        }
        match self.config().await {
            Ok(config) if !config.queueing_enabled => {
                let property = if !recent_default {
                    "RecentTvPriority"
                } else {
                    "OlderTvPriority"
                };
                Some(
                    ValidationFailure::error(property, "Queueing Not Enabled").with_detail(&format!(
                        "Torrent Queueing is not enabled in your {NAME} settings. Enable it in {NAME} or select 'Last' as priority."
                    )),
                )
            }
            Ok(_) => None,
            Err(e) => {
                tracing::error!(error = %e.exception(), "failed to test qBittorrent");
                Some(ValidationFailure::error(
                    "",
                    &format!("Unknown exception: {}", e.message()),
                ))
            }
        }
    }

    async fn test_get_torrents(&self) -> Option<ValidationFailure> {
        match self.torrents().await {
            Ok(_) => None,
            Err(e) => {
                tracing::error!(error = %e.exception(), "failed to get torrents");
                Some(ValidationFailure::error(
                    "",
                    &format!("Failed to get the list of torrents: {}", e.message()),
                ))
            }
        }
    }
}

/// `AddTorrentSeedingFormParameters`: -2 means "use the global limit".
fn seeding_fields(seed: &SeedConfiguration, always: bool) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let ratio = seed.ratio.unwrap_or(-2.0);
    let minutes = seed.seed_time_minutes.unwrap_or(-2);
    if ratio != -2.0 || always {
        fields.push(("ratioLimit".to_string(), format_ratio(ratio)));
    }
    if minutes != -2 || always {
        fields.push(("seedingTimeLimit".to_string(), minutes.to_string()));
    }
    fields
}

/// A ratio as .NET prints a double: no trailing `.0`.
fn format_ratio(ratio: f64) -> String {
    if ratio.fract() == 0.0 {
        format!("{}", ratio as i64)
    } else {
        format!("{ratio}")
    }
}

/// `GetRemainingTime`: qBittorrent sends 8640000 when unknown.
fn remaining_time(t: &Torrent) -> Option<Duration> {
    if t.eta < 0.0 || t.eta > 365.0 * 24.0 * 3600.0 || t.eta == 8640000.0 {
        return None;
    }
    Some(super::time_left(t.eta as i64))
}

fn has_reached_inactive_limit(t: &Torrent, config: &Preferences, now: i64) -> bool {
    let limit = if t.inactive_seeding_time_limit >= 0 {
        t.inactive_seeding_time_limit * 60
    } else if t.inactive_seeding_time_limit == -2 && config.max_inactive_seeding_time_enabled {
        config.max_inactive_seeding_time * 60
    } else {
        return false;
    };
    now - t.last_activity > limit
}

/// `RemovesCompletedDownloads`: a ratio or a short seeding time limit that
/// removes (or deletes) torrents.
fn removes_completed_downloads(config: &Preferences) -> bool {
    let minimum_retention = 60 * 24 * 14;
    (config.max_ratio_enabled
        || (config.max_seeding_time_enabled && config.max_seeding_time < minimum_retention))
        && (config.max_ratio_act == 1 || config.max_ratio_act == 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_fields_and_remaining_time() {
        let seed = SeedConfiguration {
            ratio: Some(1.5),
            seed_time_minutes: None,
        };
        assert_eq!(
            seeding_fields(&seed, false),
            vec![("ratioLimit".to_string(), "1.5".to_string())]
        );
        assert_eq!(
            seeding_fields(&seed, true),
            vec![
                ("ratioLimit".to_string(), "1.5".to_string()),
                ("seedingTimeLimit".to_string(), "-2".to_string())
            ]
        );
        let t = Torrent {
            eta: 8640000.0,
            ..Default::default()
        };
        assert_eq!(remaining_time(&t), None);
        let t = Torrent {
            eta: 600.0,
            ..Default::default()
        };
        assert_eq!(remaining_time(&t), Some(Duration::minutes(10)));
    }

    #[test]
    fn preferences_default_queueing_on() {
        let p: Preferences =
            serde_json::from_str(r#"{"save_path": "/d/", "max_ratio_act": 1, "max_ratio_enabled": true}"#)
                .unwrap();
        assert!(p.queueing_enabled);
        assert!(removes_completed_downloads(&p));
        let t: Torrent = serde_json::from_str(r#"{"hash": "ab", "eta": 99999999999999999999}"#).unwrap();
        assert_eq!(t.ratio_limit, -2.0);
        assert_eq!(remaining_time(&t), None);
    }

    /// qBittorrent answers 409 when nothing it was sent was added (the
    /// torrent is there already): the message stays the original's, and
    /// the status reaches the log and the description.
    #[tokio::test]
    async fn an_error_status_keeps_its_code_for_the_log() {
        use axum::routing::{get, post};
        let app = axum::Router::new()
            .route("/api/v2/app/webapiVersion", get(|| async { "2.11.4" }))
            .route(
                "/api/v2/torrents/add",
                post(|| async { (axum::http::StatusCode::CONFLICT, "") }),
            )
            .route(
                "/api/v2/torrents/info",
                get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut settings = QBittorrentSettings::defaults(super::super::RADARR);
        settings.host = addr.ip().to_string();
        settings.port = addr.port() as i64;
        let definition = super::super::DownloadClientDefinition {
            id: 1,
            name: "qBittorrent".into(),
            kind: super::super::ClientKind::QBittorrent,
            enable: true,
            priority: 1,
            remove_completed_downloads: true,
            remove_failed_downloads: true,
            tags: vec![],
            settings: super::super::ClientSettings::QBittorrent(settings.clone()),
        };
        let context = super::super::ClientContext::new(
            crate::http::build(std::time::Duration::from_secs(5)),
            chrono::Utc::now(),
        );
        let client = Client::new(&definition, &context);
        let e = QBittorrent::new(&client, &settings)
            .add_from_url(
                "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            e.message(),
            "Failed to connect to qBittorrent, check your settings."
        );
        assert_eq!(e.to_string(), e.message());
        let cause =
            format!("HTTP request failed: [409:Conflict] [POST] at [http://{addr}/api/v2/torrents/add]");
        assert_eq!(e.cause(), Some(cause.as_str()));
        assert_eq!(
            e.exception(),
            format!("Failed to connect to qBittorrent, check your settings. ---> {cause}")
        );
        let api = e.into_anyhow();
        assert_eq!(
            api.to_string(),
            "Failed to connect to qBittorrent, check your settings."
        );
        assert!(format!("{api:#}").ends_with(&cause), "{api:#}");

        // Radarr 7 refuses the release over that 409, where Sonarr's pin
        // fails the grab as the client's error.
        let rejected = QBittorrent::new(&client, &settings).conflict_rejects(
            QBittorrent::new(&client, &settings)
                .add_from_url("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567", None)
                .await
                .unwrap_err(),
            "magnet link",
        );
        assert!(
            matches!(&rejected, ClientError::Rejected(m) if m == "QBittorrent rejected the magnet link due to a conflict"),
            "{rejected:?}"
        );
        let mut sonarr = settings.clone();
        sonarr.app = super::super::SONARR;
        let kept = QBittorrent::new(&client, &sonarr).conflict_rejects(
            QBittorrent::new(&client, &sonarr)
                .add_from_url("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567", None)
                .await
                .unwrap_err(),
            "magnet link",
        );
        assert!(matches!(&kept, ClientError::Http { .. }), "{kept:?}");

        // The cause names the URL with its query, as the originals log it.
        let e = QBittorrent::new(&client, &settings)
            .get("/api/v2/torrents/info", &[("category", "radarr".to_string())])
            .await
            .unwrap_err();
        assert_eq!(
            e.cause(),
            Some(
                format!(
                    "HTTP request failed: [500:InternalServerError] [GET] at [http://{addr}/api/v2/torrents/info?category=radarr]"
                )
                .as_str()
            )
        );
    }
}
