//! Which addresses a managed agent's bots may dial (ADR-0019, S46, S69):
//! [`classify`] sorts an IP address into never, private or public by [the
//! table](super#the-table) in [`net`](super)'s header, and [`TargetPolicy`]
//! decides with the agent's `[control_plane] private_targets`,
//! [`PrivateTargets`].
//!
//! fleet-mc checks the socket it resolved, the one azalea dials, right
//! before the bot connects; a refusal never dials. Standalone mode, whose
//! servers the operator configures, isn't checked ([`TargetPolicy::Unchecked`]).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::IpNet;

/// A named range of [`classify`]'s table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Range {
    /// A cloud provider's metadata service outside link-local space.
    Metadata,
    /// `0.0.0.0/8` or `::`: no host.
    Unspecified,
    /// The host itself.
    Loopback,
    /// `::a.b.c.d`, deprecated.
    Ipv4Compatible,
    /// RFC 1918's private networks.
    Rfc1918,
    /// `100.64.0.0/10`, carrier-grade NAT.
    SharedAddressSpace,
    /// `fc00::/7`.
    UniqueLocal,
    /// `169.254.0.0/16` or `fe80::/10`, the cloud metadata address included.
    LinkLocal,
    /// The IETF's protocol assignments.
    ProtocolAssignments,
    /// The documentation networks.
    Documentation,
    /// `192.88.99.0/24`, deprecated.
    Relay6to4,
    /// `198.18.0.0/15`.
    Benchmarking,
    /// Multicast.
    Multicast,
    /// `255.255.255.255`.
    Broadcast,
    /// `240.0.0.0/4`.
    Reserved,
    /// `64:ff9b:1::/48`, a network's own NAT64.
    LocalNat64,
    /// `64:ff9b::/96` around an IPv4 that isn't public.
    Nat64,
    /// `2002::/16` around an IPv4 that isn't public.
    SixToFour,
    /// IPv6 outside `2000::/3` that no other row names.
    NotGlobalUnicast,
}

impl Range {
    /// The range's name, for a log line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Unspecified => "unspecified",
            Self::Loopback => "loopback",
            Self::Ipv4Compatible => "ipv4_compatible",
            Self::Rfc1918 => "rfc1918",
            Self::SharedAddressSpace => "shared_address_space",
            Self::UniqueLocal => "unique_local",
            Self::LinkLocal => "link_local",
            Self::ProtocolAssignments => "protocol_assignments",
            Self::Documentation => "documentation",
            Self::Relay6to4 => "relay_6to4",
            Self::Benchmarking => "benchmarking",
            Self::Multicast => "multicast",
            Self::Broadcast => "broadcast",
            Self::Reserved => "reserved",
            Self::LocalNat64 => "local_nat64",
            Self::Nat64 => "nat64",
            Self::SixToFour => "6to4",
            Self::NotGlobalUnicast => "not_global_unicast",
        }
    }
}

/// What kind of address an IP is, for a bot to dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressClass {
    /// Never dialled, whatever the config lists.
    Never(Range),
    /// Dialled only inside [`PrivateTargets`].
    Private(Range),
    /// Dialled.
    Public,
}

impl AddressClass {
    /// The class's name, for a log line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never(_) => "never",
            Self::Private(_) => "private",
            Self::Public => "public",
        }
    }

    /// The range it's in, if it's not public.
    #[must_use]
    pub const fn range(self) -> Option<Range> {
        match self {
            Self::Never(range) | Self::Private(range) => Some(range),
            Self::Public => None,
        }
    }
}

/// An IPv4 network of the table.
const fn v4(a: u8, b: u8, c: u8, d: u8, prefix_len: u8) -> IpNet {
    IpNet::from_parts(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), prefix_len)
}

/// An IPv6 network of the table.
const fn v6(bits: u128, prefix_len: u8) -> IpNet {
    IpNet::from_parts(IpAddr::V6(Ipv6Addr::from_bits(bits)), prefix_len)
}

