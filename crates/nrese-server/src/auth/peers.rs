//! Which peers may vouch for a client: the proxies that terminate TLS and pass the client
//! certificate's subject on in a header (the `mtls` mode). A header from any other peer is
//! a claim anyone could make, so it is dropped before authentication sees it. Likewise
//! for the client's address ([`client`]): `X-Forwarded-For` counts only from a trusted
//! proxy.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// An address or a range of addresses (`10.0.0.0/8`, `fd00::/8`, `127.0.0.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressRange {
    network: IpAddr,
    prefix: u8,
}

impl AddressRange {
    /// Whether `address` lies in the range (an IPv4 address mapped into IPv6 counts as
    /// itself).
    pub fn contains(&self, address: IpAddr) -> bool {
        let address = match address {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
            v4 => v4,
        };
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(network) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }

    /// The loopback ranges: a proxy on the same host.
    pub fn loopback() -> Vec<Self> {
        vec![
            Self {
                network: IpAddr::from([127, 0, 0, 0]),
                prefix: 8,
            },
            Self {
                network: IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
                prefix: 128,
            },
        ]
    }
}

impl FromStr for AddressRange {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let network: IpAddr = address
            .parse()
            .map_err(|_| format!("'{text}' is not an address or an address range"))?;
        let bits = if network.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => bits,
            Some(prefix) => prefix
                .parse::<u8>()
                .ok()
                .filter(|&p| p <= bits)
                .ok_or_else(|| format!("'{text}': the prefix must be 0 to {bits}"))?,
        };
        Ok(Self { network, prefix })
    }
}

impl fmt::Display for AddressRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

/// Whether one of `ranges` holds the peer `address` (an unknown peer is never trusted).
pub fn trusted(ranges: &[AddressRange], address: Option<IpAddr>) -> bool {
    address.is_some_and(|address| ranges.iter().any(|range| range.contains(address)))
}

/// The client's address: the peer's, or, when the peer is a trusted proxy, the last
/// address of `X-Forwarded-For` that isn't one (each proxy appends the address it was
/// reached from, so the entries left of the first untrusted one are the client's word).
/// An entry that isn't an address ends the search at the peer.
pub fn client(
    ranges: &[AddressRange],
    peer: Option<IpAddr>,
    headers: &axum::http::HeaderMap,
) -> Option<IpAddr> {
    if !trusted(ranges, peer) {
        return peer;
    }
    let entries: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .collect();
    for entry in entries.iter().rev() {
        let address = entry
            .parse::<IpAddr>()
            .ok()
            .or_else(|| entry.parse::<std::net::SocketAddr>().ok().map(|a| a.ip()));
        match address {
            None => return peer,
            Some(address) if trusted(ranges, Some(address)) => continue,
            Some(address) => return Some(address),
        }
    }
    peer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(text: &str) -> AddressRange {
        text.parse().unwrap()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn ranges_hold_their_addresses() {
        assert!(range("10.0.0.0/8").contains(ip("10.200.1.2")));
        assert!(!range("10.0.0.0/8").contains(ip("11.0.0.1")));
        assert!(range("192.168.1.7").contains(ip("192.168.1.7")));
        assert!(!range("192.168.1.7").contains(ip("192.168.1.8")));
        assert!(range("0.0.0.0/0").contains(ip("8.8.8.8")));
        assert!(range("fd00::/8").contains(ip("fd12::1")));
        assert!(!range("fd00::/8").contains(ip("fe80::1")));
        assert!(
            range("10.0.0.0/8").contains(ip("::ffff:10.1.1.1")),
            "mapped IPv4"
        );
        assert!(!range("10.0.0.0/8").contains(ip("fd00::1")));
    }

    #[test]
    fn ranges_are_checked_when_read() {
        assert!("10.0.0.0/33".parse::<AddressRange>().is_err());
        assert!("::/129".parse::<AddressRange>().is_err());
        assert!("host.example".parse::<AddressRange>().is_err());
        assert_eq!(range(" 10.0.0.0/8 ").to_string(), "10.0.0.0/8");
    }

    #[test]
    fn only_listed_peers_are_trusted() {
        let loopback = AddressRange::loopback();
        assert!(trusted(&loopback, Some(ip("127.0.0.1"))));
        assert!(trusted(&loopback, Some(ip("::1"))));
        assert!(!trusted(&loopback, Some(ip("10.0.0.1"))));
        assert!(
            !trusted(&loopback, None),
            "an unknown peer is never trusted"
        );
    }

    #[test]
    fn the_client_is_named_by_trusted_proxies_only() {
        let proxies = vec![range("127.0.0.0/8"), range("10.0.0.0/8")];
        let forwarded = |value: &str| {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("x-forwarded-for", value.parse().unwrap());
            headers
        };
        let none = axum::http::HeaderMap::new();
        // A direct client is its peer address, whatever it says.
        let direct = Some(ip("203.0.113.7"));
        assert_eq!(client(&proxies, direct, &forwarded("1.2.3.4")), direct);
        // Through proxies: the last address that isn't one; what the client put before
        // it doesn't count.
        let proxy = Some(ip("127.0.0.1"));
        assert_eq!(
            client(
                &proxies,
                proxy,
                &forwarded("9.9.9.9, 198.51.100.2, 10.1.1.1")
            ),
            Some(ip("198.51.100.2"))
        );
        assert_eq!(
            client(&proxies, proxy, &forwarded("198.51.100.2:4711")),
            Some(ip("198.51.100.2"))
        );
        // Nothing forwarded, or an entry that isn't an address: the proxy itself.
        assert_eq!(client(&proxies, proxy, &none), proxy);
        assert_eq!(client(&proxies, proxy, &forwarded("unknown")), proxy);
        assert_eq!(client(&proxies, None, &forwarded("1.2.3.4")), None);
    }
}
