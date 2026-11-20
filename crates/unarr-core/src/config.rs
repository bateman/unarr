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

/// `ConsoleLogFormat`: how the console writes its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleLogFormat {
    /// `[Info] Logger: message`, Unarr's own layout here.
    #[default]
    Standard,
    /// Compact log event format: one JSON object a line, for log shippers.
    Clef,
}

impl ConsoleLogFormat {
    /// The value as .NET's `Enum.Parse` reads it: a name, or its number.
    /// `config.xml` is read without regard to case, the environment with
    /// it (`Enum.TryParse`).
    pub fn parse(s: &str, ignore_case: bool) -> Option<Self> {
        let s = s.trim();
        let is = |name: &str| {
            if ignore_case {
                s.eq_ignore_ascii_case(name)
            } else {
                s == name
            }
        };
        if is("Standard") || s == "0" {
            Some(ConsoleLogFormat::Standard)
        } else if is("Clef") || s == "1" {
            Some(ConsoleLogFormat::Clef)
        } else {
            None
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            ConsoleLogFormat::Standard => "Standard",
            ConsoleLogFormat::Clef => "Clef",
        }
    }
}

/// NLog's levels, which `LogLevel` and `ConsoleLogLevel` name, from the
/// most verbose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Off,
}

impl LogLevel {
    /// `LogLevel.FromString`: the name, without regard to case, and the
    /// three other names the originals' NLog reads, as recorded on Sonarr
    /// with `Sonarr__Log__ConsoleLevel`: `Information`, `Warning`, `None`.
    /// `Critical` it does not read (the original then fails to start).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "trace" => LogLevel::Trace,
            "debug" => LogLevel::Debug,
            "info" | "information" => LogLevel::Info,
            "warn" | "warning" => LogLevel::Warn,
            "error" => LogLevel::Error,
            "fatal" => LogLevel::Fatal,
            "off" | "none" => LogLevel::Off,
            _ => return None,
        })
    }
}

/// The user name and password a `config.xml` from before Sonarr v3 holds
/// (`<Username>`, `<Password>`, the password in clear). The originals move
/// them into the Users table at start when it is empty.
#[derive(Clone, PartialEq, Eq)]
pub struct LegacyUser {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for LegacyUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyUser")
            .field("username", &self.username)
            .field("password", &"(removed)")
            .finish()
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
    /// `TrustCgnatIpAddresses`: `100.64.0.0/10` counts as local.
    pub trust_cgnat_ip_addresses: bool,
    /// `ConsoleLogLevel`: the console's own level; blank follows `LogLevel`
    /// (see [`HostConfig::console_level`]).
    pub console_log_level: String,
    /// `ConsoleLogFormat`.
    pub console_log_format: ConsoleLogFormat,
    /// `<Username>` and `<Password>` of a file from before Sonarr v3, read
    /// and never written back.
    pub legacy_user: Option<LegacyUser>,
}

