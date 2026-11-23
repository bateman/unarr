//! `AllowedHosts`: the host names the server answers to.
//!
//! The originals enforce the `config.xml` setting with ASP.NET's host
//! filtering, which protects against DNS rebinding: a page on an attacker's
//! domain pointed at the server's address would otherwise read
//! `/initialize.json`, and with it the API key, from a browser on the LAN.
//! Rules as the originals apply them: the list is split on commas and
//! semicolons; empty, or containing `*`, means every host; otherwise the
//! loopback names and the machine's host name are always allowed too, a
//! request without a `Host` header passes, `*.example.com` matches the
//! subdomains of `example.com`, ports are ignored, and anything else is
//! answered with 400.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// The parsed setting. `None` allows every host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedHosts(Option<Vec<String>>);

impl AllowedHosts {
    /// Parse the `config.xml` value, adding the hosts the originals always allow.
    pub fn parse(setting: &str) -> AllowedHosts {
        let mut hosts: Vec<String> = setting
            .split([',', ';'])
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(normalize)
            // Bind addresses, which the originals' parser drops: with nothing
            // else listed they would lock every real host out.
            .filter(|h| !is_bind_address(h))
            .collect();
        if hosts.is_empty() || hosts.iter().any(|h| h == "*") {
            return AllowedHosts(None);
        }
        for always in ["localhost", "127.0.0.1", "[::1]"] {
            hosts.push(always.to_string());
        }
        hosts.extend(machine_host_names().into_iter().map(|h| normalize(&h)));
        hosts.sort();
        hosts.dedup();
        AllowedHosts(Some(hosts))
    }

    pub fn allows_everything(&self) -> bool {
        self.0.is_none()
    }

    /// Whether a request with this `Host` header value may be served.
    pub fn allows(&self, host_header: Option<&str>) -> bool {
        let Some(allowed) = &self.0 else {
            return true;
        };
        let Some(raw) = host_header.map(str::trim).filter(|h| !h.is_empty()) else {
            return true;
        };
        let host = without_port(raw).to_ascii_lowercase();
        allowed.iter().any(|a| {
            if let Some(suffix) = a.strip_prefix("*.") {
                host.len() > suffix.len() + 1
                    && host.ends_with(suffix)
                    && host[..host.len() - suffix.len()].ends_with('.')
            } else {
                *a == host
            }
        })
    }
}

/// `IsBindAddress`: an address a server listens on, which no request names.
pub fn is_bind_address(host: &str) -> bool {
    host == "0.0.0.0" || host == "[::]"
}

/// The bind addresses the setting lists, as written once normalised.
pub fn bind_addresses(setting: &str) -> Vec<String> {
    setting
        .split([',', ';'])
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(normalize)
        .filter(|h| is_bind_address(h))
        .collect()
}

/// Lower-case, and bare IPv6 addresses in brackets as the originals store them.
fn normalize(host: &str) -> String {
    let h = host.to_ascii_lowercase();
    if h.contains(':') && !h.starts_with('[') {
        format!("[{h}]")
    } else {
        h
    }
}

/// `example.com:8989` → `example.com`; `[::1]:8989` → `[::1]`.
fn without_port(host: &str) -> &str {
    if host.starts_with('[') {
        match host.find(']') {
            Some(end) => &host[..=end],
            None => host,
        }
    } else {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    }
}

/// The machine's host name, as the originals add it to the allowed list.
fn machine_host_names() -> Vec<String> {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes into a valid buffer.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return Vec::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    if name.is_empty() {
        Vec::new()
    } else {
        vec![name]
    }
}

/// What ASP.NET's host filtering answers a host it refuses, recorded on
/// Sonarr 4.0.20 and Radarr 6.4.4 (`Content-Type: text/html`, no charset).
const INVALID_HOSTNAME: &str = "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\"\"http://www.w3.org/TR/html4/strict.dtd\">\r\n<HTML><HEAD><TITLE>Bad Request</TITLE>\r\n<META HTTP-EQUIV=\"Content-Type\" Content=\"text/html; charset=us-ascii\"></ HEAD >\r\n<BODY><h2>Bad Request - Invalid Hostname</h2>\r\n<hr><p>HTTP Error 400. The request hostname is invalid.</p>\r\n</BODY></HTML>";

