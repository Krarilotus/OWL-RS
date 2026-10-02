//! Which peers may vouch for a client: the proxies that terminate TLS and pass the client
//! certificate's subject on in a header (the `mtls` mode). A header from any other peer is
//! a claim anyone could make, so it is dropped before authentication sees it.

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
}