/// IPv4's rows, earlier ones first; an address in none is public.
const V4: [(IpNet, AddressClass); 18] = [
    (
        v4(100, 100, 100, 200, 32),
        AddressClass::Never(Range::Metadata),
    ),
    (v4(0, 0, 0, 0, 8), AddressClass::Never(Range::Unspecified)),
    (v4(127, 0, 0, 0, 8), AddressClass::Private(Range::Loopback)),
    (v4(10, 0, 0, 0, 8), AddressClass::Private(Range::Rfc1918)),
    (v4(172, 16, 0, 0, 12), AddressClass::Private(Range::Rfc1918)),
    (
        v4(192, 168, 0, 0, 16),
        AddressClass::Private(Range::Rfc1918),
    ),
    (
        v4(100, 64, 0, 0, 10),
        AddressClass::Private(Range::SharedAddressSpace),
    ),
    (
        v4(169, 254, 0, 0, 16),
        AddressClass::Never(Range::LinkLocal),
    ),
    (
        v4(192, 0, 0, 0, 24),
        AddressClass::Never(Range::ProtocolAssignments),
    ),
    (
        v4(192, 0, 2, 0, 24),
        AddressClass::Never(Range::Documentation),
    ),
    (
        v4(198, 51, 100, 0, 24),
        AddressClass::Never(Range::Documentation),
    ),
    (
        v4(203, 0, 113, 0, 24),
        AddressClass::Never(Range::Documentation),
    ),
    (
        v4(192, 88, 99, 0, 24),
        AddressClass::Never(Range::Relay6to4),
    ),
    (
        v4(198, 18, 0, 0, 15),
        AddressClass::Never(Range::Benchmarking),
    ),
    (v4(224, 0, 0, 0, 4), AddressClass::Never(Range::Multicast)),
    (
        v4(255, 255, 255, 255, 32),
        AddressClass::Never(Range::Broadcast),
    ),
    (v4(240, 0, 0, 0, 4), AddressClass::Never(Range::Reserved)),
    // The lower half's public addresses match here and the upper half's
    // match no row: either way, what's left of IPv4 is public.
    (v4(0, 0, 0, 0, 1), AddressClass::Public),
];

/// IPv6's rows before the two that embed an IPv4, earlier ones first.
const V6: [(IpNet, AddressClass); 12] = [
    (
        v6(0xfd00_0ec2_0000_0000_0000_0000_0000_0254, 128),
        AddressClass::Never(Range::Metadata),
    ),
    (v6(0, 128), AddressClass::Never(Range::Unspecified)),
    (v6(1, 128), AddressClass::Private(Range::Loopback)),
    (v6(0, 96), AddressClass::Never(Range::Ipv4Compatible)),
    (
        v6(0x0064_ff9b_0001_0000_0000_0000_0000_0000, 48),
        AddressClass::Never(Range::LocalNat64),
    ),
    (
        v6(0xfc00_0000_0000_0000_0000_0000_0000_0000, 7),
        AddressClass::Private(Range::UniqueLocal),
    ),
    (
        v6(0xfe80_0000_0000_0000_0000_0000_0000_0000, 10),
        AddressClass::Never(Range::LinkLocal),
    ),
    (
        v6(0xff00_0000_0000_0000_0000_0000_0000_0000, 8),
        AddressClass::Never(Range::Multicast),
    ),
    (
        v6(0x2001_0db8_0000_0000_0000_0000_0000_0000, 32),
        AddressClass::Never(Range::Documentation),
    ),
    (
        v6(0x3fff_0000_0000_0000_0000_0000_0000_0000, 20),
        AddressClass::Never(Range::Documentation),
    ),
    (
        v6(0x2001_0000_0000_0000_0000_0000_0000_0000, 23),
        AddressClass::Never(Range::ProtocolAssignments),
    ),
    (
        v6(0x2000_0000_0000_0000_0000_0000_0000_0000, 3),
        AddressClass::Public,
    ),
];

/// NAT64's well-known prefix, `64:ff9b::/96`.
const NAT64: IpNet = v6(0x0064_ff9b_0000_0000_0000_0000_0000_0000, 96);

/// 6to4's prefix, `2002::/16`.
const SIX_TO_FOUR: IpNet = v6(0x2002_0000_0000_0000_0000_0000_0000_0000, 16);

