//! [`IpNet`]: one IP network in strict CIDR form, as the server's
//! `[http] trusted_proxies` and the agent's `[control_plane]
//! private_targets` list them (ADR-0016, ADR-0019 S69).
//!
//! Addresses are compared in their canonical form
//! ([`IpAddr::to_canonical`]): a dual-stack `[::]` listener reports an IPv4
//! peer as `::ffff:a.b.c.d`, which must still match an IPv4 network.

use core::fmt;
use core::str::FromStr;
use std::net::{IpAddr, Ipv4Addr};

/// One IP network in CIDR form (`172.30.0.0/24`, `2001:db8::/32`).
///
/// Parsing is strict, so a typo can't silently cover the wrong addresses:
/// the prefix length is required and at least 1 (a /0 would cover every
/// address of its family), the address must be the network's first (no host
/// bits set), and an IPv4 network must be written in IPv4 form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpNet {
    address: IpAddr,
    prefix_len: u8,
}

/// Why text isn't an [`IpNet`]. No message echoes the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IpNetError {
    /// Not an IP address, a `/` and a prefix length.
    #[error("must be an IP network in CIDR form, like 172.30.0.0/24 or 2001:db8::/32")]
    Shape,
    /// A prefix length beyond the address family's bits.
    #[error("the prefix length can be at most {max} for this address family")]
    PrefixLength {
        /// 32 for IPv4, 128 for IPv6.
        max: u8,
    },
    /// The address has bits set after the prefix.
    #[error("has bits set after the prefix length; write the network's first address")]
    HostBits,
    /// An IPv4 network written as IPv4-mapped IPv6 (`::ffff:…`), which no
    /// canonical address would ever match.
    #[error("an IPv4 network must be written in IPv4 form, like 172.30.0.0/24")]
    MappedIpv4,
    /// A prefix length of 0 (`0.0.0.0/0`, `::/0`): every address of its
    /// family. A list that means "everything" says so by other means, if at
    /// all; each caller words what it would open.
    #[error("a prefix length of 0 would cover every address of its family")]
    Everything,
}

impl IpNet {
    /// The network of `address` and `prefix_len` as they are, for this
    /// crate's own tables; every one of them is pinned by a test that parses
    /// its text back.
    pub(crate) const fn from_parts(address: IpAddr, prefix_len: u8) -> Self {
        Self {
            address,
            prefix_len,
        }
    }

    /// The network's first address.
    #[must_use]
    pub const fn address(&self) -> IpAddr {
        self.address
    }

    /// The prefix length.
    #[must_use]
    pub const fn prefix_len(&self) -> u8 {
        self.prefix_len
    }

    /// Whether `ip`, in its canonical form, is in the network.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.address, ip.to_canonical()) {
            (IpAddr::V4(network), IpAddr::V4(ip)) => {
                bits_v4(ip) & mask::<32>(self.prefix_len) == bits_v4(network)
            }
            (IpAddr::V6(network), IpAddr::V6(ip)) => {
                u128::from(ip) & mask::<128>(self.prefix_len) == u128::from(network)
            }
            (IpAddr::V4(_), IpAddr::V6(_)) | (IpAddr::V6(_), IpAddr::V4(_)) => false,
        }
    }

    /// Whether every address of `other` is in this network.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        self.prefix_len <= other.prefix_len && self.contains(other.address)
    }
}

impl FromStr for IpNet {
    type Err = IpNetError;

