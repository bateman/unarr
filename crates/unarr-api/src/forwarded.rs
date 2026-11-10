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

/// The forwarded-headers middleware's walk over `X-Forwarded-For`, from the
/// last entry back: while the address held so far belongs to a trusted
/// network, the entry before it replaces it and is consumed. The walk
/// stops at the first address that is not trusted, or at an entry that is
/// not an address.
pub fn client(remote: Option<IpAddr>, headers: &HeaderMap, trusted: &TrustedNetworks) -> Client {
    let entries: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    let mut address = remote.map(unmapped);
    let mut consumed = 0;
    for entry in entries.iter().rev() {
        if !address.is_some_and(|a| trusted.contains(a)) {
            break;
        }
        if entry.is_empty() {
            // Nothing named: consumed, the address stays.
            consumed += 1;
            continue;
        }
        match parse_entry(entry) {
            Some(named) => {
                address = Some(unmapped(named));
                consumed += 1;
            }
            None => break,
        }
    }
    Client {
        address,
        forwarded_left: consumed < entries.len(),
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
}