/// What kind of address `ip` is, by [the table](super#the-table) in
/// [`net`](super)'s header.
#[must_use]
pub fn classify(ip: IpAddr) -> AddressClass {
    match ip.to_canonical() {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

/// IPv4's class: the first row that holds it, public past them all.
fn classify_v4(v4: Ipv4Addr) -> AddressClass {
    let ip = IpAddr::V4(v4);
    V4.iter()
        .find(|(network, _)| network.contains(ip))
        .map_or(AddressClass::Public, |(_, class)| *class)
}

/// IPv6's class, never IPv4-mapped: the two prefixes around an IPv4 by the
/// IPv4 inside, then the first row that holds it, never past them all.
fn classify_v6(v6: Ipv6Addr) -> AddressClass {
    let ip = IpAddr::V6(v6);
    let bits = v6.to_bits();
    let around = |range, embedded: u128| {
        let inner = u32::try_from(embedded & u128::from(u32::MAX)).unwrap_or(0);
        match classify_v4(Ipv4Addr::from(inner)) {
            AddressClass::Public => AddressClass::Public,
            AddressClass::Never(_) | AddressClass::Private(_) => AddressClass::Never(range),
        }
    };
    if NAT64.contains(ip) {
        return around(Range::Nat64, bits);
    }
    if SIX_TO_FOUR.contains(ip) {
        return around(Range::SixToFour, bits >> 80);
    }
    V6.iter().find(|(network, _)| network.contains(ip)).map_or(
        AddressClass::Never(Range::NotGlobalUnicast),
        |(_, class)| *class,
    )
}

/// `[control_plane] private_targets`: the private networks a managed agent's
/// bots may dial, empty by default. Every network lies inside a private
/// range of [`classify`]'s table; a never-class address inside one stays
/// refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivateTargets(Vec<IpNet>);

/// Why networks aren't [`PrivateTargets`]. No message names an entry's text;
/// the config names its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PrivateTargetsError {
    /// More networks than [`PrivateTargets::MAX`].
    #[error("must list at most {max} networks")]
    TooMany {
        /// The most allowed.
        max: usize,
    },
    /// A network that isn't inside one private range (loopback, RFC 1918,
    /// shared address space, unique local).
    #[error(
        "must lie inside a private range: loopback, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 100.64.0.0/10 or fc00::/7"
    )]
    NotPrivate {
        /// Its index in the list.
        index: usize,
    },
    /// A network the agent never dials, whatever the list says: a cloud's
    /// metadata address, though it lies inside a private range, or one
    /// inside a never-dialled range (link-local, documentation, …).
    #[error(
        "is never dialled, whatever the list says: a metadata, link-local, documentation or other special address"
    )]
    NeverDialled {
        /// Its index in the list.
        index: usize,
    },
    /// A network an earlier entry already names.
    #[error("is listed twice")]
    Duplicate {
        /// Its index in the list.
        index: usize,
    },
}

impl PrivateTargets {
    /// The most networks the config may list.
    pub const MAX: usize = 32;

    /// The networks of `networks`.
    ///
    /// # Errors
    /// [`PrivateTargetsError::TooMany`] past [`PrivateTargets::MAX`], or
    /// the first entry that's [`PrivateTargetsError::NotPrivate`],
    /// [`PrivateTargetsError::NeverDialled`] or a
    /// [`PrivateTargetsError::Duplicate`].
    pub fn try_new(networks: Vec<IpNet>) -> Result<Self, PrivateTargetsError> {
        match Self::problems(&networks).first() {
            Some(problem) => Err(*problem),
            None => Ok(Self(networks)),
        }
    }

    /// Every problem of `networks`, in order, for a config that reports them
    /// all at once: [`PrivateTargetsError::TooMany`] first, then each entry's.
    #[must_use]
    pub fn problems(networks: &[IpNet]) -> Vec<PrivateTargetsError> {
        let mut problems = Vec::new();
        if networks.len() > Self::MAX {
            problems.push(PrivateTargetsError::TooMany { max: Self::MAX });
        }
        for (index, network) in networks.iter().enumerate() {
            match standing(network) {
                Standing::NotPrivate => {
                    problems.push(PrivateTargetsError::NotPrivate { index });
                    continue;
                }
                Standing::NeverDialled => {
                    problems.push(PrivateTargetsError::NeverDialled { index });
                    continue;
                }
                Standing::Private => {}
            }
            if networks
                .iter()
                .take(index)
                .any(|earlier| earlier == network)
            {
                problems.push(PrivateTargetsError::Duplicate { index });
            }
        }
        problems
    }

    /// The networks.
    #[must_use]
    pub fn networks(&self) -> &[IpNet] {
        &self.0
    }

    /// Whether `ip`, in its canonical form, is in a listed network.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|network| network.contains(ip))
    }
}

/// Where a listed network stands.
enum Standing {
    /// Inside one private row of the table.
    Private,
    /// A never-class row itself (the metadata addresses, inside a private
    /// range), or inside a never-class row and no private one.
    NeverDialled,
    /// Neither: public, or across ranges.
    NotPrivate,
}

/// Where `network` stands: every address of it inside one private row and
/// it not a never-class row itself is private; one a never-class row covers
/// is never dialled; the rest isn't private.
fn standing(network: &IpNet) -> Standing {
    let rows = || V4.iter().chain(V6.iter());
    let inside_private = rows()
        .any(|(range, class)| matches!(class, AddressClass::Private(_)) && range.covers(network));
    let a_never_row =
        rows().any(|(range, class)| matches!(class, AddressClass::Never(_)) && range == network);
    let inside_never = rows()
        .any(|(range, class)| matches!(class, AddressClass::Never(_)) && range.covers(network));
    match (inside_private, a_never_row, inside_never) {
        (true, false, _) => Standing::Private,
        (true, true, _) | (false, _, true) => Standing::NeverDialled,
        (false, _, false) => Standing::NotPrivate,
    }
}

/// Whether a bot may dial an address: a process's policy, set where fleet-mc's
/// connector is built. [`TargetPolicy::default`] is the strictest, so a
/// connector nobody configured fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPolicy {
    /// Every address: standalone mode, whose servers the operator names.
    Unchecked,
    /// Public addresses, and private ones inside the list; never the
    /// never-class (managed mode).
    Checked(PrivateTargets),
}

