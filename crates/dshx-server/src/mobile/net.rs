//! The machine's addresses, sorted the way a phone should try them: LAN,
//! then Tailscale, then public ones. Loopback and link-local addresses stay
//! out of the URLs a payload carries.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::Serialize;

/// Where an address lives; the order is the order a phone tries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Lan,
    Tailscale,
    Public,
    Other,
    Loopback,
}

#[derive(Debug, Clone, Serialize)]
pub struct Address {
    pub ip: IpAddr,
    pub interface: String,
    pub kind: Kind,
}

/// `127/8` and `::1` loopback; `10/8`, `172.16/12`, `192.168/16`, `fe80::/10`
/// and `fc00::/7` (unique local) lan; `100.64/10` and `fd7a:115c:a1e0::/48`
/// tailscale; the rest public (link-local IPv4 and non-unicast addresses:
/// other).
pub fn kind_of(ip: IpAddr) -> Kind {
    match ip {
        IpAddr::V4(v4) => kind_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => kind_v4(v4),
            None => kind_v6(v6),
        },
    }
}

fn kind_v4(ip: Ipv4Addr) -> Kind {
    let [a, b, _, _] = ip.octets();
    if ip.is_loopback() {
        Kind::Loopback
    } else if a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
        Kind::Lan
    } else if a == 100 && (64..=127).contains(&b) {
        Kind::Tailscale
    } else if ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() {
        Kind::Other
    } else {
        Kind::Public
    }
}

fn kind_v6(ip: Ipv6Addr) -> Kind {
    let [first, second, third, ..] = ip.segments();
    if ip.is_loopback() {
        Kind::Loopback
    } else if [first, second, third] == [0xfd7a, 0x115c, 0xa1e0] {
        // Tailscale's IPv6 range, fd7a:115c:a1e0::/48.
        Kind::Tailscale
    } else if first & 0xffc0 == 0xfe80 || first & 0xfe00 == 0xfc00 {
        Kind::Lan
    } else if ip.is_unspecified() || ip.is_multicast() {
        Kind::Other
    } else {
        Kind::Public
    }
}

/// Whether another machine could use a URL with this address (IPv6
/// link-local needs a zone the phone does not have).
fn usable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => kind_v4(v4) != Kind::Other,
        IpAddr::V6(v6) => kind_v6(v6) != Kind::Other && v6.segments()[0] & 0xffc0 != 0xfe80,
    }
}

/// Every address of this machine, best first (LAN, Tailscale, public,
/// loopback; IPv4 before IPv6 within a kind). Link-local and non-unicast
/// addresses are left out.
pub fn addresses() -> Vec<Address> {
    let mut out: Vec<Address> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| {
            let ip = i.addr.ip();
            Address {
                ip,
                interface: i.name,
                kind: kind_of(ip),
            }
        })
        .filter(|a| usable(a.ip))
        .collect();
    out.sort_by_key(|a| (a.kind, a.ip.is_ipv6(), a.ip));
    out.dedup_by(|a, b| a.ip == b.ip);
    out
}

/// `https://<host>:<port>` for every address a phone could reach: all but
/// the loopback ones, in the order of `addresses`.
pub fn urls(addresses: &[Address], port: u16) -> Vec<String> {
    addresses
        .iter()
        .filter(|a| a.kind != Kind::Loopback)
        .map(|a| format!("https://{}:{port}", url_host(a.ip)))
        .collect()
}

/// An address as a URL host: IPv6 in brackets.
pub fn url_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(s: &str) -> Kind {
        kind_of(s.parse().unwrap())
    }

    #[test]
    fn kinds_by_range() {
        assert_eq!(kind("127.0.0.1"), Kind::Loopback);
        assert_eq!(kind("::1"), Kind::Loopback);
        assert_eq!(kind("10.1.2.3"), Kind::Lan);
        assert_eq!(kind("172.16.0.9"), Kind::Lan);
        assert_eq!(kind("172.32.0.9"), Kind::Public);
        assert_eq!(kind("192.168.178.20"), Kind::Lan);
        assert_eq!(kind("fe80::1"), Kind::Lan);
        assert_eq!(kind("fd7a:115c:a1e0::1"), Kind::Tailscale);
        assert_eq!(kind("fc00::6440:1"), Kind::Lan);
        assert_eq!(kind("100.79.232.37"), Kind::Tailscale);
        assert_eq!(kind("100.128.0.1"), Kind::Public);
        assert_eq!(kind("130.162.241.44"), Kind::Public);
        assert_eq!(kind("2001:db8::1"), Kind::Public);
        assert_eq!(kind("169.254.1.1"), Kind::Other);
        assert_eq!(kind("::ffff:192.168.1.1"), Kind::Lan);
    }

    #[test]
    fn urls_skip_loopback_and_bracket_ipv6() {
        let address = |ip: &str, kind| Address {
            ip: ip.parse().unwrap(),
            interface: "en0".into(),
            kind,
        };
        let list = vec![
            address("192.168.1.2", Kind::Lan),
            address("fd00::2", Kind::Lan),
            address("127.0.0.1", Kind::Loopback),
        ];
        assert_eq!(
            urls(&list, 7443),
            vec!["https://192.168.1.2:7443", "https://[fd00::2]:7443"]
        );
        let mine = addresses();
        assert!(mine.iter().all(|a| !a.ip.to_string().starts_with("fe80")));
        assert!(
            mine.windows(2).all(|w| w[0].kind <= w[1].kind),
            "sorted by kind"
        );
    }
}
