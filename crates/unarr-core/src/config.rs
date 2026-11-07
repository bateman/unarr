//! Host-level settings shared by the importer (which reads them from
//! `config.xml`) and the API (which reports them).

use serde::{Deserialize, Serialize};

/// `AuthenticationMethod` as the originals spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthenticationMethod {
    None,
    Basic,
    Forms,
    External,
}

impl AuthenticationMethod {
    /// camelCase name used by the v3 API (`authenticationMethod`, `authentication`).
    pub const fn api_name(self) -> &'static str {
        match self {
            AuthenticationMethod::None => "none",
            AuthenticationMethod::Basic => "basic",
            AuthenticationMethod::Forms => "forms",
            AuthenticationMethod::External => "external",
        }
    }

    /// Parse the spelling found in `config.xml` (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "none" => AuthenticationMethod::None,
            "basic" => AuthenticationMethod::Basic,
            "forms" => AuthenticationMethod::Forms,
            "external" => AuthenticationMethod::External,
            _ => return None,
        })
    }
}

/// `AuthenticationRequired`: when the UI asks for credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthenticationRequired {
    Enabled,
    DisabledForLocalAddresses,
    DisabledForLocalhost,
}

impl AuthenticationRequired {
    pub const fn api_name(self) -> &'static str {
        match self {
            AuthenticationRequired::Enabled => "enabled",
            AuthenticationRequired::DisabledForLocalAddresses => "disabledForLocalAddresses",
            AuthenticationRequired::DisabledForLocalhost => "disabledForLocalhost",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "enabled" => AuthenticationRequired::Enabled,
            "disabledforlocaladdresses" => AuthenticationRequired::DisabledForLocalAddresses,
            "disabledforlocalhost" => AuthenticationRequired::DisabledForLocalhost,
            _ => return None,
        })
    }
}

/// The host settings the originals keep in `config.xml`, with their defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfig {
    pub bind_address: String,
    pub port: u16,
    pub ssl_port: u16,
    pub enable_ssl: bool,
    pub launch_browser: bool,
    pub api_key: String,
    pub authentication_method: AuthenticationMethod,
    pub authentication_required: AuthenticationRequired,
    pub allowed_hosts: String,
    pub trusted_networks: String,
    pub branch: String,
    pub log_level: String,
    pub ssl_cert_path: String,
    pub ssl_cert_password: String,
    pub url_base: String,
    /// Raw value; see [`HostConfig::effective_instance_name`].
    pub instance_name: Option<String>,
    pub analytics_enabled: bool,
    pub theme: String,
}

impl HostConfig {
    /// The instance name the original would report. Sonarr only honours a
    /// custom name that starts or ends with the application name; anything
    /// else falls back to the application name itself.
    pub fn effective_instance_name(&self, app_name: &str) -> String {
        match &self.instance_name {
            Some(name) if name.starts_with(app_name) || name.ends_with(app_name) => name.clone(),
            _ => app_name.to_string(),
        }
    }

    /// `UrlBase` normalised as the originals do: a leading slash, no trailing
    /// slash, empty when unset.
    pub fn normalized_url_base(&self) -> String {
        let trimmed = self.url_base.trim().trim_matches('/');
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("/{trimmed}")
        }
    }
}
