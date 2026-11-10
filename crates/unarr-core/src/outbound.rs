//! What Settings > General says about outgoing connections: the proxy and
//! how strictly certificates are checked.
//!
//! The originals read both for every request (`HttpProxySettingsProvider`,
//! `X509CertificateValidationService`), so a save takes effect at once. The
//! client here is built once; it asks this module per request which proxy
//! a URL goes through and, when a certificate fails, whether that is to be
//! let through.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{
    ClientSessionMemoryCache, ClientSessionStore, Tls12ClientSessionValue, Tls13ClientSessionValue,
};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// `ProxyType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyType {
    Http,
    Socks4,
    Socks5,
}

/// `CertificateValidationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CertificateValidation {
    #[default]
    Enabled,
    DisabledForLocalAddresses,
    Disabled,
}

/// `HttpProxySettings`, with the bypass list already read.
#[derive(Debug, Clone)]
pub struct Proxy {
    kind: ProxyType,
    host: String,
    port: i64,
    username: String,
    password: String,
    bypass_local: bool,
    /// The list's entries as the regular expressions `WebProxy` makes of
    /// them, matched against `scheme://host[:port]`.
    patterns: Vec<fancy_regex::Regex>,
    /// The entries that are an address, a range or a network.
    ranges: Vec<(u128, u128, bool)>,
}

impl Proxy {
    pub fn new(
        kind: ProxyType,
        host: &str,
        port: i64,
        username: &str,
        password: &str,
        bypass_filter: &str,
        bypass_local: bool,
    ) -> Proxy {
        let mut patterns = Vec::new();
        let mut ranges = Vec::new();
        for entry in bypass_filter.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if let Some(range) = parse_range(entry) {
                ranges.push(range);
            }
            // `BypassListAsArray`: a leading `*` would not be an
            // expression; the originals put a `;` before it.
            let expression = if entry.starts_with('*') {
                format!(";{entry}")
            } else {
                entry.to_string()
            };
            match fancy_regex::Regex::new(&format!("(?i){expression}")) {
                Ok(pattern) => patterns.push(pattern),
                Err(e) => tracing::warn!("Proxy bypass entry '{entry}' is not usable: {e}"),
            }
        }
        Proxy {
            kind,
            host: if host.trim().is_empty() {
                "127.0.0.1".to_string()
            } else {
                host.trim().to_string()
            },
            port,
            username: username.to_string(),
            password: password.to_string(),
            bypass_local,
            patterns,
            ranges,
        }
    }

    /// `ManagedWebProxyFactory.GetProxyUri`, with the credentials when both
    /// are set. SOCKS5 leaves the name to the proxy to resolve, SOCKS4
    /// resolves it here, as the originals' runtime does.
    fn url(&self) -> Option<reqwest::Url> {
        let scheme = match self.kind {
            ProxyType::Http => "http",
            ProxyType::Socks4 => "socks4",
            ProxyType::Socks5 => "socks5h",
        };
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let mut url = reqwest::Url::parse(&format!("{scheme}://{host}:{}", self.port)).ok()?;
        if !self.username.trim().is_empty() && !self.password.trim().is_empty() {
            url.set_username(&self.username).ok()?;
            url.set_password(Some(&self.password)).ok()?;
        }
        Some(url)
    }

    /// `ShouldProxyBeBypassed`: `WebProxy.IsBypassed`, or an address the
    /// list names by range.
    fn bypassed(&self, url: &reqwest::Url) -> bool {
        let Some(host) = url.host_str() else {
            return true;
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if self.bypass_local && is_local_host(host) {
            return true;
        }
        // The port only when it is not the scheme's own.
        let target = match url.port() {
            Some(port) => format!("{}://{}:{port}", url.scheme(), url.host_str().unwrap_or(host)),
            None => format!("{}://{}", url.scheme(), url.host_str().unwrap_or(host)),
        };
        if self.patterns.iter().any(|p| p.is_match(&target).unwrap_or(false)) {
            return true;
        }
        host.parse::<IpAddr>().is_ok_and(|address| {
            let (value, v4) = numeric(address);
            self.ranges
                .iter()
                .any(|(from, to, range_v4)| *range_v4 == v4 && *from <= value && value <= *to)
        })
    }
}