/// Reject requests for a host the configuration does not list. The host
/// checked is the one a proxy `TrustedNetworks` believes says the client
/// asked for (`X-Forwarded-Host`), when there is one: the originals run
/// the forwarded headers before the host filter, so a proxy that rewrites
/// `Host` still reaches them under the name the client used.
pub async fn require_allowed_host(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let settings = state.settings();
    if settings.allowed_hosts.allows_everything() {
        return next.run(request).await;
    }
    let remote = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    let forwarded = crate::forwarded::walk(remote, request.headers(), &settings.trusted_networks);
    let host = forwarded.host.or_else(|| {
        request
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    });
    if settings.allowed_hosts.allows(host.as_deref()) {
        return next.run(request).await;
    }
    tracing::info!(
        target: "Microsoft.AspNetCore.HostFiltering.HostFilteringMiddleware",
        "The host '{}' does not match an allowed host.",
        host.unwrap_or_default()
    );
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "text/html")],
        INVALID_HOSTNAME,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AllowedHostsParserFixture.should_be_bind_address` and
    /// `should_not_be_bind_address` (Radarr 7), case for case; and
    /// `ConfigureHostFilteringOptionsFixture`: bind addresses alone allow
    /// every host.
    #[test]
    fn bind_addresses_are_told_apart() {
        for host in ["0.0.0.0", "::", "[::]"] {
            assert_eq!(bind_addresses(host).len(), 1, "{host}");
            assert!(AllowedHosts::parse(host).allows_everything(), "{host}");
        }
        for host in ["*", "sonarr.local", "*.example.com", "192.168.1.5", "[::1]"] {
            assert!(!is_bind_address(host), "{host}");
        }
        assert!(AllowedHosts::parse("0.0.0.0, [::]").allows_everything());
        assert_eq!(bind_addresses("sonarr.local, 0.0.0.0, [::]"), ["0.0.0.0", "[::]"]);
        let some = AllowedHosts::parse("sonarr.local, 0.0.0.0, [::]");
        assert!(some.allows(Some("sonarr.local")));
        assert!(!some.allows(Some("0.0.0.0")));
    }

    #[test]
    fn empty_or_star_allows_everything() {
        for setting in ["", "  ", "*", "sonarr.local, *", "0.0.0.0", "::, 0.0.0.0"] {
            let hosts = AllowedHosts::parse(setting);
            assert!(hosts.allows_everything(), "{setting:?}");
            assert!(hosts.allows(Some("evil.example")));
        }
    }

    #[test]
    fn listed_loopback_and_wildcards_pass_others_do_not() {
        let hosts = AllowedHosts::parse("sonarr.local; 192.168.1.5, ::1, *.example.com");
        for ok in [
            "sonarr.local",
            "SONARR.LOCAL:8989",
            "192.168.1.5:8989",
            "localhost",
            "127.0.0.1:8989",
            "[::1]:8989",
            "tv.example.com",
            "a.b.example.com",
        ] {
            assert!(hosts.allows(Some(ok)), "{ok}");
        }
        for bad in [
            "evil.example",
            "example.com",
            "notexample.com",
            "sonarr.local.evil.example",
        ] {
            assert!(!hosts.allows(Some(bad)), "{bad}");
        }
        // A bind address beside a real host is dropped, the host stays.
        let with_bind = AllowedHosts::parse("0.0.0.0, sonarr.local");
        assert!(!with_bind.allows_everything());
        assert!(with_bind.allows(Some("sonarr.local")));
        assert!(!with_bind.allows(Some("192.168.1.5:8989")));
        // No Host header at all: allowed, as AllowEmptyHosts does.
        assert!(hosts.allows(None));
        assert!(hosts.allows(Some("")));
    }
}
