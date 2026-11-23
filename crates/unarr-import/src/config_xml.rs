//! Parser for the original applications' `config.xml`.
//!
//! The file is a flat `<Config>` element whose children are the settings
//! Sonarr/Radarr/Lidarr persist outside the database: the HTTP listener, the
//! API key, authentication, logging, the URL base and the instance name.
//! Anything missing takes the same default the original would apply, so a
//! half-written file (Sonarr fills in elements lazily) imports cleanly.

use std::path::Path;

use anyhow::Context;
pub use unarr_core::config::{
    AuthenticationMethod, AuthenticationRequired, ConsoleLogFormat, HostConfig, LegacyUser,
};

/// Parse the text of a `config.xml`. `default_port` is the personality's
/// port, used when `<Port>` is absent.
pub fn parse(xml: &str, default_port: u16) -> anyhow::Result<HostConfig> {
    let doc = roxmltree::Document::parse(xml).context("config.xml is not well-formed XML")?;
    let root = doc.root_element();
    anyhow::ensure!(
        root.has_tag_name("Config"),
        "config.xml root element is <{}>, expected <Config>",
        root.tag_name().name()
    );

    let text = |name: &str| -> Option<String> {
        root.children()
            .find(|n| n.is_element() && n.has_tag_name(name))
            .map(|n| n.text().unwrap_or("").trim().to_string())
    };
    let string = |name: &str, default: &str| text(name).unwrap_or_else(|| default.to_string());
    let boolean = |name: &str, default: bool| -> anyhow::Result<bool> {
        match text(name) {
            None => Ok(default),
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                "" => Ok(default),
                other => anyhow::bail!("<{name}> is {other:?}, expected True or False"),
            },
        }
    };
    let port = |name: &str, default: u16| -> anyhow::Result<u16> {
        match text(name) {
            None => Ok(default),
            Some(v) if v.is_empty() => Ok(default),
            Some(v) => v
                .parse()
                .with_context(|| format!("<{name}> is {v:?}, expected a port")),
        }
    };

    let api_key = text("ApiKey").filter(|k| !k.is_empty());
    anyhow::ensure!(api_key.is_some(), "config.xml has no <ApiKey>");

    Ok(HostConfig {
        bind_address: string("BindAddress", "*"),
        port: port("Port", default_port)?,
        ssl_port: port("SslPort", 9898)?,
        enable_ssl: boolean("EnableSsl", false)?,
        launch_browser: boolean("LaunchBrowser", true)?,
        api_key: api_key.unwrap_or_default(),
        authentication_method: match text("AuthenticationMethod") {
            Some(v) if !v.is_empty() => AuthenticationMethod::parse(&v)
                .ok_or_else(|| anyhow::anyhow!("unknown AuthenticationMethod {v:?}"))?,
            // Sonarr v4 defaults to Forms; older files simply have no element.
            _ => AuthenticationMethod::Forms,
        },
        authentication_required: match text("AuthenticationRequired") {
            Some(v) if !v.is_empty() => AuthenticationRequired::parse(&v)
                .ok_or_else(|| anyhow::anyhow!("unknown AuthenticationRequired {v:?}"))?,
            _ => AuthenticationRequired::Enabled,
        },
        allowed_hosts: string("AllowedHosts", ""),
        trusted_networks: string("TrustedNetworks", ""),
        branch: string("Branch", "main"),
        log_level: string("LogLevel", "info"),
        ssl_cert_path: string("SslCertPath", ""),
        ssl_cert_password: string("SslCertPassword", ""),
        ssl_key_path: string("SslKeyPath", ""),
        oidc: unarr_core::config::OidcSettings {
            authority: string("OidcAuthority", ""),
            client_id: string("OidcClientId", ""),
            client_secret: string("OidcClientSecret", ""),
            user_identifier: string("OidcUserIdentifier", ""),
            scopes: string("OidcScopes", "openid profile email"),
        },
        url_base: string("UrlBase", ""),
        instance_name: text("InstanceName").filter(|s| !s.is_empty()),
        analytics_enabled: boolean("AnalyticsEnabled", true)?,
        theme: string("Theme", "auto"),
        trust_cgnat_ip_addresses: boolean("TrustCgnatIpAddresses", false)?,
        console_log_level: string("ConsoleLogLevel", ""),
        console_log_format: match text("ConsoleLogFormat") {
            Some(v) if !v.is_empty() => ConsoleLogFormat::parse(&v, true)
                .ok_or_else(|| anyhow::anyhow!("unknown ConsoleLogFormat {v:?}"))?,
            _ => ConsoleLogFormat::Standard,
        },
        // `UserService.Handle(ApplicationStartedEvent)`: both elements, or
        // nothing; their text as it is, the password in clear.
        legacy_user: {
            let raw = |name: &str| {
                root.children()
                    .find(|n| n.is_element() && n.has_tag_name(name))
                    .map(|n| n.text().unwrap_or("").to_string())
            };
            match (raw("Username"), raw("Password")) {
                (Some(username), Some(password)) => Some(LegacyUser { username, password }),
                _ => None,
            }
        },
    })
}

