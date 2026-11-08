//! The settings of the clients this phase drives, read from the stored JSON
//! (the schema's field names) and validated as the originals'
//! `*SettingsValidator`s do, in their order and with their messages.

use serde_json::{json, Value};

use super::{flag, int, text, ClientKind};
use crate::validation::ValidationFailure;

/// The decoded settings of a client.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientSettings {
    Sabnzbd(SabnzbdSettings),
    Nzbget(NzbgetSettings),
    QBittorrent(QBittorrentSettings),
    Transmission(TransmissionSettings),
}

impl ClientSettings {
    pub fn from_value(kind: ClientKind, v: &Value) -> ClientSettings {
        match kind {
            ClientKind::Sabnzbd => ClientSettings::Sabnzbd(SabnzbdSettings::from_value(v)),
            ClientKind::Nzbget => ClientSettings::Nzbget(NzbgetSettings::from_value(v)),
            ClientKind::QBittorrent => ClientSettings::QBittorrent(QBittorrentSettings::from_value(v)),
            ClientKind::Transmission => ClientSettings::Transmission(TransmissionSettings::from_value(v)),
        }
    }

    pub fn validate(&self) -> Vec<ValidationFailure> {
        match self {
            ClientSettings::Sabnzbd(s) => s.validate(),
            ClientSettings::Nzbget(s) => s.validate(),
            ClientSettings::QBittorrent(s) => s.validate(),
            ClientSettings::Transmission(s) => s.validate(),
        }
    }

    pub fn host(&self) -> &str {
        match self {
            ClientSettings::Sabnzbd(s) => &s.host,
            ClientSettings::Nzbget(s) => &s.host,
            ClientSettings::QBittorrent(s) => &s.host,
            ClientSettings::Transmission(s) => &s.host,
        }
    }

