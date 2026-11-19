//! `TrustedNetworks` and the `X-Forwarded-For` header: who the client of a
//! request is when it came through a reverse proxy.
//!
//! The originals run ASP.NET's forwarded-headers middleware with the
//! networks of the setting as `KnownNetworks`, on top of the loopback the
//! framework trusts by default. A proxy on one of them is believed: the
//! address it names in `X-Forwarded-For` becomes the client's, and the
//! entry is taken off the header. A header from anyone else is left as it
//! is, and the originals then treat the client as unknown
//! (`UiAuthorizationHandler.IsClientAddressKnown`): "Disabled for Local
//! Addresses" does not exempt it. Without the setting, every request that
//! came through a proxy is asked to log in.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::http::HeaderMap;

/// An address and how many of its leading bits name the network.
type Network = (IpAddr, u8);

/// `IPNetworkParser.TryParse`: an address, or a network in CIDR notation
/// whose host bits are zero.
pub fn parse_network(entry: &str) -> Option<Network> {
    let mut parts = entry.split('/');
    let address_part = parts.next()?.trim();
    let prefix_part = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let address: IpAddr = address_part.parse().ok()?;
    let max = if address.is_ipv4() { 32 } else { 128 };
    let Some(prefix_part) = prefix_part else {
        return Some((address, max));
    };
    let prefix: u8 = prefix_part.trim().parse().ok()?;
    if prefix < 1 || prefix > max {
        return None;
    }
    (mask(address, prefix) == address).then_some((address, prefix))
}

/// `address` with the bits past `prefix` cleared.
fn mask(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let kept = if prefix == 0 {
                0
            } else {
                bits & (u32::MAX << (32 - prefix as u32))
            };
            IpAddr::V4(Ipv4Addr::from(kept))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let kept = if prefix == 0 {
                0
            } else {
                bits & (u128::MAX << (128 - prefix as u32))
            };
            IpAddr::V6(Ipv6Addr::from(kept))
        }
    }
}

/// An IPv4 address carried in IPv6 (`::ffff:10.0.0.5`), as IPv4.
fn unmapped(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        v4 => v4,
    }
}

/// The networks whose forwarded headers are believed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedNetworks(Vec<Network>);

impl TrustedNetworks {
    /// The setting's comma-separated list, after the loopback the framework
    /// trusts on its own (`127.0.0.0/8` and `::1`). An entry that does not
    /// parse is left out, with a warning.
    pub fn parse(setting: &str) -> TrustedNetworks {
        let mut networks: Vec<Network> = vec![
            (IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0)), 8),
            (IpAddr::V6(Ipv6Addr::LOCALHOST), 128),
        ];
        for entry in setting.split(',').filter(|e| !e.trim().is_empty()) {
            match parse_network(entry) {
                Some((address, prefix)) => {
                    tracing::info!("Trusting forwarded headers from {address}/{prefix}");
                    networks.push((address, prefix));
                }
                None => tracing::warn!(
                    "Invalid trusted network '{}', forwarded headers from it will not be trusted",
                    entry.trim()
                ),
            }
        }
        TrustedNetworks(networks)
    }

    pub fn contains(&self, address: IpAddr) -> bool {
        let address = unmapped(address);
        self.0.iter().any(|(network, prefix)| {
            network.is_ipv4() == address.is_ipv4() && mask(address, *prefix) == *network
        })
    }
}

/// Who a request is from, once the proxies that are believed have spoken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client {
    /// The client's address; `None` when the connection's is not known.
    pub address: Option<IpAddr>,
    /// Whether `X-Forwarded-For` entries are left that nobody trusted
    /// vouched for: the client's address is then not known for sure.
    pub forwarded_left: bool,
}

/// `IPEndPoint.TryParse` on an entry: an address, with or without a port.
fn parse_entry(text: &str) -> Option<IpAddr> {
    if let Ok(address) = text.parse::<IpAddr>() {
        return Some(address);
    }
    if let Ok(socket) = text.parse::<SocketAddr>() {
        return Some(socket.ip());
    }
    text.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

/// What a request says of itself once the proxies that are believed have
/// spoken: the client, and the host and the scheme the client asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forwarded {
    pub client: Client,
    /// `X-Forwarded-Host` as the walk left it; `None` when no believed
    /// proxy named one, and the request's own `Host` stands.
    pub host: Option<String>,
    /// `X-Forwarded-Proto` likewise.
    pub scheme: Option<String>,
}

