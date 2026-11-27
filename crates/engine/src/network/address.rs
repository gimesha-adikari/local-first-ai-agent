//! Address classification for destination scopes. Every resolved address is
//! checked before a connection; one unacceptable answer refuses the request.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressClass {
    Public,
    Loopback,
    /// Private, unique-local and shared address space.
    Private,
    LinkLocal,
    Unspecified,
    Multicast,
    /// Documentation, benchmarking, translation and other special ranges.
    Reserved,
}

/// Which addresses a destination may connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkScope {
    /// Globally routable addresses only.
    Public,
    /// This computer only.
    Loopback,
    /// Exactly these explicitly registered local-network addresses.
    Pinned(Vec<IpAddr>),
}

impl NetworkScope {
    pub(crate) fn permits(&self, address: IpAddr) -> bool {
        let address = canonical(address);
        let class = classify(address);
        match self {
            Self::Public => class == AddressClass::Public,
            Self::Loopback => class == AddressClass::Loopback,
            Self::Pinned(allowed) => {
                class == AddressClass::Private && allowed.iter().any(|ip| canonical(*ip) == address)
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Loopback => "loopback",
            Self::Pinned(_) => "local_network",
        }
    }
}

fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(address, IpAddr::V4),
        IpAddr::V4(_) => address,
    }
}

fn in_v4(ip: Ipv4Addr, network: [u8; 4], prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(ip) & mask) == (u32::from(Ipv4Addr::from(network)) & mask)
}

fn classify_v4(ip: Ipv4Addr) -> AddressClass {
    use AddressClass::*;
    let table: [([u8; 4], u32, AddressClass); 17] = [
        ([0, 0, 0, 0], 8, Unspecified),
        ([10, 0, 0, 0], 8, Private),
        ([100, 64, 0, 0], 10, Private),
        ([127, 0, 0, 0], 8, Loopback),
        ([169, 254, 0, 0], 16, LinkLocal),
        ([172, 16, 0, 0], 12, Private),
        ([192, 0, 0, 0], 24, Reserved),
        ([192, 0, 2, 0], 24, Reserved),
        ([192, 31, 196, 0], 24, Reserved),
        ([192, 52, 193, 0], 24, Reserved),
        ([192, 88, 99, 0], 24, Reserved),
        ([192, 168, 0, 0], 16, Private),
        ([192, 175, 48, 0], 24, Reserved),
        ([198, 18, 0, 0], 15, Reserved),
        ([198, 51, 100, 0], 24, Reserved),
        ([203, 0, 113, 0], 24, Reserved),
        ([224, 0, 0, 0], 4, Multicast),
    ];
    if in_v4(ip, [240, 0, 0, 0], 4) {
        return Reserved;
    }
    table
        .iter()
        .find(|(network, prefix, _)| in_v4(ip, *network, *prefix))
        .map_or(Public, |(_, _, class)| *class)
}

fn in_v6(ip: Ipv6Addr, network: [u16; 8], prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    (u128::from(ip) & mask) == (u128::from(Ipv6Addr::from(network)) & mask)
}

fn embedded_v4(bits: u128, shift: u32) -> Ipv4Addr {
    Ipv4Addr::from(((bits >> shift) & 0xffff_ffff) as u32)
}

