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

/// Build a client with Unarr's defaults. Tests build their own to point at
/// mocks; production code takes [`shared`].
pub fn build(timeout: Duration) -> reqwest::Client {
    install_crypto_provider();
    reqwest::Client::builder()
        .user_agent(user_agent())
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::limited(5))
        // A redirect would otherwise carry the first URL, API key included,
        // to the next host in `Referer`.
        .referer(false)
        .build()
        .expect("reqwest client with static configuration")
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
        install_crypto_provider();
        reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(Duration::from_secs(100))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
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
}