    fn from_str(text: &str) -> Result<Self, IpNetError> {
        let (address, length) = text.split_once('/').ok_or(IpNetError::Shape)?;
        // Only plain decimal digits, without a sign or a leading zero, so
        // each network has one way to be written.
        let canonical_length = !length.is_empty()
            && length.bytes().all(|byte| byte.is_ascii_digit())
            && (length == "0" || !length.starts_with('0'));
        if !canonical_length {
            return Err(IpNetError::Shape);
        }
        let address: IpAddr = address.parse().map_err(|_| IpNetError::Shape)?;
        let length: u32 = length.parse().map_err(|_| IpNetError::Shape)?;
        let (bits, max) = match address {
            IpAddr::V4(v4) => (bits_v4(v4), 32_u8),
            IpAddr::V6(v6) => {
                if v6.to_ipv4_mapped().is_some() {
                    return Err(IpNetError::MappedIpv4);
                }
                (u128::from(v6), 128_u8)
            }
        };
        let prefix_len = u8::try_from(length)
            .ok()
            .filter(|length| *length <= max)
            .ok_or(IpNetError::PrefixLength { max })?;
        if prefix_len == 0 {
            return Err(IpNetError::Everything);
        }
        let host_mask = match address {
            IpAddr::V4(_) => !mask::<32>(prefix_len) & mask::<32>(32),
            IpAddr::V6(_) => !mask::<128>(prefix_len),
        };
        if bits & host_mask != 0 {
            return Err(IpNetError::HostBits);
        }
        Ok(Self {
            address,
            prefix_len,
        })
    }
}

/// `address/prefix_len`.
impl fmt::Display for IpNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.address, self.prefix_len)
    }
}

/// An IPv4 address's bits, in the low 32 bits.
fn bits_v4(ip: Ipv4Addr) -> u128 {
    u128::from(u32::from(ip))
}