    /// The JSON the store keeps, under the schema's field names.
    pub fn to_value(&self) -> Value {
        match self {
            ClientSettings::Sabnzbd(s) => s.to_value(),
            ClientSettings::Nzbget(s) => s.to_value(),
            ClientSettings::QBittorrent(s) => s.to_value(),
            ClientSettings::Transmission(s) => s.to_value(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SabnzbdSettings {
    pub host: String,
    pub port: i64,
    pub use_ssl: bool,
    pub url_base: String,
    pub api_key: String,
    pub username: String,
    pub password: String,
    pub tv_category: String,
    /// `SabnzbdPriority`: Default -100, Paused -2, Low -1, Normal 0, High 1, Force 2.
    pub recent_tv_priority: i64,
    pub older_tv_priority: i64,
}

impl Default for SabnzbdSettings {
    fn default() -> Self {
        SabnzbdSettings {
            host: "localhost".into(),
            port: 8080,
            use_ssl: false,
            url_base: String::new(),
            api_key: String::new(),
            username: String::new(),
            password: String::new(),
            tv_category: "tv".into(),
            recent_tv_priority: -100,
            older_tv_priority: -100,
        }
    }
}

impl SabnzbdSettings {
    pub fn from_value(v: &Value) -> SabnzbdSettings {
        let d = SabnzbdSettings::default();
        SabnzbdSettings {
            host: text_or(v, "host", &d.host),
            port: int(v, "port", d.port),
            use_ssl: flag(v, "useSsl", d.use_ssl),
            url_base: text(v, "urlBase"),
            api_key: text(v, "apiKey"),
            username: text(v, "username"),
            password: text(v, "password"),
            tv_category: text_or(v, "tvCategory", &d.tv_category),
            recent_tv_priority: int(v, "recentTvPriority", d.recent_tv_priority),
            older_tv_priority: int(v, "olderTvPriority", d.older_tv_priority),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "host": self.host, "port": self.port, "useSsl": self.use_ssl, "urlBase": nullable(&self.url_base),
            "apiKey": nullable(&self.api_key), "username": nullable(&self.username), "password": nullable(&self.password),
            "tvCategory": self.tv_category, "recentTvPriority": self.recent_tv_priority, "olderTvPriority": self.older_tv_priority,
        })
    }

    pub fn validate(&self) -> Vec<ValidationFailure> {
        let mut f = valid_host(&self.host);
        f.extend(valid_port(self.port));
        f.extend(valid_url_base(&self.url_base));
        if blank(&self.username) && blank(&self.api_key) {
            f.push(not_empty_with(
                "ApiKey",
                "API Key is required when username/password are not configured",
                &self.api_key,
            ));
        }
        if blank(&self.api_key) {
            if blank(&self.username) {
                f.push(not_empty_with(
                    "Username",
                    "Username is required when API key is not configured",
                    &self.username,
                ));
            }
            if blank(&self.password) {
                f.push(not_empty_with(
                    "Password",
                    "Password is required when API key is not configured",
                    &self.password,
                ));
            }
        }
        if blank(&self.tv_category) {
            f.push(category_recommended(&self.tv_category));
        }
        f
    }

    /// `priority` for a release, by whether an episode is recent.
    pub fn priority(&self, recent: bool) -> i64 {
        if recent {
            self.recent_tv_priority
        } else {
            self.older_tv_priority
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NzbgetSettings {
    pub host: String,
    pub port: i64,
    pub use_ssl: bool,
    pub url_base: String,
    pub username: String,
    pub password: String,
    pub tv_category: String,
    /// `NzbgetPriority`: VeryLow -100, Low -50, Normal 0, High 50, VeryHigh 100, Force 900.
    pub recent_tv_priority: i64,
    pub older_tv_priority: i64,
    pub add_paused: bool,
}

impl Default for NzbgetSettings {
    fn default() -> Self {
        NzbgetSettings {
            host: "localhost".into(),
            port: 6789,
            use_ssl: false,
            url_base: String::new(),
            username: String::new(),
            password: String::new(),
            tv_category: "tv".into(),
            recent_tv_priority: 0,
            older_tv_priority: 0,
            add_paused: false,
        }
    }
}

impl NzbgetSettings {
    pub fn from_value(v: &Value) -> NzbgetSettings {
        let d = NzbgetSettings::default();
        NzbgetSettings {
            host: text_or(v, "host", &d.host),
            port: int(v, "port", d.port),
            use_ssl: flag(v, "useSsl", d.use_ssl),
            url_base: text(v, "urlBase"),
            username: text(v, "username"),
            password: text(v, "password"),
            tv_category: text_or(v, "tvCategory", &d.tv_category),
            recent_tv_priority: int(v, "recentTvPriority", d.recent_tv_priority),
            older_tv_priority: int(v, "olderTvPriority", d.older_tv_priority),
            add_paused: flag(v, "addPaused", d.add_paused),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "host": self.host, "port": self.port, "useSsl": self.use_ssl, "urlBase": nullable(&self.url_base),
            "username": nullable(&self.username), "password": nullable(&self.password), "tvCategory": self.tv_category,
            "recentTvPriority": self.recent_tv_priority, "olderTvPriority": self.older_tv_priority, "addPaused": self.add_paused,
        })
    }

    pub fn validate(&self) -> Vec<ValidationFailure> {
        let mut f = valid_host(&self.host);
        f.extend(valid_port(self.port));
        f.extend(valid_url_base(&self.url_base));
        if !blank(&self.password) && blank(&self.username) {
            f.push(ValidationFailure::setting_not_empty(
                "Username",
                json!(self.username),
            ));
        }
        if !blank(&self.username) && blank(&self.password) {
            f.push(ValidationFailure::setting_not_empty(
                "Password",
                json!(self.password),
            ));
        }
        if blank(&self.tv_category) {
            f.push(category_recommended(&self.tv_category));
        }
        f
    }

    pub fn priority(&self, recent: bool) -> i64 {
        if recent {
            self.recent_tv_priority
        } else {
            self.older_tv_priority
        }
    }
}

/// `QBittorrentPriority`: Last 0, First 1.
pub const QBITTORRENT_PRIORITY_FIRST: i64 = 1;
/// `QBittorrentState`: Start 0, ForceStart 1, Stop 2.
pub const QBITTORRENT_STATE_START: i64 = 0;
pub const QBITTORRENT_STATE_FORCE_START: i64 = 1;
pub const QBITTORRENT_STATE_STOP: i64 = 2;
/// `QBittorrentContentLayout`: Default 0, Original 1, Subfolder 2.
pub const QBITTORRENT_LAYOUT_ORIGINAL: i64 = 1;
pub const QBITTORRENT_LAYOUT_SUBFOLDER: i64 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct QBittorrentSettings {
    pub host: String,
    pub port: i64,
    pub use_ssl: bool,
    pub url_base: String,
    pub api_key: String,
    pub username: String,
    pub password: String,
    pub tv_category: String,
    pub tv_imported_category: String,
    pub recent_tv_priority: i64,
    pub older_tv_priority: i64,
    pub initial_state: i64,
    pub sequential_order: bool,
    pub first_and_last: bool,
    pub content_layout: i64,
}

impl Default for QBittorrentSettings {
    fn default() -> Self {
        QBittorrentSettings {
            host: "localhost".into(),
            port: 8080,
            use_ssl: false,
            url_base: String::new(),
            api_key: String::new(),
            username: String::new(),
            password: String::new(),
            tv_category: "tv-sonarr".into(),
            tv_imported_category: String::new(),
            recent_tv_priority: 0,
            older_tv_priority: 0,
            initial_state: 0,
            sequential_order: false,
            first_and_last: false,
            content_layout: 0,
        }
    }
}

impl QBittorrentSettings {
    pub fn from_value(v: &Value) -> QBittorrentSettings {
        let d = QBittorrentSettings::default();
        QBittorrentSettings {
            host: text_or(v, "host", &d.host),
            port: int(v, "port", d.port),
            use_ssl: flag(v, "useSsl", d.use_ssl),
            url_base: text(v, "urlBase"),
            api_key: text(v, "apiKey"),
            username: text(v, "username"),
            password: text(v, "password"),
            tv_category: text_or(v, "tvCategory", &d.tv_category),
            tv_imported_category: text(v, "tvImportedCategory"),
            recent_tv_priority: int(v, "recentTvPriority", d.recent_tv_priority),
            older_tv_priority: int(v, "olderTvPriority", d.older_tv_priority),
            initial_state: int(v, "initialState", d.initial_state),
            sequential_order: flag(v, "sequentialOrder", d.sequential_order),
            first_and_last: flag(v, "firstAndLast", d.first_and_last),
            content_layout: int(v, "contentLayout", d.content_layout),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "host": self.host, "port": self.port, "useSsl": self.use_ssl, "urlBase": nullable(&self.url_base),
            "apiKey": nullable(&self.api_key), "username": nullable(&self.username), "password": nullable(&self.password),
            "tvCategory": self.tv_category, "tvImportedCategory": nullable(&self.tv_imported_category),
            "recentTvPriority": self.recent_tv_priority, "olderTvPriority": self.older_tv_priority,
            "initialState": self.initial_state, "sequentialOrder": self.sequential_order,
            "firstAndLast": self.first_and_last, "contentLayout": self.content_layout,
        })
    }

    pub fn validate(&self) -> Vec<ValidationFailure> {
        let mut f = valid_host(&self.host);
        f.extend(valid_port(self.port));
        f.extend(valid_url_base(&self.url_base));
        if !blank(&self.api_key) {
            if !blank(&self.username) {
                f.push(ValidationFailure::setting(
                    "Username",
                    "Username must be empty when using API Key.",
                    json!(self.username),
                ));
            }
            if !blank(&self.password) {
                f.push(ValidationFailure::setting(
                    "Password",
                    "Password must be empty when using API Key.",
                    json!(self.password),
                ));
            }
        }
        for (name, value) in [
            ("TvCategory", &self.tv_category),
            ("TvImportedCategory", &self.tv_imported_category),
        ] {
            if !valid_qbittorrent_category(value) {
                f.push(ValidationFailure::setting(
                    name,
                    "Can not contain '\\', '//', or start/end with '/'",
                    json!(value),
                ));
            }
        }
        f
    }

    /// Whether a release goes to the top of the queue.
    pub fn move_to_top(&self, recent: bool) -> bool {
        let priority = if recent {
            self.recent_tv_priority
        } else {
            self.older_tv_priority
        };
        priority == QBITTORRENT_PRIORITY_FIRST
    }
}

/// `^([^\\\/](\/?[^\\\/])*)?$`: no backslash, no `//`, no leading or
/// trailing slash.
fn valid_qbittorrent_category(value: &str) -> bool {
    value.is_empty()
        || (!value.contains('\\')
            && !value.contains("//")
            && !value.starts_with('/')
            && !value.ends_with('/'))
}

/// `TransmissionPriority`: Last 0, First 1.
pub const TRANSMISSION_PRIORITY_FIRST: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct TransmissionSettings {
    pub host: String,
    pub port: i64,
    pub use_ssl: bool,
    pub url_base: String,
    pub username: String,
    pub password: String,
    pub tv_category: String,
    pub tv_imported_category: String,
    pub tv_directory: String,
    pub recent_tv_priority: i64,
    pub older_tv_priority: i64,
    pub add_paused: bool,
}

impl Default for TransmissionSettings {
    fn default() -> Self {
        TransmissionSettings {
            host: "localhost".into(),
            port: 9091,
            use_ssl: false,
            url_base: "/transmission/".into(),
            username: String::new(),
            password: String::new(),
            tv_category: "tv-sonarr".into(),
            tv_imported_category: String::new(),
            tv_directory: String::new(),
            recent_tv_priority: 0,
            older_tv_priority: 0,
            add_paused: false,
        }
    }
}

impl TransmissionSettings {
    pub fn from_value(v: &Value) -> TransmissionSettings {
        let d = TransmissionSettings::default();
        TransmissionSettings {
            host: text_or(v, "host", &d.host),
            port: int(v, "port", d.port),
            use_ssl: flag(v, "useSsl", d.use_ssl),
            url_base: text_or(v, "urlBase", &d.url_base),
            username: text(v, "username"),
            password: text(v, "password"),
            tv_category: text_or(v, "tvCategory", &d.tv_category),
            tv_imported_category: text(v, "tvImportedCategory"),
            tv_directory: text(v, "tvDirectory"),
            recent_tv_priority: int(v, "recentTvPriority", d.recent_tv_priority),
            older_tv_priority: int(v, "olderTvPriority", d.older_tv_priority),
            add_paused: flag(v, "addPaused", d.add_paused),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "host": self.host, "port": self.port, "useSsl": self.use_ssl, "urlBase": self.url_base,
            "username": nullable(&self.username), "password": nullable(&self.password),
            "tvCategory": self.tv_category, "tvImportedCategory": nullable(&self.tv_imported_category),
            "tvDirectory": nullable(&self.tv_directory), "recentTvPriority": self.recent_tv_priority,
            "olderTvPriority": self.older_tv_priority, "addPaused": self.add_paused,
        })
    }

    pub fn validate(&self) -> Vec<ValidationFailure> {
        let mut f = valid_host(&self.host);
        f.extend(valid_port(self.port));
        f.extend(valid_url_base(&self.url_base));
        let category_ok = {
            let rest = self.tv_category.strip_prefix('.').unwrap_or(&self.tv_category);
            rest.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
        };
        if !category_ok {
            f.push(ValidationFailure::setting(
                "TvCategory",
                "Allowed characters a-z and -",
                json!(self.tv_category),
            ));
        }
        if !blank(&self.tv_directory) && !blank(&self.tv_category) {
            f.push(ValidationFailure::setting(
                "TvCategory",
                "Cannot use Category and Directory",
                json!(self.tv_category),
            ));
        }
        f
    }

    pub fn move_to_top(&self, recent: bool) -> bool {
        let priority = if recent {
            self.recent_tv_priority
        } else {
            self.older_tv_priority
        };
        priority == TRANSMISSION_PRIORITY_FIRST
    }
}

// --- shared rules

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn text_or(v: &Value, key: &str, default: &str) -> String {
    match &v[key] {
        Value::Null => default.to_string(),
        _ => text(v, key),
    }
}

fn nullable(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        json!(s)
    }
}

/// `ValidHost`: not empty, then a host name or IP address without a scheme.
fn valid_host(host: &str) -> Vec<ValidationFailure> {
    let mut f = Vec::new();
    if blank(host) {
        f.push(ValidationFailure::setting_not_empty("Host", json!(host)));
    }
    let name_ok = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    let ip_ok = host.parse::<std::net::IpAddr>().is_ok();
    if !name_ok && !ip_ok {
        f.push(ValidationFailure::setting(
            "Host",
            "must be valid Host without http://",
            json!(host),
        ));
    }
    f
}

/// `InclusiveBetween(1, 65535)`.
fn valid_port(port: i64) -> Option<ValidationFailure> {
    if (1..=65535).contains(&port) {
        return None;
    }
    Some(ValidationFailure::setting(
        "Port",
        &format!("'Port' must be between 1 and 65535. You entered {port}."),
        json!(port),
    ))
}

/// `ValidUrlBase`: a path, not a URL (`^(?!\/?https?://[-_a-z0-9.]+)`).
fn valid_url_base(url_base: &str) -> Option<ValidationFailure> {
    if blank(url_base) {
        return None;
    }
    let lower = url_base.to_ascii_lowercase();
    let rest = lower.strip_prefix('/').unwrap_or(&lower);
    let looks_like_url = ["http://", "https://"].iter().any(|scheme| {
        rest.strip_prefix(scheme)
            .and_then(|after| after.chars().next())
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    });
    looks_like_url.then(|| {
        ValidationFailure::setting(
            "UrlBase",
            "Must be a valid URL path (ie: '/sonarr')",
            json!(url_base),
        )
    })
}

fn not_empty_with(property: &str, message: &str, value: &str) -> ValidationFailure {
    ValidationFailure::setting(property, message, json!(value))
}

/// `TvCategory NotEmpty "A category is recommended" AsWarning`.
fn category_recommended(value: &str) -> ValidationFailure {
    let mut f = not_empty_with("TvCategory", "A category is recommended", value);
    f.is_warning = true;
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sabnzbd_requires_a_key_or_credentials() {
        let s =
            SabnzbdSettings::from_value(&json!({"host": "127.0.0.1", "port": 18989, "urlBase": "/sabnzbd"}));
        let messages: Vec<_> = s.validate().iter().map(|f| f.error_message.clone()).collect();
        assert_eq!(
            messages,
            [
                "API Key is required when username/password are not configured",
                "Username is required when API key is not configured",
                "Password is required when API key is not configured",
            ]
        );
        let ok = SabnzbdSettings::from_value(&json!({"host": "nas", "port": 1, "apiKey": "k"}));
        assert!(ok.validate().is_empty());
        let warn = SabnzbdSettings::from_value(&json!({"host": "nas", "apiKey": "k", "tvCategory": ""}));
        let f = warn.validate();
        assert_eq!(f.len(), 1);
        assert!(f[0].is_warning);
    }

    #[test]
    fn hosts_ports_and_url_bases_are_checked() {
        let s = NzbgetSettings::from_value(
            &json!({"host": "http://x", "port": 0, "urlBase": "/http://evil.example"}),
        );
        let props: Vec<_> = s.validate().iter().map(|f| f.property_name.clone()).collect();
        assert_eq!(props, ["Host", "Port", "UrlBase"]);
        let empty = NzbgetSettings::from_value(&json!({"host": "", "username": "u"}));
        let codes: Vec<_> = empty.validate().iter().map(|f| f.property_name.clone()).collect();
        assert_eq!(codes, ["Host", "Host", "Password"]);
        assert!(NzbgetSettings::from_value(&json!({"host": "::1"}))
            .validate()
            .is_empty());
    }

    #[test]
    fn torrent_categories_follow_their_patterns() {
        let q = QBittorrentSettings::from_value(
            &json!({"host": "q", "apiKey": "k", "username": "u", "tvCategory": "/tv"}),
        );
        let props: Vec<_> = q.validate().iter().map(|f| f.property_name.clone()).collect();
        assert_eq!(props, ["Username", "TvCategory"]);
        assert!(
            QBittorrentSettings::from_value(&json!({"host": "q", "tvCategory": "tv/sonarr"}))
                .validate()
                .is_empty()
        );
        let t = TransmissionSettings::from_value(
            &json!({"host": "t", "tvCategory": "tv_1", "tvDirectory": "/x"}),
        );
        let messages: Vec<_> = t.validate().iter().map(|f| f.error_message.clone()).collect();
        assert_eq!(
            messages,
            [
                "Allowed characters a-z and -",
                "Cannot use Category and Directory"
            ]
        );
        assert!(
            TransmissionSettings::from_value(&json!({"host": "t", "tvCategory": ".tv-sonarr"}))
                .validate()
                .is_empty()
        );
    }
}
