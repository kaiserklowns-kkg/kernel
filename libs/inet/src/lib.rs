//! Internet addresses (ADR-0023, ADR-0043): IPv4 and IPv6 as plain byte
//! arrays, an address of either family, and their text forms.
//!
//! Shared by the stack (`oceans-net`), the socket protocol
//! (`oceans-net-proto`) and every program that takes an address from a
//! user. No allocation and no I/O, so anything can use it.
//!
//! Text is untrusted input: parsing is strict (RFC 4291 §2.2, dotted quads
//! of exactly four decimal parts) and never panics. Output follows
//! RFC 5952: lowercase, leading zeros dropped, the longest run of two or
//! more zero groups shortened to `::`.

#![no_std]

use core::fmt;

pub type Ipv4 = [u8; 4];
pub type Ipv6 = [u8; 16];

pub const IPV4_UNSPECIFIED: Ipv4 = [0; 4];
pub const IPV6_UNSPECIFIED: Ipv6 = [0; 16];
/// `::1`.
pub const IPV6_LOOPBACK: Ipv6 = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

/// An address of either family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IpAddr {
    V4(Ipv4),
    V6(Ipv6),
}

impl From<Ipv4> for IpAddr {
    fn from(address: Ipv4) -> Self {
        Self::V4(address)
    }
}

impl From<Ipv6> for IpAddr {
    fn from(address: Ipv6) -> Self {
        Self::V6(address)
    }
}

impl IpAddr {
    /// IPv6 form: IPv4 addresses as IPv4-mapped (`::ffff:a.b.c.d`,
    /// RFC 4291 §2.5.5.2), for protocols that carry one 16-byte field.
    pub fn to_mapped(self) -> Ipv6 {
        match self {
            Self::V4(address) => v4_mapped(address),
            Self::V6(address) => address,
        }
    }

    /// The inverse of [`to_mapped`](Self::to_mapped): IPv4-mapped
    /// addresses become IPv4 again.
    pub fn from_mapped(address: Ipv6) -> Self {
        match mapped_v4(address) {
            Some(v4) => Self::V4(v4),
            None => Self::V6(address),
        }
    }

    pub fn is_unspecified(self) -> bool {
        match self {
            Self::V4(address) => address == IPV4_UNSPECIFIED,
            Self::V6(address) => address == IPV6_UNSPECIFIED,
        }
    }

    pub fn is_v6(self) -> bool {
        matches!(self, Self::V6(_))
    }
}

/// `::ffff:a.b.c.d`.
pub fn v4_mapped(address: Ipv4) -> Ipv6 {
    let mut out = [0u8; 16];
    out[10] = 0xff;
    out[11] = 0xff;
    out[12..].copy_from_slice(&address);
    out
}

/// The IPv4 address inside an IPv4-mapped IPv6 address.
pub fn mapped_v4(address: Ipv6) -> Option<Ipv4> {
    (address[..10] == [0; 10] && address[10..12] == [0xff, 0xff])
        .then(|| [address[12], address[13], address[14], address[15]])
}

/// `ff00::/8`.
pub fn is_multicast(address: &Ipv6) -> bool {
    address[0] == 0xff
}

/// `fe80::/10`: valid only on one link (RFC 4291 §2.5.6).
pub fn is_link_local(address: &Ipv6) -> bool {
    address[0] == 0xfe && address[1] & 0xc0 == 0x80
}

/// Multicast scoped to one link (`ff02::/16`, and the interface-local
/// `ff01::/16`, which never leaves the node).
pub fn is_link_scope_multicast(address: &Ipv6) -> bool {
    address[0] == 0xff && matches!(address[1] & 0x0f, 1 | 2)
}

/// Whether `a` and `b` share their first `prefix` bits.
pub fn same_prefix(a: &Ipv6, b: &Ipv6, prefix: u8) -> bool {
    let prefix = usize::from(prefix.min(128));
    let (bytes, bits) = (prefix / 8, prefix % 8);
    if a[..bytes] != b[..bytes] {
        return false;
    }
    bits == 0 || (a[bytes] ^ b[bytes]) >> (8 - bits) == 0
}