fn classify_v6(ip: Ipv6Addr) -> AddressClass {
    use AddressClass::*;
    if ip.is_unspecified() {
        return Unspecified;
    }
    if ip.is_loopback() {
        return Loopback;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return classify_v4(v4);
    }
    let bits = u128::from(ip);
    if in_v6(ip, [0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96) {
        // NAT64 reaches the embedded IPv4 address.
        return classify_v4(embedded_v4(bits, 0));
    }
    if in_v6(ip, [0x2002, 0, 0, 0, 0, 0, 0, 0], 16) {
        // 6to4 tunnels to the embedded IPv4 address.
        return match classify_v4(embedded_v4(bits, 80)) {
            Public => Public,
            _ => Reserved,
        };
    }
    for (network, prefix, class) in [
        ([0u16, 0, 0, 0, 0, 0, 0, 0], 96u32, Reserved),
        ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48, Reserved),
        ([0x100, 0, 0, 0, 0, 0, 0, 0], 64, Reserved),
        ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23, Reserved),
        ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32, Reserved),
        ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20, Reserved),
        ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7, Private),
        ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10, LinkLocal),
        ([0xfec0, 0, 0, 0, 0, 0, 0, 0], 10, Private),
        ([0xff00, 0, 0, 0, 0, 0, 0, 0], 8, Multicast),
    ] {
        if in_v6(ip, network, prefix) {
            return class;
        }
    }
    if in_v6(ip, [0x2000, 0, 0, 0, 0, 0, 0, 0], 3) {
        Public
    } else {
        Reserved
    }
}

pub(crate) fn classify(address: IpAddr) -> AddressClass {
    match address {
        IpAddr::V4(ip) => classify_v4(ip),
        IpAddr::V6(ip) => classify_v6(ip),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AddressClass::*;

    fn class(text: &str) -> AddressClass {
        classify(text.parse().unwrap())
    }

    #[test]
    fn classifies_special_ranges() {
        for (address, expected) in [
            ("8.8.8.8", Public),
            ("1.1.1.1", Public),
            ("127.0.0.1", Loopback),
            ("127.255.0.9", Loopback),
            ("10.1.2.3", Private),
            ("172.16.0.1", Private),
            ("172.31.255.255", Private),
            ("172.32.0.1", Public),
            ("192.168.0.10", Private),
            ("100.64.0.1", Private),
            ("169.254.169.254", LinkLocal),
            ("0.0.0.0", Unspecified),
            ("224.0.0.251", Multicast),
            ("255.255.255.255", Reserved),
            ("198.18.0.1", Reserved),
            ("203.0.113.9", Reserved),
            ("::1", Loopback),
            ("::", Unspecified),
            ("::ffff:127.0.0.1", Loopback),
            ("::ffff:10.0.0.1", Private),
            ("::ffff:8.8.8.8", Public),
            ("64:ff9b::a00:1", Private),
            ("64:ff9b::808:808", Public),
            ("2002:a00:1::1", Reserved),
            ("2002:808:808::1", Public),
            ("2001::1", Reserved),
            ("2001:db8::1", Reserved),
            ("fd00::1", Private),
            ("fe80::1", LinkLocal),
            ("ff02::1", Multicast),
            ("2606:4700::1111", Public),
            ("::a00:1", Reserved),
        ] {
            assert_eq!(class(address), expected, "{address}");
        }
    }

    #[test]
    fn scopes_admit_only_their_class_and_pins() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        assert!(NetworkScope::Public.permits(ip("8.8.8.8")));
        assert!(!NetworkScope::Public.permits(ip("127.0.0.1")));
        assert!(!NetworkScope::Public.permits(ip("10.0.0.1")));
        assert!(!NetworkScope::Public.permits(ip("::ffff:192.168.1.1")));
        assert!(NetworkScope::Loopback.permits(ip("127.0.0.1")));
        assert!(NetworkScope::Loopback.permits(ip("::1")));
        assert!(!NetworkScope::Loopback.permits(ip("10.0.0.1")));
        let pinned = NetworkScope::Pinned(vec![ip("192.168.1.20")]);
        assert!(pinned.permits(ip("192.168.1.20")));
        assert!(pinned.permits(ip("::ffff:192.168.1.20")));
        assert!(!pinned.permits(ip("192.168.1.21")));
        assert!(!pinned.permits(ip("127.0.0.1")));
        // A pin can only hold a private address.
        assert!(!NetworkScope::Pinned(vec![ip("8.8.8.8")]).permits(ip("8.8.8.8")));
    }
}
