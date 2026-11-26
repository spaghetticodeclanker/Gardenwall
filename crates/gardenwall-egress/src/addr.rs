//! Public-address rules (ADR-007 §10.3).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is a globally routable address that web tools may contact.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // 169.254/16, including cloud metadata
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0 // "this network"
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // benchmarking 198.18/15
        || a >= 240) // reserved 240/4
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    // Embedded IPv4 forms are judged by the IPv4 address they reach.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    // NAT64 well-known prefix 64:ff9b::/96 reaches an IPv4 address.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return is_public_v4(v4);
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // deprecated site-local
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x2001 && s[1] < 0x0200) // IETF protocol assignments, incl. Teredo
        || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1) // local-use NAT64 64:ff9b:1::/48 (RFC 8215)
        || s[0] == 0x2002 // 6to4 can reach private IPv4
        || s[0] == 0x0100 && s[1..4] == [0, 0, 0] // discard-only 100::/64
        || s[0] == 0) // IPv4-compatible and other ::/16 forms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> bool {
        is_public(s.parse().unwrap())
    }

    #[test]
    fn private_and_special_ranges_are_refused() {
        for blocked in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "192.0.0.8",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b::7f00:1",
            "64:ff9b:1::a00:1",
            "64:ff9b:1:ffff::1",
            "2001:db8::1",
            "2001::1",
            "2002:c0a8:101::1",
            "ff02::1",
            "::127.0.0.1",
        ] {
            assert!(!ok(blocked), "{blocked} must be refused");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for allowed in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
            "64:ff9b::101:101",
        ] {
            assert!(ok(allowed), "{allowed} should be allowed");
        }
    }
}