/// Read and parse `config.xml` from `path`.
pub fn read(path: &Path, default_port: u16) -> anyhow::Result<HostConfig> {
    let xml = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&xml, default_port)
}

/// Render a `config.xml` in the originals' format, for installations that
/// have none to import.
pub fn render(cfg: &HostConfig) -> String {
    fn bool_text(b: bool) -> &'static str {
        if b {
            "True"
        } else {
            "False"
        }
    }
    let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let method = match cfg.authentication_method {
        AuthenticationMethod::None => "None",
        AuthenticationMethod::Basic => "Basic",
        AuthenticationMethod::Forms => "Forms",
        AuthenticationMethod::External => "External",
    };
    let required = match cfg.authentication_required {
        AuthenticationRequired::Enabled => "Enabled",
        AuthenticationRequired::DisabledForLocalAddresses => "DisabledForLocalAddresses",
        AuthenticationRequired::DisabledForLocalhost => "DisabledForLocalhost",
    };
    format!(
        "<Config>\n  <BindAddress>{}</BindAddress>\n  <Port>{}</Port>\n  <SslPort>{}</SslPort>\n  <EnableSsl>{}</EnableSsl>\n  <LaunchBrowser>{}</LaunchBrowser>\n  <ApiKey>{}</ApiKey>\n  <AuthenticationMethod>{}</AuthenticationMethod>\n  <AuthenticationRequired>{}</AuthenticationRequired>\n  <Branch>{}</Branch>\n  <LogLevel>{}</LogLevel>\n  <SslCertPath>{}</SslCertPath>\n  <SslCertPassword>{}</SslCertPassword>\n  <UrlBase>{}</UrlBase>\n  <InstanceName>{}</InstanceName>\n  <AllowedHosts>{}</AllowedHosts>\n  <TrustedNetworks>{}</TrustedNetworks>\n  <AnalyticsEnabled>{}</AnalyticsEnabled>\n  <Theme>{}</Theme>\n{}</Config>\n",
        escape(&cfg.bind_address),
        cfg.port,
        cfg.ssl_port,
        bool_text(cfg.enable_ssl),
        bool_text(cfg.launch_browser),
        escape(&cfg.api_key),
        method,
        required,
        escape(&cfg.branch),
        escape(&cfg.log_level),
        escape(&cfg.ssl_cert_path),
        escape(&cfg.ssl_cert_password),
        escape(&cfg.url_base),
        escape(cfg.instance_name.as_deref().unwrap_or("")),
        escape(&cfg.allowed_hosts),
        escape(&cfg.trusted_networks),
        bool_text(cfg.analytics_enabled),
        escape(&cfg.theme),
        console_elements(cfg, &escape),
    )
}

