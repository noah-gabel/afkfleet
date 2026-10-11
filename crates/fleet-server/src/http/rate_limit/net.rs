//! IP networks (Plan.md P7.10, ADR-0016): [`IpNet`], one CIDR network of
//! `[http] trusted_proxies`; [`TrustedProxies`], the whole list; and
//! [`limiter_key`], the address a per-IP limit counts a client under.
//!
//! Addresses are compared in their canonical form
//! ([`IpAddr::to_canonical`]): a dual-stack `[::]` listener reports an IPv4
//! peer as `::ffff:a.b.c.d`, which must still match an IPv4 network, or a
//! trusted proxy would count as untrusted and every client behind it would
//! share its one bucket.

use core::fmt;
use core::str::FromStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// One IP network in CIDR form (`172.30.0.0/24`, `2001:db8::/32`), as
/// `[http] trusted_proxies` lists them.
///
/// Parsing is strict, so a typo can't silently trust the wrong addresses:
/// the prefix length is required and at least 1 (a /0 would trust every
/// address), the address must be the network's first (no host bits set),
/// and an IPv4 network must be written in IPv4 form.
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
    /// family. Trusting every address would let any client choose its own
    /// address through `X-Forwarded-For`, past every per-IP limit and into
    /// the audit log.
    #[error("a prefix length of 0 would trust every address; list only your proxies' own networks")]
    Everything,
}

impl IpNet {
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

/// `[http] trusted_proxies`: the networks whose `X-Forwarded-For` the server
/// believes. Empty by default: then nobody's is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    /// The most networks the config may list.
    pub const MAX: usize = 32;

    /// The proxies in `networks`.
    #[must_use]
    pub const fn new(networks: Vec<IpNet>) -> Self {
        Self(networks)
    }

    /// The networks.
    #[must_use]
    pub fn networks(&self) -> &[IpNet] {
        &self.0
    }

    /// Whether `ip`, in its canonical form, is a trusted proxy.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|network| network.contains(ip))
    }
}

/// The address a per-IP limit counts `ip` under: its canonical form, and for
/// IPv6 its /64, since one host usually gets a whole /64 (ADR-0016). Only
/// the limiter's key is masked; the audit log records the full address.
#[must_use]
pub fn limiter_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask::<128>(64))),
    }
}

/// An IPv4 address's bits, in the low 32 bits.
fn bits_v4(ip: Ipv4Addr) -> u128 {
    u128::from(u32::from(ip))
}

/// The mask of a `prefix_len`-bit prefix in a `BITS`-bit address.
fn mask<const BITS: u32>(prefix_len: u8) -> u128 {
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

    #[test]
    fn a_trusted_ipv4_proxy_seen_as_a_mapped_ipv6_peer_is_trusted() {
        let proxies = TrustedProxies::new(vec![net("192.0.2.0/24")]);

        assert!(proxies.contains(ip("::ffff:192.0.2.7")));
        assert!(!proxies.contains(ip("::ffff:198.51.100.7")));
    }

    #[test]
    fn no_proxies_trust_nobody() {
        assert!(!TrustedProxies::default().contains(ip("192.0.2.1")));
    }

    #[test]
    fn any_listed_network_counts() {
        let proxies = TrustedProxies::new(vec![net("192.0.2.0/24"), net("2001:db8::/32")]);

        assert!(proxies.contains(ip("192.0.2.200")));
        assert!(proxies.contains(ip("2001:db8::7")));
        assert!(!proxies.contains(ip("198.51.100.1")));
        assert_eq!(proxies.networks().len(), 2);
    }

    #[rstest]
    #[case::ipv4_is_itself("192.0.2.10", "192.0.2.10")]
    #[case::ipv6_by_its_64("2001:db8:1:2:aaaa:bbbb:cccc:dddd", "2001:db8:1:2::")]
    #[case::ipv6_already_masked("2001:db8:1:2::", "2001:db8:1:2::")]
    #[case::mapped_is_ipv4("::ffff:192.0.2.10", "192.0.2.10")]
    fn a_client_is_counted_by_its_address_or_its_64(#[case] address: &str, #[case] key: &str) {
        assert_eq!(limiter_key(ip(address)), ip(key));
    }

    #[test]
    fn two_addresses_in_one_64_share_a_key_and_the_next_64_doesnt() {
        let first = limiter_key(ip("2001:db8:0:1::1"));

        assert_eq!(first, limiter_key(ip("2001:db8:0:1:ffff:ffff:ffff:ffff")));
        assert_ne!(first, limiter_key(ip("2001:db8:0:2::1")));
    }

    /// Cross-checks against ipnet, an independent implementation (a dev-only
    /// dependency, ADR-0016). Networks are built from ipnet's own text, so
    /// parsing is checked too. A mapped address is canonicalized to IPv4 here,
    /// so it's left out of the IPv6 comparison, which ipnet doesn't
    /// canonicalize; its own cases are above.
    mod against_ipnet {
        use ipnet::{Ipv4Net, Ipv6Net};
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn ipv4_networks_contain_what_ipnet_says(
                network: u32,
                prefix_len in 1_u8..=32,
                address: u32,
            ) {
                let theirs = Ipv4Net::new(Ipv4Addr::from(network), prefix_len).unwrap().trunc();
                let ours: IpNet = theirs.to_string().parse().unwrap();
                let address = Ipv4Addr::from(address);

                prop_assert_eq!(ours.address(), IpAddr::V4(theirs.network()));
                prop_assert_eq!(ours.contains(IpAddr::V4(address)), theirs.contains(&address));
                prop_assert!(ours.contains(IpAddr::V4(theirs.network())));
                prop_assert!(ours.contains(IpAddr::V4(theirs.broadcast())));
            }

            #[test]
            fn ipv6_networks_contain_what_ipnet_says(
                network: u128,
                prefix_len in 1_u8..=128,
                address: u128,
            ) {
                let theirs = Ipv6Net::new(Ipv6Addr::from(network), prefix_len).unwrap().trunc();
                prop_assume!(theirs.network().to_ipv4_mapped().is_none());
                let address = Ipv6Addr::from(address);
                prop_assume!(address.to_ipv4_mapped().is_none());
                let ours: IpNet = theirs.to_string().parse().unwrap();

                prop_assert_eq!(ours.address(), IpAddr::V6(theirs.network()));
                prop_assert_eq!(ours.contains(IpAddr::V6(address)), theirs.contains(&address));
            }

            #[test]
            fn an_ipv6_clients_key_is_its_64_as_ipnet_truncates_it(address: u128) {
                let address = Ipv6Addr::from(address);
                prop_assume!(address.to_ipv4_mapped().is_none());
                let theirs = Ipv6Net::new(address, 64).unwrap().trunc().network();

                prop_assert_eq!(limiter_key(IpAddr::V6(address)), IpAddr::V6(theirs));
            }

            #[test]
            fn an_ipv4_clients_key_is_its_address(address: u32) {
                let address = IpAddr::V4(Ipv4Addr::from(address));

                prop_assert_eq!(limiter_key(address), address);
            }
        }
    }

    #[test]
    fn a_zero_prefix_says_it_would_trust_every_address() {
        for text in ["0.0.0.0/0", "::/0"] {
            let message = text.parse::<IpNet>().unwrap_err().to_string();

            assert!(message.contains("would trust every address"), "{message}");
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
