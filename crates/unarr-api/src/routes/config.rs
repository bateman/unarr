//! `config/*` resources. Values the originals keep in their key/value
//! `Config` table are read through [`Values`], which supplies the same
//! defaults the originals compile in.

use std::collections::HashMap;

use axum::extract::State;
use axum::Json;
use unarr_core::store;

use crate::error::ApiResult;
use crate::state::AppState;

/// The key/value settings table with typed, defaulted access.
pub struct Values(HashMap<String, String>);

impl Values {
    pub async fn load(state: &AppState) -> anyhow::Result<Values> {
        let rows = state.db().call(|conn| store::config::all_config(conn)).await?;
        Ok(Values(rows.into_iter().collect()))
    }

    pub fn str(&self, key: &str, default: &str) -> String {
        self.0
            .get(&key.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    pub fn bool(&self, key: &str, default: bool) -> bool {
        match self.0.get(&key.to_ascii_lowercase()) {
            Some(v) => v.eq_ignore_ascii_case("true"),
            None => default,
        }
    }

    pub fn int(&self, key: &str, default: i64) -> i64 {
        self.0
            .get(&key.to_ascii_lowercase())
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    pub fn float(&self, key: &str, default: f64) -> f64 {
        self.0
            .get(&key.to_ascii_lowercase())
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// An enumeration stored by name (the originals store the C# member name,
    /// e.g. `PreferAndUpgrade`); returned in camelCase as the API spells it.
    pub fn enum_name(&self, key: &str, default: &str) -> String {
        match self.0.get(&key.to_ascii_lowercase()) {
            Some(v) if !v.is_empty() => lower_first(v),
            _ => default.to_string(),
        }
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub async fn host(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let v = Values::load(&state).await?;
    // The originals report the first user's name and stored password hash.
    let user = state
        .db()
        .call(|c| Ok(store::providers::list_users(c)?.into_iter().next()))
        .await?;
    let s = state.settings();
    let h = &s.host;
    Ok(Json(serde_json::json!({
        "bindAddress": h.bind_address,
        "port": h.port,
        "sslPort": h.ssl_port,
        "enableSsl": h.enable_ssl,
        "launchBrowser": h.launch_browser,
        "authenticationMethod": h.authentication_method.api_name(),
        "authenticationRequired": h.authentication_required.api_name(),
        "allowedHosts": h.allowed_hosts,
        "analyticsEnabled": h.analytics_enabled,
        "username": user.as_ref().map(|u| u.username.clone()).unwrap_or_default(),
        "password": user.as_ref().map(|u| u.password.clone()).unwrap_or_default(),
        "passwordConfirmation": "",
        "logLevel": h.log_level,
        "logSizeLimit": 1,
        "consoleLogLevel": "",
        "branch": h.branch,
        "apiKey": h.api_key,
        "sslCertPath": h.ssl_cert_path,
        "sslCertPassword": h.ssl_cert_password,
        "urlBase": s.url_base,
        "trustedNetworks": h.trusted_networks,
        "instanceName": s.instance_name,
        "applicationUrl": v.str("ApplicationUrl", ""),
        "updateAutomatically": false,
        "updateMechanism": "builtIn",
        "updateScriptPath": "",
        "proxyEnabled": v.bool("ProxyEnabled", false),
        "proxyType": v.enum_name("ProxyType", "http"),
        "proxyHostname": v.str("ProxyHostname", ""),
        "proxyPort": v.int("ProxyPort", 8080),
        "proxyUsername": v.str("ProxyUsername", ""),
        "proxyPassword": v.str("ProxyPassword", ""),
        "proxyBypassFilter": v.str("ProxyBypassFilter", ""),
        "proxyBypassLocalAddresses": v.bool("ProxyBypassLocalAddresses", true),
        "certificateValidation": v.enum_name("CertificateValidation", "enabled"),
        "backupFolder": v.str("BackupFolder", "Backups"),
        "backupInterval": v.int("BackupInterval", 7),
        "backupRetention": v.int("BackupRetention", 28),
        "trustCgnatIpAddresses": h.trust_cgnat_ip_addresses,
        "id": 1,
    })))
}

pub async fn ui(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let v = Values::load(&state).await?;
    let mut out = serde_json::json!({
        "firstDayOfWeek": v.int("FirstDayOfWeek", 0),
        "calendarWeekColumnHeader": v.str("CalendarWeekColumnHeader", "ddd M/D"),
        "shortDateFormat": v.str("ShortDateFormat", "MMM D YYYY"),
        "longDateFormat": v.str("LongDateFormat", "dddd, MMMM D YYYY"),
        "timeFormat": v.str("TimeFormat", "h(:mm)a"),
        "showRelativeDates": v.bool("ShowRelativeDates", true),
        "enableColorImpairedMode": v.bool("EnableColorImpairedMode", false),
        "theme": state.settings().host.theme,
        "uiLanguage": v.int("UILanguage", 1),
        "id": 1,
    });
    // The originals omit an empty time zone.
    let tz = v.str("TimeZone", "");
    if !tz.is_empty() {
        out["timeZone"] = serde_json::Value::String(tz);
    }
    Ok(Json(out))
}

pub async fn download_client(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(serde_json::json!({
        "downloadClientWorkingFolders": v.str("DownloadClientWorkingFolders", "_UNPACK_|_FAILED_"),
        "enableCompletedDownloadHandling": v.bool("EnableCompletedDownloadHandling", true),
        "autoRedownloadFailed": v.bool("AutoRedownloadFailed", true),
        "autoRedownloadFailedFromInteractiveSearch": v.bool("AutoRedownloadFailedFromInteractiveSearch", true),
        "id": 1,
    })))
}

pub async fn import_list(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(serde_json::json!({
        "listSyncLevel": v.enum_name("ListSyncLevel", "disabled"),
        "listSyncTag": v.int("ListSyncTag", 0),
        "id": 1,
    })))
}