/// `WebProxy.IsLocal`, which "Bypass Proxy for Local Addresses" means: the
/// loopback, an address of this machine, or a name without a dot. Not the
/// private networks: those go in the list.
fn is_local_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        // An address of this machine is one a socket can be bound to.
        Ok(address) => address.is_loopback() || std::net::UdpSocket::bind((address, 0)).is_ok(),
        Err(_) => !host.contains('.'),
    }
}

fn numeric(address: IpAddr) -> (u128, bool) {
    match address {
        IpAddr::V4(v4) => (u32::from(v4) as u128, true),
        IpAddr::V6(v6) => (u128::from(v6), false),
    }
}

/// `IPAddressRange.TryParse`: an address, `network/prefix`, `network/mask`
/// or `first-last`.
fn parse_range(entry: &str) -> Option<(u128, u128, bool)> {
    if let Some((network, suffix)) = entry.split_once('/') {
        let (base, v4) = numeric(network.trim().parse().ok()?);
        let bits: u32 = if v4 { 32 } else { 128 };
        let prefix = match suffix.trim().parse::<u32>() {
            Ok(prefix) if prefix <= bits => prefix,
            Ok(_) => return None,
            Err(_) => {
                // A mask: its leading ones.
                let mask: Ipv4Addr = suffix.trim().parse().ok()?;
                let mask = u32::from(mask);
                if !v4 || mask.leading_ones() + mask.trailing_zeros() != 32 {
                    return None;
                }
                mask.leading_ones()
            }
        };
        let host_bits = bits - prefix;
        let span: u128 = if host_bits == 128 {
            u128::MAX
        } else {
            (1u128 << host_bits) - 1
        };
        let first = base & !span;
        return Some((first, first | span, v4));
    }
    if let Some((first, last)) = entry.split_once(['-', '\u{2013}']) {
        let (first, v4) = numeric(first.trim().parse().ok()?);
        let (last, last_v4) = numeric(last.trim().parse().ok()?);
        return (v4 == last_v4 && first <= last).then_some((first, last, v4));
    }
    let (value, v4) = numeric(entry.parse().ok()?);
    Some((value, value, v4))
}

/// `IPAddressExtensions.IsLocalAddress`: the loopback, the private and
/// link-local IPv4 networks, the link-local, unique-local and site-local
/// IPv6 ones.
pub fn is_local_address(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match address {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6 == Ipv6Addr::LOCALHOST
                || (first & 0xffc0) == 0xfe80
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfec0
        }
    }
}

/// `IPAddressExtensions.IsCgnatIpAddress`: `100.64.0.0/10`, the range
/// carriers and overlay networks (Tailscale) hand out.
pub fn is_cgnat_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]),
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .is_some_and(|v4| is_cgnat_address(IpAddr::V4(v4))),
    }
}

/// The settings as they stand.
#[derive(Debug, Clone, Default)]
pub struct Outbound {
    pub proxy: Option<Proxy>,
    pub certificate_validation: CertificateValidation,
}

impl Outbound {
    /// The settings as the configuration table holds them
    /// (`ConfigService`'s `Proxy…` and `CertificateValidation`), with the
    /// originals' defaults. An enumeration is stored by name; an older
    /// database may hold its number.
    pub fn load(conn: &rusqlite::Connection) -> anyhow::Result<Outbound> {
        let values: std::collections::HashMap<String, String> =
            crate::store::config::all_config(conn)?.into_iter().collect();
        let text = |key: &str| values.get(key).cloned().unwrap_or_default();
        let flag = |key: &str, default: bool| match values.get(key) {
            Some(v) => v.eq_ignore_ascii_case("true"),
            None => default,
        };
        let certificate_validation = match text("certificatevalidation").to_ascii_lowercase().as_str() {
            "disabledforlocaladdresses" | "1" => CertificateValidation::DisabledForLocalAddresses,
            "disabled" | "2" => CertificateValidation::Disabled,
            _ => CertificateValidation::Enabled,
        };
        let proxy = flag("proxyenabled", false).then(|| {
            let kind = match text("proxytype").to_ascii_lowercase().as_str() {
                "socks4" | "1" => ProxyType::Socks4,
                "socks5" | "2" => ProxyType::Socks5,
                _ => ProxyType::Http,
            };
            Proxy::new(
                kind,
                &text("proxyhostname"),
                text("proxyport").parse().unwrap_or(8080),
                &text("proxyusername"),
                &text("proxypassword"),
                &text("proxybypassfilter"),
                flag("proxybypasslocaladdresses", true),
            )
        });
        Ok(Outbound {
            proxy,
            certificate_validation,
        })
    }
}