impl HostConfig {
    /// The originals' environment overrides (`Sonarr__Server__Port`,
    /// `Sonarr__Auth__ApiKey`, `Sonarr__Log__Level`, ...): each wins over
    /// what `config.xml` and the saved settings say, for as long as the
    /// variable is set. `app` is the application's name, the first part of
    /// the variable; names compare without regard to case, as the
    /// originals' configuration keys do. A value that does not parse
    /// leaves the setting as it was.
    pub fn apply_environment(&mut self, app: &str, vars: impl IntoIterator<Item = (String, String)>) {
        let prefix = format!("{}__", app.to_ascii_lowercase());
        for (name, value) in vars {
            let name = name.to_ascii_lowercase();
            let Some(key) = name.strip_prefix(&prefix) else {
                continue;
            };
            let flag = || match value.trim().to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            let mut unusable = false;
            match key {
                "server__urlbase" => self.url_base = value.clone(),
                "server__bindaddress" => self.bind_address = value.clone(),
                "server__allowedhosts" => self.allowed_hosts = value.clone(),
                "server__trustednetworks" => self.trusted_networks = value.clone(),
                "server__sslcertpath" => self.ssl_cert_path = value.clone(),
                "server__sslcertpassword" => self.ssl_cert_password = value.clone(),
                "server__port" => match value.trim().parse() {
                    Ok(port) => self.port = port,
                    Err(_) => unusable = true,
                },
                "server__sslport" => match value.trim().parse() {
                    Ok(port) => self.ssl_port = port,
                    Err(_) => unusable = true,
                },
                "server__enablessl" => match flag() {
                    Some(on) => self.enable_ssl = on,
                    None => unusable = true,
                },
                // A blank key would open the API to a blank `apikey=`.
                "auth__apikey" => {
                    if value.trim().is_empty() {
                        unusable = true;
                    } else {
                        self.api_key = value.clone();
                    }
                }
                "auth__method" => match AuthenticationMethod::parse(&value) {
                    Some(method) => self.authentication_method = method,
                    None => unusable = true,
                },
                "auth__required" => match AuthenticationRequired::parse(&value) {
                    Some(required) => self.authentication_required = required,
                    None => unusable = true,
                },
                "auth__trustcgnatipaddresses" => match flag() {
                    Some(on) => self.trust_cgnat_ip_addresses = on,
                    None => unusable = true,
                },
                "app__instancename" => self.instance_name = Some(value.clone()),
                "app__theme" => self.theme = value.clone(),
                "app__launchbrowser" => match flag() {
                    Some(on) => self.launch_browser = on,
                    None => unusable = true,
                },
                "log__level" => self.log_level = value.trim().to_ascii_lowercase(),
                "log__consolelevel" => self.console_log_level = value.clone(),
                // `Enum.TryParse`: with regard to case; a value it does
                // not read leaves the file's.
                "log__consoleformat" => match ConsoleLogFormat::parse(&value, false) {
                    Some(format) => self.console_log_format = format,
                    None => unusable = true,
                },
                "log__analyticsenabled" => match flag() {
                    Some(on) => self.analytics_enabled = on,
                    None => unusable = true,
                },
                "update__branch" => self.branch = value.trim().to_ascii_lowercase(),
                _ => {}
            }
            if unusable {
                tracing::warn!("Environment variable {name} has a value that cannot be used; ignored");
            }
        }
    }

    /// `ReconfigureLogging`: the console's level. `ConsoleLogLevel` when
    /// set; else `LogLevel` when it is quieter than info; else info, so
    /// that the debug and trace levels of Settings > General reach the
    /// files and not the console. A `ConsoleLogLevel` that names no level
    /// counts as blank (see [`HostConfig::unreadable_console_level`]).
    pub fn console_level(&self) -> LogLevel {
        if let Some(level) = LogLevel::parse(&self.console_log_level) {
            return level;
        }
        LogLevel::parse(&self.log_level)
            .unwrap_or(LogLevel::Info)
            .max(LogLevel::Info)
    }

    /// A `ConsoleLogLevel` that is not blank and names no level: the
    /// original fails to configure its logging over it; Unarr says so and
    /// logs as if it were blank.
    pub fn unreadable_console_level(&self) -> Option<&str> {
        let value = self.console_log_level.trim();
        (!value.is_empty() && LogLevel::parse(value).is_none()).then_some(value)
    }

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

#[cfg(test)]
mod environment_tests {
    use super::*;

    fn host() -> HostConfig {
        HostConfig {
            bind_address: "*".into(),
            port: 8989,
            ssl_port: 9898,
            enable_ssl: false,
            launch_browser: true,
            api_key: "from-the-file".into(),
            authentication_method: AuthenticationMethod::Forms,
            authentication_required: AuthenticationRequired::Enabled,
            allowed_hosts: String::new(),
            trusted_networks: String::new(),
            branch: "main".into(),
            log_level: "info".into(),
            ssl_cert_path: String::new(),
            ssl_cert_password: String::new(),
            url_base: String::new(),
            instance_name: None,
            analytics_enabled: true,
            theme: "auto".into(),
            trust_cgnat_ip_addresses: false,
            console_log_level: String::new(),
            console_log_format: ConsoleLogFormat::Standard,
            legacy_user: None,
        }
    }