/// The number of leading bits `a` and `b` share.
pub fn common_prefix_len(a: &Ipv6, b: &Ipv6) -> u8 {
    let mut len = 0u8;
    for (x, y) in a.iter().zip(b) {
        let diff = x ^ y;
        if diff != 0 {
            return len + diff.leading_zeros() as u8;
        }
        len += 8;
    }
    len
}

/// Parses dotted-quad text (`10.0.2.2`): exactly four decimal parts of at
/// most three digits, each at most 255.
pub fn parse_ipv4(text: &str) -> Option<Ipv4> {
    let mut address = [0u8; 4];
    let mut parts = text.split('.');
    for byte in &mut address {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *byte = part.parse().ok()?;
    }
    parts.next().is_none().then_some(address)
}

/// Parses IPv6 text (RFC 4291 §2.2): eight groups of one to four hex
/// digits, at most one `::` standing for one or more zero groups, and
/// optionally a dotted quad as the last 32 bits. Zone indices (`%eth0`)
/// are not accepted: there is one interface.
pub fn parse_ipv6(text: &str) -> Option<Ipv6> {
    if text.len() > 45 || !text.contains(':') {
        return None;
    }
    let (head, tail) = match text.split_once("::") {
        Some((head, tail)) => {
            if tail.contains("::") {
                return None;
            }
            (head, Some(tail))
        }
        None => (text, None),
    };
    let mut groups = [0u16; 8];
    let mut head_len = 0;
    let mut tail_groups = [0u16; 8];
    let mut tail_len = 0;
    // Fills `out` from colon-separated text; a dotted quad may end the
    // whole address (the last part of the tail, or of the head without a
    // tail).
    let parse_part = |part: &str, out: &mut [u16; 8], len: &mut usize, last: bool| -> bool {
        if part.is_empty() {
            return true;
        }
        let pieces = part.split(':').count();
        for (index, piece) in part.split(':').enumerate() {
            if last && index + 1 == pieces && piece.contains('.') {
                let Some(v4) = parse_ipv4(piece) else {
                    return false;
                };
                if *len + 2 > 8 {
                    return false;
                }
                out[*len] = u16::from_be_bytes([v4[0], v4[1]]);
                out[*len + 1] = u16::from_be_bytes([v4[2], v4[3]]);
                *len += 2;
                continue;
            }
            if piece.is_empty() || piece.len() > 4 || !piece.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return false;
            }
            if *len >= 8 {
                return false;
            }
            let Ok(value) = u16::from_str_radix(piece, 16) else {
                return false;
            };
            out[*len] = value;
            *len += 1;
        }
        true
    };
    if !parse_part(head, &mut groups, &mut head_len, tail.is_none()) {
        return None;
    }
    match tail {
        None => {
            if head_len != 8 {
                return None;
            }
        }
        Some(tail) => {
            if !parse_part(tail, &mut tail_groups, &mut tail_len, true) {
                return None;
            }
            // `::` stands for at least one group.
            if head_len + tail_len > 7 {
                return None;
            }
            groups[8 - tail_len..].copy_from_slice(&tail_groups[..tail_len]);
        }
    }
    let mut address = [0u8; 16];
    for (bytes, group) in address.as_chunks_mut::<2>().0.iter_mut().zip(groups) {
        *bytes = group.to_be_bytes();
    }
    Some(address)
}

/// Parses an address of either family; IPv6 may be in brackets
/// (`[fec0::2]`), as in URLs.
pub fn parse_ip(text: &str) -> Option<IpAddr> {
    if let Some(inner) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        return parse_ipv6(inner).map(IpAddr::V6);
    }
    parse_ipv4(text)
        .map(IpAddr::V4)
        .or_else(|| parse_ipv6(text).map(IpAddr::V6))
}

/// Splits `HOST[:PORT]` text: `name:53`, `10.0.2.2:53`, `[fec0::2]:53`,
/// or without a port (`fec0::2` is all host: an IPv6 address needs
/// brackets to carry a port). Brackets are removed from the host. `None`
/// if the port is not a number or the brackets do not close.
pub fn split_host_port(text: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        return match after {
            "" => Some((host, None)),
            port => Some((host, Some(port.strip_prefix(':')?.parse().ok()?))),
        };
    }
    match text.split_once(':') {
        Some((host, port)) if !port.contains(':') => Some((host, Some(port.parse().ok()?))),
        _ => Some((text, None)),
    }
}

/// `a.b.c.d` for display.
pub struct Dotted(pub Ipv4);