/// The settings a client reads for every request, and whoever saves them
/// replaces.
#[derive(Debug, Clone, Default)]
pub struct Handle(Arc<Shared>);

#[derive(Debug, Default)]
struct Shared {
    settings: RwLock<Outbound>,
    /// Counts the saves: what was remembered under earlier settings (the
    /// TLS sessions) is not to be used under these.
    generation: AtomicU64,
}

impl Handle {
    pub fn set(&self, outbound: Outbound) {
        *self.0.settings.write().unwrap_or_else(|e| e.into_inner()) = outbound;
        self.0.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn get(&self) -> Outbound {
        self.0.settings.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `GetProxySettings(uri)`: the proxy `url` goes through, if any.
    pub fn proxy_for(&self, url: &reqwest::Url) -> Option<reqwest::Url> {
        let proxy = self.get().proxy?;
        if proxy.bypassed(url) {
            return None;
        }
        proxy.url()
    }

    /// `ShouldByPassValidationError`, for a certificate that failed.
    fn lets_through(&self, host: &str) -> bool {
        if host == "localhost" || host == "127.0.0.1" {
            return true;
        }
        match self.get().certificate_validation {
            CertificateValidation::Disabled => true,
            CertificateValidation::Enabled => false,
            CertificateValidation::DisabledForLocalAddresses => match host.parse::<IpAddr>() {
                Ok(address) => is_local_address(address),
                // `Dns.GetHostEntry`: every address the name has.
                Err(_) => (host, 0)
                    .to_socket_addrs()
                    .is_ok_and(|mut addresses| addresses.all(|a| is_local_address(a.ip()))),
            },
        }
    }

    /// The TLS configuration of a client that follows these settings: the
    /// platform's verification, and this handle asked when it fails.
    pub fn tls_config(&self) -> Result<rustls::ClientConfig, rustls::Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let platform = rustls_platform_verifier::Verifier::new(provider.clone())?;
        let verifier = Lenient {
            platform: Arc::new(platform),
            settings: self.clone(),
        };
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        config.resumption = rustls::client::Resumption::store(Arc::new(Sessions {
            settings: self.clone(),
            cache: Mutex::new((0, Arc::new(ClientSessionMemoryCache::new(256)))),
        }));
        Ok(config)
    }
}

/// The TLS sessions kept for resuming, forgotten when the settings are
/// saved: a resumed session does not look at the certificate again, and
/// one accepted while validation was off would outlive turning it on.
#[derive(Debug)]
struct Sessions {
    settings: Handle,
    cache: Mutex<(u64, Arc<ClientSessionMemoryCache>)>,
}

impl Sessions {
    fn current(&self) -> Arc<ClientSessionMemoryCache> {
        let generation = self.settings.0.generation.load(Ordering::SeqCst);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.0 != generation {
            *cache = (generation, Arc::new(ClientSessionMemoryCache::new(256)));
        }
        cache.1.clone()
    }
}

impl ClientSessionStore for Sessions {
    fn set_kx_hint(&self, server_name: ServerName<'static>, group: rustls::NamedGroup) {
        self.current().set_kx_hint(server_name, group)
    }

    fn kx_hint(&self, server_name: &ServerName<'_>) -> Option<rustls::NamedGroup> {
        self.current().kx_hint(server_name)
    }

    fn set_tls12_session(&self, server_name: ServerName<'static>, value: Tls12ClientSessionValue) {
        self.current().set_tls12_session(server_name, value)
    }

