//! Parser for the original applications' `config.xml`.
//!
//! The file is a flat `<Config>` element whose children are the settings
//! Sonarr/Radarr/Lidarr persist outside the database: the HTTP listener, the
//! API key, authentication, logging, the URL base and the instance name.
//! Anything missing takes the same default the original would apply, so a
//! half-written file (Sonarr fills in elements lazily) imports cleanly.

use std::path::Path;

use anyhow::Context;
pub use unarr_core::config::{AuthenticationMethod, AuthenticationRequired, HostConfig};

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
        url_base: string("UrlBase", ""),
        instance_name: text("InstanceName").filter(|s| !s.is_empty()),
        analytics_enabled: boolean("AnalyticsEnabled", true)?,
        theme: string("Theme", "auto"),
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
        "<Config>\n  <BindAddress>{}</BindAddress>\n  <Port>{}</Port>\n  <SslPort>{}</SslPort>\n  <EnableSsl>{}</EnableSsl>\n  <LaunchBrowser>{}</LaunchBrowser>\n  <ApiKey>{}</ApiKey>\n  <AuthenticationMethod>{}</AuthenticationMethod>\n  <AuthenticationRequired>{}</AuthenticationRequired>\n  <Branch>{}</Branch>\n  <LogLevel>{}</LogLevel>\n  <SslCertPath>{}</SslCertPath>\n  <SslCertPassword>{}</SslCertPassword>\n  <UrlBase>{}</UrlBase>\n  <InstanceName>{}</InstanceName>\n  <AllowedHosts>{}</AllowedHosts>\n  <TrustedNetworks>{}</TrustedNetworks>\n  <AnalyticsEnabled>{}</AnalyticsEnabled>\n  <Theme>{}</Theme>\n</Config>\n",
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
    )
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
    }
}
