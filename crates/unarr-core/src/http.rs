//! The one HTTP client every indexer and download client shares.
//!
//! `reqwest` on `rustls` with the `ring` provider: no OpenSSL, no C
//! cryptography library to build on musl, certificates from the platform
//! store. One client means one connection pool and one set of timeouts for
//! the whole process, as the originals' `HttpClient` is one service.

use std::sync::OnceLock;
use std::time::Duration;

/// The client type, for crates that do not depend on `reqwest` themselves.
pub type HttpClient = reqwest::Client;

/// `User-Agent` towards indexers, download clients and notification
/// targets: the pinned original's, `Sonarr/<version> (ubuntu 24.04)` for
/// the Sonarr personality, as Unarr replaces the original rather than runs
/// beside it (`docs/DESIGN.md` section 21.13). Indexers, trackers and
/// Prowlarr, which keep statistics by it, see the application they saw
/// before, and Prowlarr counts each personality apart under the
/// original's name. A personality with no pin yet (Lidarr) goes by
/// Unarr's own name.
pub fn user_agent(personality: crate::Personality) -> String {
    original_user_agent(personality).unwrap_or_else(unarr_user_agent)
}

/// `User-Agent` towards the Servarr metadata proxies (Skyhook and the
/// Servarr services, `api.radarr.video`): the pinned original's, as
/// towards every other upstream, but for a reason of its own and with an
/// exit condition of its own (`docs/DESIGN.md` section 7): the responses,
/// rate limits and any user-agent-dependent behaviour are the ones the
/// goldens were recorded with. When the maintainer says so, this becomes
/// Unarr's own name, the Servarr team is told, and the goldens are
/// recorded again; it is apart from [`user_agent`] so that the switch
/// does not reach the indexers. Lidarr, which emulates nothing yet, goes
/// by Unarr's own name.
pub fn servarr_proxy_user_agent(personality: crate::Personality) -> String {
    original_user_agent(personality).unwrap_or_else(unarr_user_agent)
}

/// The platform the originals report in their user agent on the
/// maintainer's hosts; the version is the pin's (`upstream/<app>.pin`,
/// [`crate::pin`]).
const ORIGINAL_PLATFORM: &str = "ubuntu 24.04";

/// The pinned original's own `User-Agent`, built here and nowhere else;
/// none for a personality with no pin.
fn original_user_agent(personality: crate::Personality) -> Option<String> {
    crate::pin::of(personality)
        .map(|pin| format!("{}/{} ({ORIGINAL_PLATFORM})", personality.app_name(), pin.version))
}

/// Unarr's own name, for a personality with no original pinned.
fn unarr_user_agent() -> String {
    format!(
        "Unarr/{} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Build a client with Unarr's defaults, following the process's proxy
/// and certificate settings, under the Sonarr personality's name as
/// [`shared`]. Tests build their own to point at mocks; production code
/// takes [`shared`].
pub fn build(timeout: Duration) -> reqwest::Client {
    build_with(timeout, crate::outbound::process(), crate::Personality::Sonarr)
}

/// [`build`], following the settings of `outbound`, under the name of
/// `personality`.
pub fn build_with(
    timeout: Duration,
    outbound: &crate::outbound::Handle,
    personality: crate::Personality,
) -> reqwest::Client {
    builder(outbound, personality)
        .timeout(timeout)
        .pool_idle_timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .expect("reqwest client with static configuration")
}

/// What every client has: the name it goes by, its personality's, and the
/// proxy and the certificate checks of Settings > General, read anew for
/// each request.
fn builder(outbound: &crate::outbound::Handle, personality: crate::Personality) -> reqwest::ClientBuilder {
    install_crypto_provider();
    let proxy = {
        let outbound = outbound.clone();
        reqwest::Proxy::custom(move |url| outbound.proxy_for(url))
    };
    let builder = reqwest::Client::builder()
        .user_agent(user_agent(personality))
        .connect_timeout(Duration::from_secs(10))
        // A redirect would otherwise carry the first URL, API key included,
        // to the next host in `Referer`.
        .referer(false)
        .proxy(proxy);
    match outbound.tls_config() {
        Ok(tls) => builder.tls_backend_preconfigured(tls),
        Err(e) => {
            // No certificate store to read: the client's own defaults.
            tracing::warn!("Certificate validation settings cannot be applied: {e}");
            builder
        }
    }
}

/// Most a response body may hold. Feeds and client answers are kilobytes;
/// past this a response is an attack or a bug, and reading it whole would
/// exhaust memory: gzip is on, so a small transfer can decode to a lot.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Why a body could not be read.
#[derive(Debug)]
pub enum BodyError {
    TooLarge { limit: usize },
    Transport(reqwest::Error),
}

impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BodyError::TooLarge { limit } => write!(f, "the response exceeds {limit} bytes"),
            // The error's own text names the URL, key included; the cause
            // at the end of its chain does not.
            BodyError::Transport(e) => {
                let mut cause: &dyn std::error::Error = e;
                while let Some(next) = cause.source() {
                    cause = next;
                }
                write!(f, "{cause}")
            }
        }
    }
}