    /// `ReconfigureLogging`'s console level: its own when set, else the
    /// files' when that is quieter than info, else info.
    #[test]
    fn the_console_has_a_level_of_its_own() {
        let mut h = host();
        for (log, console, expected) in [
            ("debug", "", LogLevel::Info),
            ("trace", "", LogLevel::Info),
            ("info", "", LogLevel::Info),
            ("warn", "", LogLevel::Warn),
            ("info", "Trace", LogLevel::Trace),
            ("trace", "warn", LogLevel::Warn),
            ("debug", "off", LogLevel::Off),
            ("debug", "Critical", LogLevel::Info),
            ("debug", "loud", LogLevel::Info),
            ("debug", "Warning", LogLevel::Warn),
            ("trace", "Information", LogLevel::Info),
            ("debug", "None", LogLevel::Off),
        ] {
            h.log_level = log.into();
            h.console_log_level = console.into();
            assert_eq!(
                h.console_level(),
                expected,
                "LogLevel {log}, ConsoleLogLevel {console:?}"
            );
        }
        h.console_log_level = "loud".into();
        assert_eq!(h.unreadable_console_level(), Some("loud"));
        h.console_log_level = " ".into();
        assert_eq!(h.unreadable_console_level(), None);
    }

    #[test]
    fn the_console_takes_its_level_and_format_from_the_environment() {
        let mut h = host();
        h.apply_environment(
            "Sonarr",
            vars(&[
                ("SONARR__LOG__CONSOLELEVEL", "Debug"),
                ("Sonarr__Log__ConsoleFormat", "Clef"),
            ]),
        );
        assert_eq!(h.console_log_level, "Debug");
        assert_eq!(h.console_level(), LogLevel::Debug);
        assert_eq!(h.console_log_format, ConsoleLogFormat::Clef);
        // `Enum.TryParse` minds the case: the file's format stays.
        h.apply_environment("Sonarr", vars(&[("Sonarr__Log__ConsoleFormat", "standard")]));
        assert_eq!(h.console_log_format, ConsoleLogFormat::Clef);
        h.apply_environment("Sonarr", vars(&[("Sonarr__Log__ConsoleFormat", "0")]));
        assert_eq!(h.console_log_format, ConsoleLogFormat::Standard);
        assert_eq!(
            format!(
                "{:?}",
                LegacyUser {
                    username: "a".into(),
                    password: "secret".into()
                }
            ),
            "LegacyUser { username: \"a\", password: \"(removed)\" }"
        );
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let mut h = host();
        h.apply_environment(
            "Sonarr",
            vars(&[
                ("Sonarr__Server__Port", "7878"),
                ("SONARR__SERVER__URLBASE", "/tv"),
                ("sonarr__server__trustednetworks", "172.29.4.0/22"),
                ("Sonarr__Auth__ApiKey", "from-the-environment"),
                ("Sonarr__Auth__Method", "External"),
                ("Sonarr__Auth__Required", "DisabledForLocalAddresses"),
                ("Sonarr__Auth__TrustCgnatIpAddresses", "true"),
                ("Sonarr__App__InstanceName", "Sonarr (4K)"),
                ("Sonarr__Log__Level", "Trace"),
                ("Sonarr__Log__AnalyticsEnabled", "False"),
                // Another application's, and nothing of ours.
                ("Radarr__Server__Port", "1"),
                ("PATH", "/usr/bin"),
            ]),
        );
        assert_eq!(h.port, 7878);
        assert_eq!(h.url_base, "/tv");
        assert_eq!(h.trusted_networks, "172.29.4.0/22");
        assert_eq!(h.api_key, "from-the-environment");
        assert_eq!(h.authentication_method, AuthenticationMethod::External);
        assert_eq!(
            h.authentication_required,
            AuthenticationRequired::DisabledForLocalAddresses
        );
        assert!(h.trust_cgnat_ip_addresses);
        assert_eq!(h.instance_name.as_deref(), Some("Sonarr (4K)"));
        assert_eq!(h.log_level, "trace");
        assert!(!h.analytics_enabled);
    }

    #[test]
    fn a_value_that_does_not_parse_changes_nothing() {
        let mut h = host();
        h.apply_environment(
            "Sonarr",
            vars(&[
                ("Sonarr__Server__Port", "eighty"),
                ("Sonarr__Auth__Method", "Kerberos"),
                ("Sonarr__Server__EnableSsl", "yes"),
                // An unset variable of a compose file: no key at all.
                ("Sonarr__Auth__ApiKey", ""),
                ("SONARR__AUTH__APIKEY", "  "),
            ]),
        );
        assert_eq!(h, host());
    }
}