/// `ConsoleLogLevel` and `ConsoleLogFormat`, which the originals do not
/// write until they are set (`persist: false`). The legacy user is never
/// written: the originals drop elements they do not know at start.
fn console_elements(cfg: &HostConfig, escape: &dyn Fn(&str) -> String) -> String {
    let mut out = String::new();
    if !cfg.console_log_level.is_empty() {
        out.push_str(&format!(
            "  <ConsoleLogLevel>{}</ConsoleLogLevel>\n",
            escape(&cfg.console_log_level)
        ));
    }
    if cfg.console_log_format != ConsoleLogFormat::Standard {
        out.push_str(&format!(
            "  <ConsoleLogFormat>{}</ConsoleLogFormat>\n",
            cfg.console_log_format.name()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/sonarr/config.xml"
    );

    #[test]
    fn parses_the_fixture() {
        let cfg = read(Path::new(FIXTURE), 8989).unwrap();
        assert_eq!(cfg.port, 8989);
        assert_eq!(cfg.ssl_port, 9898);
        assert!(!cfg.enable_ssl);
        assert!(cfg.launch_browser);
        assert_eq!(cfg.api_key, "0123456789abcdef0123456789abcdef");
        assert_eq!(cfg.authentication_method, AuthenticationMethod::Forms);
        assert_eq!(cfg.authentication_required, AuthenticationRequired::Enabled);
        assert_eq!(cfg.branch, "main");
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.url_base, "");
        assert_eq!(cfg.instance_name.as_deref(), Some("Sonarr"));
        assert_eq!(cfg.effective_instance_name("Sonarr"), "Sonarr");
        assert_eq!(cfg.allowed_hosts, "*");
        assert_eq!(cfg.trusted_networks, "");
        // The anonymised fixture comes from an installation with telemetry off.
        assert!(!cfg.analytics_enabled);
    }

    #[test]
    fn missing_elements_take_defaults() {
        let cfg = parse("<Config><ApiKey>k</ApiKey></Config>", 7878).unwrap();
        assert_eq!(cfg.port, 7878);
        assert_eq!(cfg.bind_address, "*");
        assert!(cfg.launch_browser);
        assert_eq!(cfg.authentication_method, AuthenticationMethod::Forms);
        assert_eq!(cfg.instance_name, None);
        assert_eq!(cfg.effective_instance_name("Radarr"), "Radarr");
    }

    #[test]
    fn honours_instance_names_that_keep_the_app_name() {
        let cfg = parse(
            "<Config><ApiKey>k</ApiKey><InstanceName>Sonarr 4K</InstanceName><UrlBase>/sonarr/</UrlBase></Config>",
            8989,
        )
        .unwrap();
        assert_eq!(cfg.effective_instance_name("Sonarr"), "Sonarr 4K");
        assert_eq!(cfg.normalized_url_base(), "/sonarr");
    }

    #[test]
    fn rejects_files_without_an_api_key() {
        assert!(parse("<Config><Port>1</Port></Config>", 8989).is_err());
        assert!(parse("<Nope/>", 8989).is_err());
        assert!(parse("not xml", 8989).is_err());
    }

    #[test]
    fn render_round_trips() {
        let cfg = read(Path::new(FIXTURE), 8989).unwrap();
        let again = parse(&render(&cfg), 8989).unwrap();
        assert_eq!(again, cfg);
        let mut console = cfg.clone();
        console.console_log_level = "warn".into();
        console.console_log_format = ConsoleLogFormat::Clef;
        let text = render(&console);
        assert!(text.contains("<ConsoleLogLevel>warn</ConsoleLogLevel>"), "{text}");
        assert_eq!(parse(&text, 8989).unwrap(), console);
        // Unset, as the originals leave them, they are not written.
        assert!(!render(&cfg).contains("ConsoleLog"));
    }

    /// The console's level and format, as the originals read them: the
    /// level as it is written, the format without regard to case.
    #[test]
    fn reads_the_console_settings() {
        let cfg = parse(
            "<Config><ApiKey>k</ApiKey><ConsoleLogLevel>Warn</ConsoleLogLevel><ConsoleLogFormat>clef</ConsoleLogFormat></Config>",
            8989,
        )
        .unwrap();
        assert_eq!(cfg.console_log_level, "Warn");
        assert_eq!(cfg.console_log_format, ConsoleLogFormat::Clef);
        let cfg = parse("<Config><ApiKey>k</ApiKey></Config>", 8989).unwrap();
        assert_eq!(
            (cfg.console_log_level.as_str(), cfg.console_log_format),
            ("", ConsoleLogFormat::Standard)
        );
        assert!(parse(
            "<Config><ApiKey>k</ApiKey><ConsoleLogFormat>Json</ConsoleLogFormat></Config>",
            8989
        )
        .is_err());
    }

    /// A file from before Sonarr v3: the user and the password in clear,
    /// both or nothing, and never written back.
    #[test]
    fn reads_the_user_of_a_file_from_before_v3() {
        let cfg = parse(
            "<Config><ApiKey>k</ApiKey><Username>Unarr</Username><Password> fixture </Password></Config>",
            8989,
        )
        .unwrap();
        assert_eq!(
            cfg.legacy_user,
            Some(LegacyUser {
                username: "Unarr".into(),
                password: " fixture ".into()
            })
        );
        assert!(!render(&cfg).contains("Username") && !render(&cfg).contains("fixture"));
        let half = parse(
            "<Config><ApiKey>k</ApiKey><Username>Unarr</Username></Config>",
            8989,
        )
        .unwrap();
        assert_eq!(half.legacy_user, None);
        assert_eq!(read(Path::new(FIXTURE), 8989).unwrap().legacy_user, None);
    }
}