/// The body read chunk by chunk and refused past `limit`, where `bytes()`
/// would read on until memory runs out.
pub async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, BodyError> {
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(BodyError::Transport)? {
        if out.len() + chunk.len() > limit {
            return Err(BodyError::TooLarge { limit });
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// [`read_body`] as text; bytes that are not UTF-8 become U+FFFD.
pub async fn read_text(response: reqwest::Response, limit: usize) -> Result<String, BodyError> {
    Ok(String::from_utf8_lossy(&read_body(response, limit).await?).into_owned())
}

/// The query parameters that carry a secret: an indexer or client key, a
/// tracker passkey, SABnzbd's login fields.
const SECRET_PARAMS: &[&str] = &[
    "apikey",
    "api_key",
    "passkey",
    "token",
    "x-plex-token",
    "key",
    "password",
    "ma_username",
    "ma_password",
    "r",
];

/// `CleanseLogMessage`: a URL with its secret query values blanked, for
/// logs and error texts; the originals scrub theirs the same way.
pub fn redact(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let (query, fragment) = match query.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (query, None),
    };
    let cleaned: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) if SECRET_PARAMS.contains(&k.to_ascii_lowercase().as_str()) => {
                format!("{k}=(removed)")
            }
            _ => pair.to_string(),
        })
        .collect();
    let mut out = format!("{base}?{}", cleaned.join("&"));
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// The HTTP clients of one personality with the settings they follow. One
/// process serves several personalities, and each has its own proxy and
/// certificate settings in Settings > General: a client built over the
/// handle of one never follows a save of another. Each also goes by its
/// personality's name ([`user_agent`]).
#[derive(Debug, Clone)]
pub struct Clients {
    /// What Settings > General says about outgoing connections; whoever
    /// saves them sets it here.
    pub outbound: crate::outbound::Handle,
    pub client: reqwest::Client,
    /// For torrent downloads, see [`no_redirect`].
    pub no_redirect: reqwest::Client,
}

impl Clients {
    /// Clients of their own over settings of their own, under the name of
    /// `personality`.
    pub fn new(personality: crate::Personality) -> Clients {
        let outbound = crate::outbound::Handle::default();
        Clients {
            client: build_with(Duration::from_secs(100), &outbound, personality),
            no_redirect: builder(&outbound, personality)
                .timeout(Duration::from_secs(100))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest client with static configuration"),
            outbound,
        }
    }

    /// The clients [`shared`] and [`no_redirect`] hand out, over the
    /// settings [`crate::outbound::process`] holds: what the series side
    /// takes where it is not handed a client, and so the Sonarr
    /// personality's, under its name. No other personality may set those
    /// settings.
    pub fn process() -> Clients {
        Clients {
            outbound: crate::outbound::process().clone(),
            client: shared().clone(),
            no_redirect: no_redirect().clone(),
        }
    }
}

/// The process-wide client, the Sonarr personality's. The originals use
/// 100 s for indexers and download clients alike; a slow tracker holds a
/// connection, not a thread.
pub fn shared() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build(Duration::from_secs(100)))
}

/// A client that does not follow redirects, for torrent downloads: a
/// tracker may answer with a `magnet:` location, which only the caller can
/// act on.
pub fn no_redirect() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        builder(crate::outbound::process(), crate::Personality::Sonarr)
            .timeout(Duration::from_secs(100))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client with static configuration")
    })
}