/// The values of a header, over all its lines, split on commas and trimmed.
fn values<'a>(headers: &'a HeaderMap, name: &str) -> Vec<&'a str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect()
}

/// ASP.NET's forwarded-headers middleware, with `X-Forwarded-For`,
/// `X-Forwarded-Proto` and `X-Forwarded-Host` switched on and no limit on
/// the entries, as the originals set it. The three headers are read
/// together from their last entries back: while the address held so far
/// belongs to a trusted network, the entry before it in each header is
/// applied and consumed. The walk stops at the first address that is not
/// trusted, or at an entry of `X-Forwarded-For` that is not an address.
/// A scheme or a host that is not one undoes the whole walk: the request
/// stays as it came.
///
/// So with no `X-Forwarded-For` at all a trusted proxy's hosts are all
/// applied in turn, and the leftmost one stands (recorded on Sonarr 4.0.20
/// and Radarr 6.4.4: `evil.example, tv.example.com` is checked against
/// Allowed Hosts as `evil.example`).
pub fn walk(remote: Option<IpAddr>, headers: &HeaderMap, trusted: &TrustedNetworks) -> Forwarded {
    let fors = values(headers, "x-forwarded-for");
    let protos = values(headers, "x-forwarded-proto");
    let hosts = values(headers, "x-forwarded-host");
    let untouched = Forwarded {
        client: Client {
            address: remote.map(unmapped),
            forwarded_left: !fors.is_empty(),
        },
        host: None,
        scheme: None,
    };
    let mut address = remote.map(unmapped);
    let mut host = None;
    let mut scheme = None;
    let mut consumed = 0;
    for i in 0..fors.len().max(protos.len()).max(hosts.len()) {
        if !address.is_some_and(|a| trusted.contains(a)) {
            break;
        }
        // Nothing named: consumed, the address stays.
        if let Some(named) = nth_back(&fors, i).filter(|e| !e.is_empty()) {
            match parse_entry(named) {
                Some(named) => address = Some(unmapped(named)),
                None => break,
            }
        }
        if let Some(proto) = nth_back(&protos, i).filter(|e| !e.is_empty()) {
            if !valid_scheme(proto) {
                return untouched;
            }
            scheme = Some(proto.to_string());
        }
        if let Some(named) = nth_back(&hosts, i).filter(|e| !e.is_empty()) {
            if !valid_host(named) {
                return untouched;
            }
            host = Some(named.to_string());
        }
        consumed += 1;
    }
    Forwarded {
        client: Client {
            address,
            forwarded_left: consumed < fors.len(),
        },
        host,
        scheme,
    }
}

/// The `i`-th value from the end.
fn nth_back<'a>(list: &[&'a str], i: usize) -> Option<&'a str> {
    list.len().checked_sub(i + 1).map(|at| list[at])
}

/// The client alone: [`walk`]'s.
pub fn client(remote: Option<IpAddr>, headers: &HeaderMap, trusted: &TrustedNetworks) -> Client {
    walk(remote, headers, trusted).client
}