/// The strictest: checked, nothing private listed.
impl Default for TargetPolicy {
    fn default() -> Self {
        Self::Checked(PrivateTargets::default())
    }
}

/// Why a [`TargetPolicy`] refused an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetRefusal {
    class: AddressClass,
}

impl TargetRefusal {
    /// The address's class: never, or private and unlisted.
    #[must_use]
    pub const fn class(&self) -> AddressClass {
        self.class
    }
}

impl TargetPolicy {
    /// Whether a bot may dial `ip`.
    ///
    /// # Errors
    /// [`TargetRefusal`] with the address's class when it's never-class, or
    /// private outside the list.
    pub fn check(&self, ip: IpAddr) -> Result<(), TargetRefusal> {
        let Self::Checked(listed) = self else {
            return Ok(());
        };
        match classify(ip) {
            AddressClass::Public => Ok(()),
            AddressClass::Private(_) if listed.contains(ip) => Ok(()),
            class @ (AddressClass::Never(_) | AddressClass::Private(_)) => {
                Err(TargetRefusal { class })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn net(text: &str) -> IpNet {
        text.parse().unwrap()
    }

    fn targets(texts: &[&str]) -> Result<PrivateTargets, PrivateTargetsError> {
        PrivateTargets::try_new(texts.iter().map(|text| net(text)).collect())
    }

    use AddressClass::{Never, Private, Public};

    #[rstest]
    // Each range's first and last address, and its neighbours just outside.
    #[case::this_network_first("0.0.0.0", Never(Range::Unspecified))]
    #[case::this_network_last("0.255.255.255", Never(Range::Unspecified))]
    #[case::after_this_network("1.0.0.0", Public)]
    #[case::before_rfc1918_10("9.255.255.255", Public)]
    #[case::rfc1918_10_first("10.0.0.0", Private(Range::Rfc1918))]
    #[case::rfc1918_10_last("10.255.255.255", Private(Range::Rfc1918))]
    #[case::after_rfc1918_10("11.0.0.0", Public)]
    #[case::before_shared("100.63.255.255", Public)]
    #[case::shared_first("100.64.0.0", Private(Range::SharedAddressSpace))]
    #[case::before_alibaba("100.100.100.199", Private(Range::SharedAddressSpace))]
    #[case::alibaba_metadata("100.100.100.200", Never(Range::Metadata))]
    #[case::after_alibaba("100.100.100.201", Private(Range::SharedAddressSpace))]
    #[case::shared_last("100.127.255.255", Private(Range::SharedAddressSpace))]
    #[case::after_shared("100.128.0.0", Public)]
    #[case::before_loopback("126.255.255.255", Public)]
    #[case::loopback_first("127.0.0.0", Private(Range::Loopback))]
    #[case::loopback_host("127.0.0.1", Private(Range::Loopback))]
    #[case::loopback_last("127.255.255.255", Private(Range::Loopback))]
    #[case::after_loopback("128.0.0.0", Public)]
    #[case::before_link_local("169.253.255.255", Public)]
    #[case::link_local_first("169.254.0.0", Never(Range::LinkLocal))]
    #[case::cloud_metadata("169.254.169.254", Never(Range::LinkLocal))]
    #[case::link_local_last("169.254.255.255", Never(Range::LinkLocal))]
    #[case::after_link_local("169.255.0.0", Public)]
    #[case::before_rfc1918_172("172.15.255.255", Public)]
    #[case::rfc1918_172_first("172.16.0.0", Private(Range::Rfc1918))]
    #[case::rfc1918_172_last("172.31.255.255", Private(Range::Rfc1918))]
    #[case::after_rfc1918_172("172.32.0.0", Public)]
    #[case::before_protocol("191.255.255.255", Public)]
    #[case::protocol_first("192.0.0.0", Never(Range::ProtocolAssignments))]
    #[case::protocol_last("192.0.0.255", Never(Range::ProtocolAssignments))]
    #[case::between_protocol_and_test_net("192.0.1.0", Public)]
    #[case::test_net_1_first("192.0.2.0", Never(Range::Documentation))]
    #[case::test_net_1_host("192.0.2.10", Never(Range::Documentation))]
    #[case::test_net_1_last("192.0.2.255", Never(Range::Documentation))]
    #[case::after_test_net_1("192.0.3.0", Public)]
    #[case::before_relay("192.88.98.255", Public)]
    #[case::relay_first("192.88.99.0", Never(Range::Relay6to4))]
    #[case::relay_last("192.88.99.255", Never(Range::Relay6to4))]
    #[case::after_relay("192.88.100.0", Public)]
    #[case::before_rfc1918_192("192.167.255.255", Public)]
    #[case::rfc1918_192_first("192.168.0.0", Private(Range::Rfc1918))]
    #[case::rfc1918_192_last("192.168.255.255", Private(Range::Rfc1918))]
    #[case::after_rfc1918_192("192.169.0.0", Public)]
    #[case::before_benchmarking("198.17.255.255", Public)]
    #[case::benchmarking_first("198.18.0.0", Never(Range::Benchmarking))]
    #[case::benchmarking_last("198.19.255.255", Never(Range::Benchmarking))]
    #[case::after_benchmarking("198.20.0.0", Public)]
    #[case::before_test_net_2("198.51.99.255", Public)]
    #[case::test_net_2_first("198.51.100.0", Never(Range::Documentation))]
    #[case::test_net_2_last("198.51.100.255", Never(Range::Documentation))]
    #[case::after_test_net_2("198.51.101.0", Public)]
    #[case::before_test_net_3("203.0.112.255", Public)]
    #[case::test_net_3_first("203.0.113.0", Never(Range::Documentation))]
    #[case::test_net_3_last("203.0.113.255", Never(Range::Documentation))]
    #[case::after_test_net_3("203.0.114.0", Public)]
    #[case::before_multicast("223.255.255.255", Public)]
    #[case::multicast_first("224.0.0.0", Never(Range::Multicast))]
    #[case::multicast_last("239.255.255.255", Never(Range::Multicast))]
    #[case::reserved_first("240.0.0.0", Never(Range::Reserved))]
    #[case::reserved_last("255.255.255.254", Never(Range::Reserved))]
    #[case::broadcast("255.255.255.255", Never(Range::Broadcast))]
    #[case::public_v4("8.8.8.8", Public)]
    fn an_ipv4_address_is_classed_by_its_range(#[case] address: &str, #[case] class: AddressClass) {
        assert_eq!(classify(ip(address)), class, "{address}");
    }

    #[rstest]
    #[case::unspecified("::", Never(Range::Unspecified))]
    #[case::loopback("::1", Private(Range::Loopback))]
    #[case::compatible_first("::2", Never(Range::Ipv4Compatible))]
    #[case::compatible_public("::8.8.8.8", Never(Range::Ipv4Compatible))]
    #[case::compatible_last("::ffff:ffff", Never(Range::Ipv4Compatible))]
    #[case::after_compatible("::1:0:0", Never(Range::NotGlobalUnicast))]
    #[case::mapped_loopback("::ffff:127.0.0.1", Private(Range::Loopback))]
    #[case::mapped_metadata("::ffff:169.254.169.254", Never(Range::LinkLocal))]
    #[case::mapped_public("::ffff:8.8.8.8", Public)]
    #[case::discard_only("100::", Never(Range::NotGlobalUnicast))]
    #[case::nat64_public("64:ff9b::808:808", Public)]
    #[case::nat64_private("64:ff9b::a00:1", Never(Range::Nat64))]
    #[case::nat64_loopback("64:ff9b::7f00:1", Never(Range::Nat64))]
    #[case::nat64_metadata("64:ff9b::a9fe:a9fe", Never(Range::Nat64))]
    #[case::nat64_beyond_96("64:ff9b::1:808:808", Never(Range::NotGlobalUnicast))]
    #[case::local_nat64_first("64:ff9b:1::", Never(Range::LocalNat64))]
    #[case::local_nat64_public("64:ff9b:1::808:808", Never(Range::LocalNat64))]
    #[case::local_nat64_last("64:ff9b:1:ffff:ffff:ffff:ffff:ffff", Never(Range::LocalNat64))]
    #[case::before_global(
        "1fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        Never(Range::NotGlobalUnicast)
    )]
    #[case::protocol_first("2001::", Never(Range::ProtocolAssignments))]
    #[case::teredo("2001:0:4136:e378::1", Never(Range::ProtocolAssignments))]
    #[case::protocol_last(
        "2001:1ff:ffff:ffff:ffff:ffff:ffff:ffff",
        Never(Range::ProtocolAssignments)
    )]
    #[case::after_protocol("2001:200::", Public)]
    #[case::documentation_first("2001:db8::", Never(Range::Documentation))]
    #[case::documentation_last(
        "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff",
        Never(Range::Documentation)
    )]
    #[case::after_documentation("2001:db9::", Public)]
    #[case::six_to_four_public("2002:808:808::1", Public)]
    #[case::six_to_four_private("2002:c0a8:101::1", Never(Range::SixToFour))]
    #[case::six_to_four_metadata("2002:a9fe:a9fe::1", Never(Range::SixToFour))]
    #[case::public_v6("2606:4700::1111", Public)]
    #[case::documentation_3fff_first("3fff::", Never(Range::Documentation))]
    #[case::documentation_3fff_last(
        "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff",
        Never(Range::Documentation)
    )]
    #[case::after_documentation_3fff("3fff:1000::", Public)]
    #[case::last_global("3fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", Public)]
    #[case::after_global("4000::", Never(Range::NotGlobalUnicast))]
    #[case::srv6("5f00::1", Never(Range::NotGlobalUnicast))]
    #[case::before_unique_local(
        "fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        Never(Range::NotGlobalUnicast)
    )]
    #[case::unique_local_first("fc00::", Private(Range::UniqueLocal))]
    #[case::before_aws("fd00:ec2::253", Private(Range::UniqueLocal))]
    #[case::aws_metadata("fd00:ec2::254", Never(Range::Metadata))]
    #[case::after_aws("fd00:ec2::255", Private(Range::UniqueLocal))]
    #[case::unique_local_last(
        "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        Private(Range::UniqueLocal)
    )]
    #[case::after_unique_local("fe00::", Never(Range::NotGlobalUnicast))]
    #[case::link_local_first("fe80::", Never(Range::LinkLocal))]
    #[case::link_local_last("febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff", Never(Range::LinkLocal))]
    #[case::site_local("fec0::1", Never(Range::NotGlobalUnicast))]
    #[case::multicast_first("ff00::", Never(Range::Multicast))]
    #[case::multicast_last("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", Never(Range::Multicast))]
    fn an_ipv6_address_is_classed_by_its_range(#[case] address: &str, #[case] class: AddressClass) {
        assert_eq!(classify(ip(address)), class, "{address}");
    }

    #[test]
    fn every_row_of_the_table_is_a_strict_network() {
        let rows = V4
            .iter()
            .chain(V6.iter())
            .map(|(network, _)| *network)
            .chain([NAT64, SIX_TO_FOUR]);

        for network in rows {
            assert_eq!(network.to_string().parse(), Ok(network), "{network}");
        }
    }

    #[test]
    fn only_the_two_metadata_rows_lie_inside_a_private_range() {
        let private: Vec<IpNet> = V4
            .iter()
            .chain(V6.iter())
            .filter(|(_, class)| matches!(class, Private(_)))
            .map(|(network, _)| *network)
            .collect();

        let inside: Vec<String> = V4
            .iter()
            .chain(V6.iter())
            .filter(|(network, class)| {
                !matches!(class, Private(_)) && private.iter().any(|range| range.covers(network))
            })
            .map(|(network, _)| network.to_string())
            .collect();

        assert_eq!(inside, ["100.100.100.200/32", "fd00:ec2::254/128"]);
    }

    #[test]
    fn the_names_of_every_class_and_range_are_distinct() {
        let ranges = [
            Range::Metadata,
            Range::Unspecified,
            Range::Loopback,
            Range::Ipv4Compatible,
            Range::Rfc1918,
            Range::SharedAddressSpace,
            Range::UniqueLocal,
            Range::LinkLocal,
            Range::ProtocolAssignments,
            Range::Documentation,
            Range::Relay6to4,
            Range::Benchmarking,
            Range::Multicast,
            Range::Broadcast,
            Range::Reserved,
            Range::LocalNat64,
            Range::Nat64,
            Range::SixToFour,
            Range::NotGlobalUnicast,
        ];
        let names: std::collections::BTreeSet<_> =
            ranges.iter().map(|range| range.as_str()).collect();

        assert_eq!(names.len(), ranges.len());
        assert_eq!(
            [
                Never(Range::Loopback).as_str(),
                Private(Range::Loopback).as_str(),
                Public.as_str()
            ],
            ["never", "private", "public"]
        );
        assert_eq!(Never(Range::LinkLocal).range(), Some(Range::LinkLocal));
        assert_eq!(Private(Range::Rfc1918).range(), Some(Range::Rfc1918));
        assert_eq!(Public.range(), None);
    }

    proptest! {
        #[test]
        fn a_mapped_address_is_classed_as_its_ipv4(bits: u32) {
            let v4 = Ipv4Addr::from(bits);

            prop_assert_eq!(classify(IpAddr::V6(v4.to_ipv6_mapped())), classify(IpAddr::V4(v4)));
        }

        #[test]
        fn nat64_is_public_only_around_a_public_ipv4(bits: u32) {
            let v4 = IpAddr::V4(Ipv4Addr::from(bits));
            let nat64 = Ipv6Addr::from_bits(0x0064_ff9b_0000_0000_0000_0000_0000_0000 | u128::from(bits));

            let expected = if classify(v4) == Public { Public } else { Never(Range::Nat64) };
            prop_assert_eq!(classify(IpAddr::V6(nat64)), expected);
        }

        #[test]
        fn six_to_four_is_public_only_around_a_public_ipv4(bits: u32, site: u64, host: u16) {
            let v4 = IpAddr::V4(Ipv4Addr::from(bits));
            let six_to_four = Ipv6Addr::from_bits(
                (0x2002_u128 << 112)
                    | (u128::from(bits) << 80)
                    | (u128::from(host) << 64)
                    | u128::from(site),
            );

            let expected = if classify(v4) == Public { Public } else { Never(Range::SixToFour) };
            prop_assert_eq!(classify(IpAddr::V6(six_to_four)), expected);
        }

        #[test]
        fn ipv4_agrees_with_std_where_std_is_stable(bits: u32) {
            let v4 = Ipv4Addr::from(bits);
            let class = classify(IpAddr::V4(v4));

            if v4.is_private() {
                prop_assert_eq!(class, Private(Range::Rfc1918));
            }
            if v4.is_loopback() {
                prop_assert_eq!(class, Private(Range::Loopback));
            }
            if v4.is_link_local() {
                prop_assert_eq!(class, Never(Range::LinkLocal));
            }
            if v4.is_multicast() {
                prop_assert_eq!(class, Never(Range::Multicast));
            }
            if v4.is_broadcast() {
                prop_assert_eq!(class, Never(Range::Broadcast));
            }
            if v4.is_documentation() {
                prop_assert_eq!(class, Never(Range::Documentation));
            }
            if v4.is_unspecified() {
                prop_assert_eq!(class, Never(Range::Unspecified));
            }
        }

        #[test]
        fn ipv6_agrees_with_std_where_std_is_stable(bits: u128) {
            let v6 = Ipv6Addr::from_bits(bits);
            prop_assume!(v6.to_ipv4_mapped().is_none());
            let class = classify(IpAddr::V6(v6));

            if v6.is_loopback() {
                prop_assert_eq!(class, Private(Range::Loopback));
            }
            if v6.is_unspecified() {
                prop_assert_eq!(class, Never(Range::Unspecified));
            }
            if v6.is_multicast() {
                prop_assert_eq!(class, Never(Range::Multicast));
            }
            if v6.is_unicast_link_local() {
                prop_assert_eq!(class, Never(Range::LinkLocal));
            }
            if v6.is_unique_local() && bits != 0xfd00_0ec2_0000_0000_0000_0000_0000_0254 {
                prop_assert_eq!(class, Private(Range::UniqueLocal));
            }
        }

        #[test]
        fn ipv6_outside_global_unicast_is_never_public(bits: u128) {
            let v6 = Ipv6Addr::from_bits(bits);
            prop_assume!(v6.to_ipv4_mapped().is_none());
            prop_assume!(bits >> 125 != 0b001);
            prop_assume!(bits >> 32 != 0x0064_ff9b_0000_0000_0000_0000);

            prop_assert_ne!(classify(IpAddr::V6(v6)), Public);
        }
    }

    #[test]
    fn listed_private_networks_are_read() {
        let listed = targets(&[
            "127.0.0.1/32",
            "10.0.0.0/8",
            "172.18.0.0/16",
            "100.64.0.0/10",
            "fd00::/8",
            "::1/128",
        ])
        .unwrap();

        assert_eq!(listed.networks().len(), 6);
        assert!(listed.contains(ip("10.1.2.3")));
        assert!(listed.contains(ip("::ffff:127.0.0.1")));
        assert!(listed.contains(ip("fd12::1")));
        assert!(!listed.contains(ip("127.0.0.2")));
        assert!(!listed.contains(ip("192.168.1.1")));
    }

    #[rstest]
    #[case::public("8.8.8.0/24", PrivateTargetsError::NotPrivate { index: 0 })]
    #[case::link_local("169.254.0.0/16", PrivateTargetsError::NeverDialled { index: 0 })]
    #[case::metadata_link_local(
        "169.254.169.254/32",
        PrivateTargetsError::NeverDialled { index: 0 }
    )]
    #[case::documentation("192.0.2.0/24", PrivateTargetsError::NeverDialled { index: 0 })]
    #[case::wider_than_its_range("10.0.0.0/7", PrivateTargetsError::NotPrivate { index: 0 })]
    #[case::across_two_ranges("172.0.0.0/8", PrivateTargetsError::NotPrivate { index: 0 })]
    #[case::alibaba_metadata(
        "100.100.100.200/32",
        PrivateTargetsError::NeverDialled { index: 0 }
    )]
    #[case::aws_metadata(
        "fd00:ec2::254/128",
        PrivateTargetsError::NeverDialled { index: 0 }
    )]
    #[case::multicast_v6("ff02::/16", PrivateTargetsError::NeverDialled { index: 0 })]
    #[case::ipv6_public("2001:4860::/32", PrivateTargetsError::NotPrivate { index: 0 })]
    #[case::nat64("64:ff9b::/96", PrivateTargetsError::NotPrivate { index: 0 })]
    fn a_network_outside_every_private_range_is_refused_by_its_index(
        #[case] text: &str,
        #[case] error: PrivateTargetsError,
    ) {
        assert_eq!(targets(&[text]), Err(error));
        // Its index, after a good one.
        let shifted = match error {
            PrivateTargetsError::NotPrivate { index } => {
                PrivateTargetsError::NotPrivate { index: index + 1 }
            }
            PrivateTargetsError::NeverDialled { index } => {
                PrivateTargetsError::NeverDialled { index: index + 1 }
            }
            other => other,
        };
        assert_eq!(targets(&["10.0.0.0/8", text]), Err(shifted));
    }

    #[test]
    fn a_network_listed_twice_is_refused_by_its_second_index() {
        assert_eq!(
            targets(&["10.0.0.0/8", "127.0.0.1/32", "10.0.0.0/8"]),
            Err(PrivateTargetsError::Duplicate { index: 2 })
        );
    }

    #[test]
    fn more_than_the_most_networks_is_refused() {
        let most: Vec<IpNet> = (0..=31_u8).map(|n| v4(10, n, 0, 0, 16)).collect();
        let too_many: Vec<IpNet> = (0..=32_u8).map(|n| v4(10, n, 0, 0, 16)).collect();

        assert_eq!(
            PrivateTargets::try_new(most).map(|listed| listed.networks().len()),
            Ok(32)
        );
        assert_eq!(
            PrivateTargets::try_new(too_many),
            Err(PrivateTargetsError::TooMany { max: 32 })
        );
    }

    #[test]
    fn every_problem_of_a_list_is_found_in_order() {
        let mut networks: Vec<IpNet> = (0..=32_u8).map(|n| v4(10, n, 0, 0, 16)).collect();
        networks.push(net("8.8.8.0/24"));
        networks.push(net("10.0.0.0/16"));

        let problems = PrivateTargets::problems(&networks);

        assert_eq!(
            problems,
            [
                PrivateTargetsError::TooMany { max: 32 },
                PrivateTargetsError::NotPrivate { index: 33 },
                PrivateTargetsError::Duplicate { index: 34 },
            ]
        );
        assert_eq!(PrivateTargets::problems(&[net("10.0.0.0/8")]), []);
    }

    #[test]
    fn no_refusal_names_its_entry() {
        for error in [
            PrivateTargetsError::TooMany { max: 32 },
            PrivateTargetsError::NotPrivate { index: 31_415 },
            PrivateTargetsError::NeverDialled { index: 31_415 },
            PrivateTargetsError::Duplicate { index: 31_415 },
        ] {
            let message = error.to_string();

            assert!(!message.contains("31415"), "{message}");
        }
    }

    #[rstest]
    #[case::public("8.8.8.8", Ok(()))]
    #[case::public_v6("2606:4700::1111", Ok(()))]
    #[case::listed_loopback("127.0.0.1", Ok(()))]
    #[case::listed_mapped("::ffff:127.0.0.1", Ok(()))]
    #[case::unlisted_loopback("127.0.0.2", Err(Private(Range::Loopback)))]
    #[case::unlisted_lan("192.168.1.10", Err(Private(Range::Rfc1918)))]
    #[case::metadata("169.254.169.254", Err(Never(Range::LinkLocal)))]
    #[case::documentation("192.0.2.10", Err(Never(Range::Documentation)))]
    #[case::listed_range_metadata("100.100.100.200", Err(Never(Range::Metadata)))]
    #[case::nat64_of_listed("64:ff9b::7f00:1", Err(Never(Range::Nat64)))]
    fn a_checked_policy_dials_public_and_listed_addresses_only(
        #[case] address: &str,
        #[case] outcome: Result<(), AddressClass>,
    ) {
        let policy = TargetPolicy::Checked(targets(&["127.0.0.1/32", "100.64.0.0/10"]).unwrap());

        assert_eq!(
            policy.check(ip(address)).map_err(|refused| refused.class()),
            outcome
        );
    }

    #[rstest]
    #[case::loopback("127.0.0.1")]
    #[case::metadata("169.254.169.254")]
    #[case::public("8.8.8.8")]
    fn an_unchecked_policy_dials_everything(#[case] address: &str) {
        assert_eq!(TargetPolicy::Unchecked.check(ip(address)), Ok(()));
    }

    #[rstest]
    #[case::loopback("127.0.0.1", Private(Range::Loopback))]
    #[case::lan("10.0.0.1", Private(Range::Rfc1918))]
    #[case::metadata("169.254.169.254", Never(Range::LinkLocal))]
    fn the_default_policy_is_the_strictest(#[case] address: &str, #[case] class: AddressClass) {
        let policy = TargetPolicy::default();

        assert_eq!(policy, TargetPolicy::Checked(PrivateTargets::default()));
        assert_eq!(
            policy.check(ip(address)).map_err(|refused| refused.class()),
            Err(class)
        );
        assert_eq!(policy.check(ip("8.8.8.8")), Ok(()));
    }
}