    fn tls12_session(&self, server_name: &ServerName<'_>) -> Option<Tls12ClientSessionValue> {
        self.current().tls12_session(server_name)
    }

    fn remove_tls12_session(&self, server_name: &ServerName<'static>) {
        self.current().remove_tls12_session(server_name)
    }

    fn insert_tls13_ticket(&self, server_name: ServerName<'static>, value: Tls13ClientSessionValue) {
        self.current().insert_tls13_ticket(server_name, value)
    }

    fn take_tls13_ticket(&self, server_name: &ServerName<'static>) -> Option<Tls13ClientSessionValue> {
        self.current().take_tls13_ticket(server_name)
    }
}

/// The settings of the process: one client serves it, so one set of
/// outgoing settings does.
pub fn process() -> &'static Handle {
    static HANDLE: std::sync::OnceLock<Handle> = std::sync::OnceLock::new();
    HANDLE.get_or_init(Handle::default)
}

/// The platform's verifier, overruled where the settings say a failed
/// certificate is acceptable. The handshake's signatures are always
/// checked.
#[derive(Debug)]
struct Lenient {
    platform: Arc<dyn ServerCertVerifier>,
    settings: Handle,
}

impl ServerCertVerifier for Lenient {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let failure =
            match self
                .platform
                .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            {
                Ok(verified) => return Ok(verified),
                Err(e) => e,
            };
        let host = server_name.to_str();
        if self.settings.lets_through(&host) {
            return Ok(ServerCertVerified::assertion());
        }
        tracing::error!("Certificate validation for {host} failed. {failure}");
        Err(failure)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.platform.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.platform.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.platform.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).unwrap()
    }

    fn handle(proxy: Proxy) -> Handle {
        let handle = Handle::default();
        handle.set(Outbound {
            proxy: Some(proxy),
            certificate_validation: CertificateValidation::Enabled,
        });
        handle
    }

    #[test]
    fn the_proxy_url_follows_the_type_and_the_credentials() {
        let through = |proxy: Proxy| {
            handle(proxy)
                .proxy_for(&url("https://indexer.example/api"))
                .map(|u| u.to_string())
        };
        let plain = |kind| Proxy::new(kind, "proxy.lan", 3128, "", "", "", true);
        assert_eq!(
            through(plain(ProxyType::Http)).as_deref(),
            Some("http://proxy.lan:3128/")
        );
        assert_eq!(
            through(plain(ProxyType::Socks4)).as_deref(),
            Some("socks4://proxy.lan:3128")
        );
        assert_eq!(
            through(plain(ProxyType::Socks5)).as_deref(),
            Some("socks5h://proxy.lan:3128")
        );
        // Credentials only when both are there; no host means this machine.
        let with = |user, password| Proxy::new(ProxyType::Http, " ", 8080, user, password, "", true);
        assert_eq!(
            through(with("me", "secret")).as_deref(),
            Some("http://me:secret@127.0.0.1:8080/")
        );
        assert_eq!(through(with("me", "")).as_deref(), Some("http://127.0.0.1:8080/"));
        // No proxy configured: direct.
        assert_eq!(
            Handle::default().proxy_for(&url("https://indexer.example/")),
            None
        );
    }

    #[test]
    fn local_means_this_machine_or_a_name_without_a_dot() {
        let proxied = |bypass_local: bool, target: &str| {
            handle(Proxy::new(
                ProxyType::Http,
                "proxy.lan",
                3128,
                "",
                "",
                "",
                bypass_local,
            ))
            .proxy_for(&url(target))
            .is_some()
        };
        for local in [
            "http://localhost:8080/",
            "http://127.0.0.1:9696/",
            "http://[::1]/",
            "http://qbittorrent:8080/",
        ] {
            assert!(!proxied(true, local), "{local}");
            assert!(proxied(false, local), "{local}");
        }
        // A private address elsewhere is not "local" to the originals.
        assert!(proxied(true, "http://192.168.250.251:8080/"));
        assert!(proxied(true, "https://indexer.example/"));
    }

    #[test]
    fn the_bypass_list_holds_expressions_and_ranges() {
        let filter = "*.lan.example, tracker\\.example$, 192.168.250.0/24, 10.0.0.5-10.0.0.9, 172.16.0.0/255.240.0.0, fd00::/8, ([";
        let proxy = handle(Proxy::new(
            ProxyType::Http,
            "proxy.lan",
            3128,
            "",
            "",
            filter,
            false,
        ));
        let proxied = |target: &str| proxy.proxy_for(&url(target)).is_some();
        // Expressions, searched for anywhere in `scheme://host[:port]`,
        // whatever the case.
        assert!(!proxied("https://sab.lan.example/api"));
        assert!(!proxied("http://TRACKER.example/announce"));
        assert!(proxied("http://tracker.example:8080/announce"));
        assert!(proxied("https://indexer.example/"));
        // Networks, masks and ranges.
        assert!(!proxied("http://192.168.250.7:8080/"));
        assert!(proxied("http://192.168.251.7:8080/"));
        assert!(!proxied("http://10.0.0.9/") && proxied("http://10.0.0.10/"));
        assert!(!proxied("http://172.31.255.255/") && proxied("http://172.32.0.1/"));
        assert!(!proxied("http://[fd12::1]/") && proxied("http://[2001:db8::1]/"));
    }

    #[test]
    fn ranges_parse_as_the_originals() {
        let v4 = |text: &str| u32::from(text.parse::<Ipv4Addr>().unwrap()) as u128;
        assert_eq!(
            parse_range("10.1.2.3"),
            Some((v4("10.1.2.3"), v4("10.1.2.3"), true))
        );
        assert_eq!(
            parse_range("10.1.2.3/8"),
            Some((v4("10.0.0.0"), v4("10.255.255.255"), true))
        );
        assert_eq!(parse_range("0.0.0.0/0"), Some((0, v4("255.255.255.255"), true)));
        assert_eq!(parse_range("::/0"), Some((0, u128::MAX, false)));
        for bad in [
            "10.0.0.0/33",
            "10.0.0.0/255.0.255.0",
            "10.0.0.9-10.0.0.5",
            "sab.lan",
            "10.0.0.1-::1",
        ] {
            assert_eq!(parse_range(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_failed_certificate_passes_where_the_setting_says() {
        let with = |validation| {
            let handle = Handle::default();
            handle.set(Outbound {
                proxy: None,
                certificate_validation: validation,
            });
            handle
        };
        let enabled = with(CertificateValidation::Enabled);
        let local = with(CertificateValidation::DisabledForLocalAddresses);
        let disabled = with(CertificateValidation::Disabled);
        // This machine, always.
        assert!(enabled.lets_through("localhost") && enabled.lets_through("127.0.0.1"));
        assert!(!enabled.lets_through("192.168.250.251") && !enabled.lets_through("8.8.8.8"));
        assert!(local.lets_through("192.168.250.251") && local.lets_through("fd00::5"));
        assert!(!local.lets_through("8.8.8.8"));
        // A name that does not resolve is not known to be local.
        assert!(!local.lets_through("name.that.does.not.resolve.invalid"));
        assert!(disabled.lets_through("8.8.8.8") && disabled.lets_through("anything.invalid"));
    }

    #[test]
    fn address_classes() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        for local in [
            "127.0.0.1",
            "192.168.1.10",
            "::ffff:10.0.0.5",
            "fe80::1",
            "fec0::1",
            "169.254.1.1",
            "::1",
        ] {
            assert!(is_local_address(ip(local)), "{local}");
        }
        for remote in ["8.8.8.8", "100.64.0.1", "172.32.0.1", "2001:db8::1"] {
            assert!(!is_local_address(ip(remote)), "{remote}");
        }
        assert!(is_cgnat_address(ip("100.64.0.1")) && is_cgnat_address(ip("100.127.255.255")));
        assert!(is_cgnat_address(ip("::ffff:100.100.1.1")));
        assert!(!is_cgnat_address(ip("100.128.0.1")) && !is_cgnat_address(ip("100.63.255.255")));
    }
}