/// `TryValidateScheme`: letters, digits, `+`, `-` and `.`.
fn valid_scheme(scheme: &str) -> bool {
    scheme
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// `TryValidateHost`: a name of the characters RFC 3986 allows in one, or
/// an IPv6 address in brackets, and a port of digits after a colon.
fn valid_host(host: &str) -> bool {
    let name_end = if let Some(rest) = host.strip_prefix('[') {
        let Some(close) = rest.find(']') else {
            return false;
        };
        // `[::1]` is the shortest.
        if close < 3
            || !rest[..close]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.'))
        {
            return false;
        }
        close + 2
    } else {
        host.bytes()
            .position(|b| {
                !(b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'-' | b'.' | b'_' | b'~'
                    ))
            })
            .unwrap_or(host.len())
    };
    if name_end == 0 {
        return false;
    }
    match host[name_end..].strip_prefix(':') {
        None => name_end == host.len(),
        Some(port) => !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", v.parse().unwrap());
        }
        h
    }

    #[test]
    fn networks_parse_as_the_originals() {
        assert_eq!(parse_network("172.29.4.0/22"), Some((ip("172.29.4.0"), 22)));
        assert_eq!(parse_network(" 172.17.0.1 "), Some((ip("172.17.0.1"), 32)));
        assert_eq!(parse_network("fc00::/7"), Some((ip("fc00::"), 7)));
        assert_eq!(parse_network("::1"), Some((ip("::1"), 128)));
        // Host bits set, a prefix out of range, more than one slash, a name.
        for bad in [
            "172.29.4.1/22",
            "10.0.0.0/0",
            "10.0.0.0/33",
            "10.0.0.0/8/8",
            "traefik",
            "",
            "10.0/8",
        ] {
            assert_eq!(parse_network(bad), None, "{bad}");
        }
    }

    #[test]
    fn loopback_is_always_trusted_and_the_list_adds_to_it() {
        let none = TrustedNetworks::parse("");
        assert!(none.contains(ip("127.0.0.1")) && none.contains(ip("::1")));
        assert!(!none.contains(ip("172.29.4.2")));
        let some = TrustedNetworks::parse("172.29.4.0/22, nonsense, fd00::/8");
        assert!(some.contains(ip("172.29.4.2")) && some.contains(ip("172.29.7.255")));
        assert!(!some.contains(ip("172.29.8.1")));
        assert!(some.contains(ip("fd12::1")) && !some.contains(ip("fe80::1")));
        // An IPv4 peer seen through an IPv6 socket.
        assert!(some.contains(ip("::ffff:172.29.4.2")));
    }

    #[test]
    fn a_trusted_proxy_names_the_client_and_its_entry_is_consumed() {
        let trusted = TrustedNetworks::parse("172.29.4.0/22");
        let proxy = Some(ip("172.29.4.2"));
        assert_eq!(
            client(proxy, &headers(&["192.168.4.128"]), &trusted),
            Client {
                address: Some(ip("192.168.4.128")),
                forwarded_left: false
            }
        );
        // With a port, and in brackets.
        assert_eq!(
            client(proxy, &headers(&["192.168.4.128:51234"]), &trusted).address,
            Some(ip("192.168.4.128"))
        );
        assert_eq!(
            client(proxy, &headers(&["[fd00::5]:443"]), &trusted).address,
            Some(ip("fd00::5"))
        );
        // No header at all: the connection's address, known.
        assert_eq!(
            client(proxy, &HeaderMap::new(), &trusted),
            Client {
                address: proxy,
                forwarded_left: false
            }
        );
    }

    #[test]
    fn an_untrusted_sender_is_not_believed() {
        // The setting empty, as in a fresh installation behind a proxy.
        let nobody = TrustedNetworks::parse("");
        assert_eq!(
            client(Some(ip("172.29.4.2")), &headers(&["192.168.4.128"]), &nobody),
            Client {
                address: Some(ip("172.29.4.2")),
                forwarded_left: true
            }
        );
        // A client that writes the header itself gains nothing.
        let trusted = TrustedNetworks::parse("172.29.4.0/22");
        assert_eq!(
            client(Some(ip("8.8.8.8")), &headers(&["127.0.0.1"]), &trusted),
            Client {
                address: Some(ip("8.8.8.8")),
                forwarded_left: true
            }
        );
        assert!(client(None, &headers(&["127.0.0.1"]), &trusted).forwarded_left);
    }

    #[test]
    fn the_walk_stops_at_the_first_address_nobody_vouches_for() {
        let trusted = TrustedNetworks::parse("172.29.4.0/22");
        let proxy = Some(ip("172.29.4.2"));
        // The proxy vouches for 203.0.113.9; what that one claimed stays.
        assert_eq!(
            client(proxy, &headers(&["10.0.0.1, 203.0.113.9"]), &trusted),
            Client {
                address: Some(ip("203.0.113.9")),
                forwarded_left: true
            }
        );
        // Two trusted hops, over two header lines: both consumed.
        assert_eq!(
            client(proxy, &headers(&["192.168.4.128", "172.29.4.9"]), &trusted),
            Client {
                address: Some(ip("192.168.4.128")),
                forwarded_left: false
            }
        );
        // An entry that is not an address ends it.
        assert_eq!(
            client(proxy, &headers(&["unknown"]), &trusted),
            Client {
                address: proxy,
                forwarded_left: true
            }
        );
    }

    fn forwarded(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in pairs {
            h.append(*name, value.parse().unwrap());
        }
        h
    }

    /// The cases recorded on Sonarr 4.0.20 and Radarr 6.4.4 with Allowed
    /// Hosts `tv.example.com`, from the loopback and from another address.
    #[test]
    fn hosts_and_schemes_walk_with_the_addresses() {
        let trusted = TrustedNetworks::parse("");
        let lo = Some(ip("127.0.0.1"));
        let host = |remote, pairs: &[(&'static str, &str)]| walk(remote, &forwarded(pairs), &trusted).host;
        assert_eq!(
            host(lo, &[("x-forwarded-host", "tv.example.com")]).as_deref(),
            Some("tv.example.com")
        );
        assert_eq!(
            host(lo, &[("x-forwarded-host", "tv.example.com:443")]).as_deref(),
            Some("tv.example.com:443")
        );
        // No address to walk with: every host is applied, the first stands.
        assert_eq!(
            host(lo, &[("x-forwarded-host", "evil.example, tv.example.com")]).as_deref(),
            Some("evil.example")
        );
        assert_eq!(
            host(lo, &[("x-forwarded-host", "tv.example.com, evil.example")]).as_deref(),
            Some("tv.example.com")
        );
        // With addresses, each host goes with its address.
        let pairs = [
            ("x-forwarded-host", "evil.example, tv.example.com"),
            ("x-forwarded-for", "10.1.2.3, 127.0.0.1"),
        ];
        assert_eq!(host(lo, &pairs).as_deref(), Some("evil.example"));
        let pairs = [
            ("x-forwarded-host", "evil.example, tv.example.com"),
            ("x-forwarded-for", "10.1.2.3"),
        ];
        assert_eq!(host(lo, &pairs).as_deref(), Some("tv.example.com"));
        // Nobody vouches for the sender, or the header is empty.
        assert_eq!(
            host(Some(ip("192.0.2.2")), &[("x-forwarded-host", "tv.example.com")]),
            None
        );
        assert_eq!(host(lo, &[("x-forwarded-host", "")]), None);
        let w = walk(
            lo,
            &forwarded(&[("x-forwarded-proto", "https"), ("x-forwarded-for", "10.9.8.7")]),
            &trusted,
        );
        assert_eq!(
            (w.scheme.as_deref(), w.client.address),
            (Some("https"), Some(ip("10.9.8.7")))
        );
    }

    #[test]
    fn a_host_or_a_scheme_that_is_not_one_undoes_the_walk() {
        let trusted = TrustedNetworks::parse("");
        let lo = Some(ip("127.0.0.1"));
        for pairs in [
            [
                ("x-forwarded-host", "tv example.com"),
                ("x-forwarded-for", "10.9.8.7"),
            ],
            [
                ("x-forwarded-host", "tv.example.com:"),
                ("x-forwarded-for", "10.9.8.7"),
            ],
            [("x-forwarded-host", "[::1"), ("x-forwarded-for", "10.9.8.7")],
            [("x-forwarded-proto", "ht tp"), ("x-forwarded-for", "10.9.8.7")],
        ] {
            let w = walk(lo, &forwarded(&pairs), &trusted);
            assert_eq!(w.host, None, "{pairs:?}");
            assert_eq!(
                w.client,
                Client {
                    address: lo,
                    forwarded_left: true
                },
                "{pairs:?}"
            );
        }
        for good in [
            "[::1]",
            "[fd00::5]:8989",
            "tv.example.com",
            "a_b-c.d~e:1",
            "10.0.0.1:80",
        ] {
            assert!(valid_host(good), "{good}");
        }
        for bad in ["", ":80", "[::]", "a b", "a:b", "a/b", "a,b"] {
            assert!(!valid_host(bad), "{bad}");
        }
    }
}