/// The mask of a `prefix_len`-bit prefix in a `BITS`-bit address.
pub(crate) fn mask<const BITS: u32>(prefix_len: u8) -> u128 {
    // All `BITS` bits set; shifting by 128 or more would overflow.
    let all = 1_u128.checked_shl(BITS).map_or(u128::MAX, |bit| bit - 1);
    let host = all.checked_shr(u32::from(prefix_len)).unwrap_or(0);
    all & !host
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn net(text: &str) -> IpNet {
        text.parse().unwrap()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[rstest]
    #[case::ipv4("192.0.2.0/24", "192.0.2.0", 24)]
    #[case::ipv4_host("198.51.100.7/32", "198.51.100.7", 32)]
    #[case::ipv4_shortest("128.0.0.0/1", "128.0.0.0", 1)]
    #[case::ipv6("2001:db8::/32", "2001:db8::", 32)]
    #[case::ipv6_host("2001:db8::1/128", "2001:db8::1", 128)]
    #[case::ipv6_shortest("8000::/1", "8000::", 1)]
    fn a_network_in_cidr_form_is_read(#[case] text: &str, #[case] address: &str, #[case] len: u8) {
        let parsed = net(text);

        assert_eq!(parsed.address(), ip(address));
        assert_eq!(parsed.prefix_len(), len);
        assert_eq!(parsed.to_string(), text);
    }

    #[rstest]
    #[case::bare_address("192.0.2.10", IpNetError::Shape)]
    #[case::empty("", IpNetError::Shape)]
    #[case::no_length("192.0.2.0/", IpNetError::Shape)]
    #[case::no_address("/24", IpNetError::Shape)]
    #[case::host_name("proxy.example.com/24", IpNetError::Shape)]
    #[case::two_slashes("192.0.2.0/24/8", IpNetError::Shape)]
    #[case::signed_length("192.0.2.0/+24", IpNetError::Shape)]
    #[case::leading_zero_length("192.0.2.0/024", IpNetError::Shape)]
    #[case::spaces(" 192.0.2.0/24", IpNetError::Shape)]
    #[case::ipv4_too_long("192.0.2.0/33", IpNetError::PrefixLength { max: 32 })]
    #[case::ipv6_too_long("2001:db8::/129", IpNetError::PrefixLength { max: 128 })]
    #[case::huge_length("192.0.2.0/300", IpNetError::PrefixLength { max: 32 })]
    #[case::ipv4_host_bits("192.0.2.5/24", IpNetError::HostBits)]
    #[case::ipv6_host_bits("2001:db8::1/64", IpNetError::HostBits)]
    #[case::mapped("::ffff:192.0.2.0/120", IpNetError::MappedIpv4)]
    #[case::mapped_host("::ffff:192.0.2.10/128", IpNetError::MappedIpv4)]
    #[case::ipv4_everything("0.0.0.0/0", IpNetError::Everything)]
    #[case::ipv6_everything("::/0", IpNetError::Everything)]
    fn a_network_that_isnt_strict_cidr_is_refused(#[case] text: &str, #[case] error: IpNetError) {
        assert_eq!(text.parse::<IpNet>(), Err(error), "{text}");
    }

    #[test]
    fn no_error_message_echoes_the_text() {
        for text in [
            "192.0.2.10",
            "192.0.2.0/33",
            "192.0.2.5/24",
            "::ffff:192.0.2.0/120",
            "0.0.0.0/0",
            "::/0",
        ] {
            let message = text.parse::<IpNet>().unwrap_err().to_string();

            assert!(!message.contains(text), "{message}");
        }
    }

    #[rstest]
    #[case::first("192.0.2.0/24", "192.0.2.0", true)]
    #[case::last("192.0.2.0/24", "192.0.2.255", true)]
    #[case::next("192.0.2.0/24", "192.0.3.0", false)]
    #[case::before("192.0.2.0/24", "192.0.1.255", false)]
    #[case::host("198.51.100.7/32", "198.51.100.7", true)]
    #[case::host_neighbour("198.51.100.7/32", "198.51.100.8", false)]
    #[case::upper_half_v4("128.0.0.0/1", "203.0.113.9", true)]
    #[case::lower_half_v4("0.0.0.0/1", "203.0.113.9", false)]
    #[case::v6_inside("2001:db8::/32", "2001:db8:ffff::1", true)]
    #[case::v6_outside("2001:db8::/32", "2001:db9::", false)]
    #[case::other_family_v4_in_v6("::/1", "192.0.2.10", false)]
    #[case::other_family_v6_in_v4("0.0.0.0/1", "2001:db8::1", false)]
    #[case::mapped_peer_in_v4("192.0.2.0/24", "::ffff:192.0.2.10", true)]
    #[case::mapped_peer_outside_v4("192.0.2.0/24", "::ffff:198.51.100.10", false)]
    #[case::mapped_peer_isnt_v6("::/1", "::ffff:192.0.2.10", false)]
    fn a_network_contains_its_addresses_in_canonical_form(
        #[case] network: &str,
        #[case] address: &str,
        #[case] inside: bool,
    ) {
        assert_eq!(net(network).contains(ip(address)), inside);
    }

    #[rstest]
    #[case::itself("192.0.2.0/24", "192.0.2.0/24", true)]
    #[case::a_part("10.0.0.0/8", "10.20.0.0/16", true)]
    #[case::a_host("fc00::/7", "fd00::1/128", true)]
    #[case::a_wider_one("10.20.0.0/16", "10.0.0.0/8", false)]
    #[case::a_neighbour("10.0.0.0/8", "11.0.0.0/8", false)]
    #[case::the_other_family("10.0.0.0/8", "fd00::/8", false)]
    fn a_network_covers_the_networks_inside_it(
        #[case] outer: &str,
        #[case] inner: &str,
        #[case] covered: bool,
    ) {
        assert_eq!(net(outer).covers(&net(inner)), covered);
    }

    #[test]
    fn a_zero_prefix_says_it_would_cover_every_address() {
        for text in ["0.0.0.0/0", "::/0"] {
            let message = text.parse::<IpNet>().unwrap_err().to_string();

            assert!(message.contains("every address"), "{message}");
        }
    }

    #[test]
    fn the_masks_cover_every_prefix_length() {
        assert_eq!(mask::<32>(0), 0);
        assert_eq!(mask::<32>(24), 0xFFFF_FF00);
        assert_eq!(mask::<32>(32), 0xFFFF_FFFF);
        assert_eq!(mask::<128>(0), 0);
        assert_eq!(mask::<128>(64), u128::from(u64::MAX) << 64);
        assert_eq!(mask::<128>(128), u128::MAX);
    }
}