/// `rustls` needs a cryptography provider chosen before the first
/// connection. Installing twice is an error, so this is idempotent.
fn install_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Recover;

    #[test]
    fn redacts_secret_query_values_only() {
        assert_eq!(
            redact("http://idx.invalid/api?t=tvsearch&apikey=abc123&q=show"),
            "http://idx.invalid/api?t=tvsearch&apikey=(removed)&q=show"
        );
        assert_eq!(
            redact("http://sab.invalid/api?mode=queue&APIKEY=x&ma_password=y#f"),
            "http://sab.invalid/api?mode=queue&APIKEY=(removed)&ma_password=(removed)#f"
        );
        assert_eq!(redact("http://plain.invalid/path"), "http://plain.invalid/path");
        assert_eq!(
            redact("http://plex.invalid:32400/identity?X-Plex-Token=secret"),
            "http://plex.invalid:32400/identity?X-Plex-Token=(removed)"
        );
    }

    #[test]
    fn the_client_builds() {
        let _ = build(Duration::from_secs(1));
        let _ = shared();
    }

    /// Each personality goes by its pinned original's user agent, which
    /// Prowlarr counts under the original's name; Lidarr, with no pin, by
    /// Unarr's own.
    #[test]
    fn the_user_agent_is_the_pinned_originals() {
        use crate::Personality;
        assert_eq!(
            user_agent(Personality::Sonarr),
            format!("Sonarr/{} (ubuntu 24.04)", crate::pin::SONARR.version)
        );
        assert_eq!(
            user_agent(Personality::Radarr),
            format!("Radarr/{} (ubuntu 24.04)", crate::pin::RADARR.version)
        );
        assert_eq!(prowlarr_source(&user_agent(Personality::Sonarr)), "Sonarr");
        assert_eq!(prowlarr_source(&user_agent(Personality::Radarr)), "Radarr");
        assert!(user_agent(Personality::Lidarr).starts_with("Unarr/"));
        assert_eq!(prowlarr_source(&user_agent(Personality::Lidarr)), "Unarr");
    }

    /// `UserAgentParser.ParseSource` of Prowlarr 2.x, which names the
    /// client in its statistics: the leading word of letters and digits,
    /// followed by `/` and more or by the end
    /// (`^(?<agent>[a-z0-9]+)(?:\/.+(?:\(.*\))?|$)`, case ignored);
    /// `Other` when there is none, or for a browser.
    fn prowlarr_source(agent: &str) -> &str {
        let agent = agent.trim();
        if agent.starts_with("Mozilla/5.0") {
            return "Other";
        }
        let word = agent
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(agent.len());
        let rest = &agent[word..];
        if word > 0 && (rest.is_empty() || rest.len() > 1 && rest.starts_with('/')) {
            &agent[..word]
        } else {
            "Other"
        }
    }

    /// The clients of a personality go by its name, the plain and the one
    /// that follows no redirect alike.
    #[tokio::test]
    async fn a_personalitys_clients_send_its_name() {
        use crate::Personality;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let agent = request
                    .lines()
                    .find_map(|l| l.strip_prefix("user-agent: "))
                    .unwrap_or_default()
                    .to_string();
                log.lock().recover().push(agent);
                let _ = stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        let radarr = Clients::new(Personality::Radarr);
        for client in [&radarr.client, &radarr.no_redirect, shared()] {
            client.get(format!("http://{address}/api")).send().await.unwrap();
        }
        let seen = seen.lock().recover().clone();
        assert_eq!(
            seen,
            vec![
                user_agent(Personality::Radarr),
                user_agent(Personality::Radarr),
                user_agent(Personality::Sonarr),
            ]
        );
    }

    /// The metadata proxies hear the pinned original's own user agent.
    #[test]
    fn the_servarr_proxies_hear_the_pinned_original() {
        use crate::Personality;
        assert_eq!(
            servarr_proxy_user_agent(Personality::Sonarr),
            format!("Sonarr/{} (ubuntu 24.04)", crate::pin::SONARR.version)
        );
        assert_eq!(
            servarr_proxy_user_agent(Personality::Radarr),
            format!("Radarr/{} (ubuntu 24.04)", crate::pin::RADARR.version)
        );
        assert!(servarr_proxy_user_agent(Personality::Lidarr).starts_with("Unarr/"));
    }

    use crate::outbound::{CertificateValidation, Handle, Outbound, Proxy, ProxyType};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::{Arc, Mutex};

    /// A proxy that answers every request itself and keeps its first line.
    fn recording_proxy() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let mut lines: Vec<String> = request.lines().take(1).map(str::to_string).collect();
                lines.extend(
                    request
                        .lines()
                        .filter(|l| l.to_ascii_lowercase().starts_with("proxy-authorization:"))
                        .map(str::to_string),
                );
                log.lock().recover().extend(lines);
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\nconnection: close\r\n\r\nproxied");
            }
        });
        (address, seen)
    }

    /// The proxy of the settings is used for the requests it is not
    /// bypassed for, with its credentials, and a change of the settings
    /// reaches the client already built.
    #[tokio::test]
    async fn requests_go_through_the_configured_proxy() {
        let (proxy, seen) = recording_proxy();
        let settings = Handle::default();
        let client = build_with(Duration::from_secs(5), &settings, crate::Personality::Sonarr);
        let proxied = |host: &str| Outbound {
            proxy: Some(Proxy::new(
                ProxyType::Http,
                &proxy.ip().to_string(),
                proxy.port() as i64,
                "me",
                "secret",
                host,
                true,
            )),
            certificate_validation: CertificateValidation::Enabled,
        };

        settings.set(proxied(""));
        let body = client
            .get("http://indexer.invalid/api?t=caps")
            .send()
            .await
            .expect("answered by the proxy")
            .text()
            .await
            .unwrap();
        assert_eq!(body, "proxied");
        {
            let seen = seen.lock().recover();
            assert_eq!(seen[0], "GET http://indexer.invalid/api?t=caps HTTP/1.1");
            // "me:secret".
            assert!(seen[1].ends_with("Basic bWU6c2VjcmV0"), "{seen:?}");
        }

        // On the bypass list: not through the proxy, and the name resolves
        // nowhere.
        settings.set(proxied("indexer\\.invalid"));
        assert!(client
            .get("http://indexer.invalid/api?t=caps")
            .send()
            .await
            .is_err());
        // No proxy any more: the same.
        settings.set(Outbound::default());
        assert!(client
            .get("http://indexer.invalid/api?t=caps")
            .send()
            .await
            .is_err());
        assert_eq!(seen.lock().recover().len(), 2);
    }

    /// An HTTPS server with the self-signed certificate of the fixtures.
    fn self_signed_server() -> SocketAddr {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tls");
        let certificate = CertificateDer::from_pem_file(fixtures.join("selfsigned.cert.pem")).unwrap();
        let key = PrivateKeyDer::from_pem_file(fixtures.join("selfsigned.key.pem")).unwrap();
        let config =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate], key)
                .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let config = config.clone();
                std::thread::spawn(move || {
                    let Ok(connection) = rustls::ServerConnection::new(config) else {
                        return;
                    };
                    let mut tls = rustls::StreamOwned::new(connection, stream);
                    let mut buffer = [0u8; 4096];
                    if tls.read(&mut buffer).is_ok() {
                        let _ = tls.write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        );
                        tls.conn.send_close_notify();
                        let _ = tls.flush();
                    }
                });
            }
        });
        address
    }

    /// A certificate nobody vouches for is refused, unless the setting
    /// says otherwise; a save changes what the client already built does.
    #[tokio::test]
    async fn certificate_validation_follows_the_setting() {
        let server = self_signed_server();
        let settings = Handle::default();
        // The certificate's name, resolved to the test server.
        let client = builder(&settings, crate::Personality::Sonarr)
            .timeout(Duration::from_secs(5))
            .resolve("selfsigned.test", server)
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        let url = format!("https://selfsigned.test:{}/", server.port());
        let with = |validation| {
            settings.set(Outbound {
                proxy: None,
                certificate_validation: validation,
            })
        };

        with(CertificateValidation::Enabled);
        assert!(
            client.get(&url).send().await.is_err(),
            "refused when validation is on"
        );
        with(CertificateValidation::Disabled);
        let body = client
            .get(&url)
            .send()
            .await
            .expect("let through")
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
        with(CertificateValidation::Enabled);
        assert!(client.get(&url).send().await.is_err(), "refused again");
        // This machine by name or address is always let through.
        let local = format!("https://127.0.0.1:{}/", server.port());
        assert_eq!(
            client
                .get(&local)
                .send()
                .await
                .expect("local")
                .text()
                .await
                .unwrap(),
            "ok"
        );
    }
}
