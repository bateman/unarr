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

/// `User-Agent` the originals send is `Sonarr/4.0.20.3014 (ubuntu 24.04)`;
/// Unarr names itself, since indexers and trackers keep statistics by it.
pub fn user_agent() -> String {
    format!(
        "Unarr/{} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// `User-Agent` towards the Servarr metadata proxies (Skyhook and the
/// Servarr services), and only towards them: the original's, exactly as
/// the pinned Sonarr sends it. A deliberate, temporary exception to Unarr
/// naming itself (`docs/DESIGN.md` section 7): the responses, rate limits
/// and any user-agent-dependent behaviour are then the ones the goldens
/// were recorded with. Once phase 3b's exit criteria are met this becomes
/// [`user_agent`], the Servarr team is told, and the goldens are recorded
/// again. Read by [`servarr_proxy_user_agent`] alone; nothing else may
/// depend on which one is in use.
const SERVARR_PROXY_USER_AGENT: &str = "Sonarr/4.0.20.3014 (ubuntu 24.04)";

/// The `User-Agent` for requests to the Servarr metadata proxies.
pub fn servarr_proxy_user_agent() -> &'static str {
    SERVARR_PROXY_USER_AGENT
}

/// Build a client with Unarr's defaults, following the process's proxy
/// and certificate settings. Tests build their own to point at mocks;
/// production code takes [`shared`].
pub fn build(timeout: Duration) -> reqwest::Client {
    build_with(timeout, crate::outbound::process())
}

/// [`build`], following the settings of `outbound`.
pub fn build_with(timeout: Duration, outbound: &crate::outbound::Handle) -> reqwest::Client {
    builder(outbound)
        .timeout(timeout)
        .pool_idle_timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .expect("reqwest client with static configuration")
}

/// What every client has: the name it goes by, and the proxy and the
/// certificate checks of Settings > General, read anew for each request.
fn builder(outbound: &crate::outbound::Handle) -> reqwest::ClientBuilder {
    install_crypto_provider();
    let proxy = {
        let outbound = outbound.clone();
        reqwest::Proxy::custom(move |url| outbound.proxy_for(url))
    };
    let builder = reqwest::Client::builder()
        .user_agent(user_agent())
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

/// The process-wide client. The originals use 100 s for indexers and
/// download clients alike; a slow tracker holds a connection, not a thread.
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
        builder(crate::outbound::process())
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
    }

    #[test]
    fn user_agent_names_unarr_and_the_client_builds() {
        assert!(user_agent().starts_with("Unarr/"));
        let _ = build(Duration::from_secs(1));
        let _ = shared();
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
                log.lock().unwrap().extend(lines);
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
        let client = build_with(Duration::from_secs(5), &settings);
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
            let seen = seen.lock().unwrap();
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
        assert_eq!(seen.lock().unwrap().len(), 2);
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
        let client = builder(&settings)
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