impl fmt::Display for Dotted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

/// An IPv6 address in RFC 5952 form for display.
pub struct Colons(pub Ipv6);

impl fmt::Display for Colons {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let address = self.0;
        if let Some(v4) = mapped_v4(address) {
            return write!(f, "::ffff:{}", Dotted(v4));
        }
        let groups: [u16; 8] =
            core::array::from_fn(|i| u16::from_be_bytes([address[2 * i], address[2 * i + 1]]));
        // The longest run of two or more zero groups (the first on a tie).
        let (mut best, mut best_len) = (8, 0);
        let mut at = 0;
        while at < 8 {
            if groups[at] == 0 {
                let start = at;
                while at < 8 && groups[at] == 0 {
                    at += 1;
                }
                if at - start > best_len && at - start >= 2 {
                    (best, best_len) = (start, at - start);
                }
            } else {
                at += 1;
            }
        }
        let mut index = 0;
        while index < 8 {
            if index == best {
                f.write_str("::")?;
                index += best_len;
                continue;
            }
            if index > 0 && index != best + best_len {
                f.write_str(":")?;
            }
            write!(f, "{:x}", groups[index])?;
            index += 1;
        }
        Ok(())
    }
}

impl fmt::Display for IpAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::V4(address) => Dotted(address).fmt(f),
            Self::V6(address) => Colons(address).fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::format;
    use std::string::ToString;

    fn v6(text: &str) -> Ipv6 {
        parse_ipv6(text).unwrap_or_else(|| panic!("{text} should parse"))
    }

    #[test]
    fn parses_ipv4() {
        assert_eq!(parse_ipv4("10.0.2.2"), Some([10, 0, 2, 2]));
        assert_eq!(parse_ipv4("255.255.255.255"), Some([255; 4]));
        for bad in [
            "",
            "1.2.3",
            "1.2.3.4.5",
            "256.1.1.1",
            "1..2.3",
            "a.b.c.d",
            "1.2.3.0004",
        ] {
            assert_eq!(parse_ipv4(bad), None, "{bad}");
        }
    }

    #[test]
    fn parses_ipv6_forms() {
        assert_eq!(v6("::"), [0; 16]);
        assert_eq!(v6("::1"), IPV6_LOOPBACK);
        let router = v6("fe80::2");
        assert_eq!(router[..2], [0xfe, 0x80]);
        assert_eq!(router[15], 2);
        assert_eq!(router[2..15], [0; 13]);
        assert_eq!(
            v6("2001:db8:0:0:1:0:0:1"),
            v6("2001:DB8::1:0:0:1"),
            "case and compression"
        );
        assert_eq!(v6("fec0::"), {
            let mut a = [0u8; 16];
            a[..2].copy_from_slice(&[0xfe, 0xc0]);
            a
        });
        assert_eq!(v6("::ffff:10.0.2.2"), v4_mapped([10, 0, 2, 2]));
        assert_eq!(v6("1:2:3:4:5:6:1.2.3.4")[12..], [1, 2, 3, 4]);
        assert_eq!(v6("1:2:3:4:5:6:7:8")[14..], [0, 8]);
        assert_eq!(v6("1::8"), v6("1:0:0:0:0:0:0:8"));
        assert_eq!(v6("1:2:3:4:5:6:7::"), v6("1:2:3:4:5:6:7:0"));
    }

    #[test]
    fn rejects_bad_ipv6() {
        for bad in [
            "",
            ":",
            ":::",
            "1:2:3:4:5:6:7",
            "1:2:3:4:5:6:7:8:9",
            "1::2::3",
            "12345::",
            "g::1",
            "1:2:3:4:5:6:7:8::",
            "::1:2:3:4:5:6:7:8",
            "1.2.3.4",
            "::1.2.3",
            "::1.2.3.4:5",
            "fe80::1%eth0",
            ":1::",
            "1:",
            "1:2:3:4:5:6:7:1.2.3.4",
        ] {
            assert_eq!(parse_ipv6(bad), None, "{bad}");
        }
    }

    #[test]
    fn formats_per_rfc_5952() {
        for (text, expected) in [
            ("::", "::"),
            ("::1", "::1"),
            ("fe80:0:0:0:0:0:0:2", "fe80::2"),
            (
                "2001:0db8:0000:0000:0001:0000:0000:0001",
                "2001:db8::1:0:0:1",
            ),
            ("2001:db8:0:1:1:1:1:1", "2001:db8:0:1:1:1:1:1"),
            ("2001:db8:0:0:1:0:0:0", "2001:db8:0:0:1::"),
            ("2001:db8:0:0:1:0:0:1", "2001:db8::1:0:0:1"),
            ("1:0:0:2:0:0:0:3", "1:0:0:2::3"),
            ("FEC0::5054:FF:FE12:3456", "fec0::5054:ff:fe12:3456"),
            ("1:2:3:4:5:6:7::", "1:2:3:4:5:6:7:0"),
            ("::ffff:10.0.2.2", "::ffff:10.0.2.2"),
        ] {
            assert_eq!(Colons(v6(text)).to_string(), expected, "{text}");
            // The output parses back to the same address.
            assert_eq!(v6(expected), v6(text));
        }
        assert_eq!(IpAddr::V4([10, 0, 2, 15]).to_string(), "10.0.2.15");
    }

    #[test]
    fn every_address_round_trips() {
        // Groups chosen to make zero runs of every length and position.
        for mask in 0u16..256 {
            let mut address = [0u8; 16];
            for group in 0..8 {
                if mask & (1 << group) != 0 {
                    address[2 * group..2 * group + 2]
                        .copy_from_slice(&(0x1234u16 + group as u16).to_be_bytes());
                }
            }
            let text = format!("{}", Colons(address));
            assert_eq!(parse_ipv6(&text), Some(address), "{text}");
        }
    }

    #[test]
    fn mixed_family_helpers() {
        assert_eq!(parse_ip("10.0.2.2"), Some(IpAddr::V4([10, 0, 2, 2])));
        assert_eq!(parse_ip("[fec0::2]"), Some(IpAddr::V6(v6("fec0::2"))));
        assert_eq!(parse_ip("fec0::2"), Some(IpAddr::V6(v6("fec0::2"))));
        assert_eq!(parse_ip("[10.0.2.2]"), None);
        assert_eq!(parse_ip("oceans.test"), None);
        let mapped = IpAddr::V4([10, 0, 2, 2]).to_mapped();
        assert_eq!(IpAddr::from_mapped(mapped), IpAddr::V4([10, 0, 2, 2]));
        assert_eq!(
            IpAddr::from_mapped(IPV6_LOOPBACK),
            IpAddr::V6(IPV6_LOOPBACK)
        );
        assert!(is_link_local(&v6("fe80::1")));
        assert!(is_link_local(&v6("febf::1")));
        assert!(!is_link_local(&v6("fec0::1")));
        assert!(is_multicast(&v6("ff02::1")));
        assert!(is_link_scope_multicast(&v6("ff02::1:ff00:1")));
        assert!(!is_link_scope_multicast(&v6("ff05::2")));
    }

    #[test]
    fn prefixes() {
        let a = v6("fec0::5054:ff:fe12:3456");
        assert!(same_prefix(&a, &v6("fec0::"), 64));
        assert!(!same_prefix(&a, &v6("fec1::"), 16));
        assert!(same_prefix(&a, &v6("fec1::"), 15));
        assert!(same_prefix(&a, &v6("::"), 0));
        assert_eq!(common_prefix_len(&a, &a), 128);
        assert_eq!(common_prefix_len(&v6("fec0::"), &v6("fec1::")), 15);
        assert_eq!(common_prefix_len(&v6("8000::"), &v6("::")), 0);
    }

    #[test]
    fn splits_hosts_and_ports() {
        assert_eq!(split_host_port("10.0.2.2:53"), Some(("10.0.2.2", Some(53))));
        assert_eq!(split_host_port("[fec0::2]:53"), Some(("fec0::2", Some(53))));
        assert_eq!(split_host_port("[fec0::2]"), Some(("fec0::2", None)));
        assert_eq!(split_host_port("fec0::2"), Some(("fec0::2", None)));
        assert_eq!(split_host_port("name"), Some(("name", None)));
        assert_eq!(split_host_port("name:x"), None);
        assert_eq!(split_host_port("[fec0::2"), None);
        assert_eq!(split_host_port("[fec0::2]53"), None);
        assert_eq!(split_host_port("name:99999"), None);
    }
}
