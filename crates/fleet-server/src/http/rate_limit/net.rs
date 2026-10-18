//! IP networks (Plan.md P7.10, ADR-0016): [`TrustedProxies`], the networks
//! of `[http] trusted_proxies`, each a fleet-core [`IpNet`] (ADR-0019, S69);
//! and [`limiter_key`], the address a per-IP limit counts a client under.
//!
//! Addresses are compared in their canonical form
//! ([`IpAddr::to_canonical`]): a dual-stack `[::]` listener reports an IPv4
//! peer as `::ffff:a.b.c.d`, which must still match an IPv4 network, or a
//! trusted proxy would count as untrusted and every client behind it would
//! share its one bucket.

use std::net::{IpAddr, Ipv6Addr};

pub use fleet_core::net::{IpNet, IpNetError};

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
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use rstest::rstest;

    use super::*;

    fn net(text: &str) -> IpNet {
        text.parse().unwrap()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
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
}
