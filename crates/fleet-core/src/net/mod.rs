//! IP networks and the addresses a bot may dial (ADR-0016, ADR-0019 S46,
//! S69).
//!
//! - [`IpNet`]: one IP network in strict CIDR form, as the server's
//!   `[http] trusted_proxies` and the agent's `[control_plane]
//!   private_targets` list them.
//! - [`classify`], [`TargetPolicy`] and [`PrivateTargets`]: whether a
//!   managed agent's bot may dial the socket fleet-mc resolved.
//!
//! # The table
//!
//! An IPv4-mapped address (`::ffff:a.b.c.d`) is judged as its
//! IPv4. Earlier rows win.
//!
//! | Class | IPv4 | IPv6 |
//! |---|---|---|
//! | never: metadata | `100.100.100.200/32` (Alibaba Cloud; AWS's, GCP's and Azure's `169.254.169.254` is link-local) | `fd00:ec2::254/128` (AWS) |
//! | never: unspecified, this network | `0.0.0.0/8` | `::/128` |
//! | private: loopback | `127.0.0.0/8` | `::1/128` |
//! | never: IPv4-compatible | | `::/96` |
//! | private: RFC 1918 | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | |
//! | private: shared address space (carrier-grade NAT) | `100.64.0.0/10` | |
//! | private: unique local | | `fc00::/7` |
//! | never: link-local | `169.254.0.0/16` | `fe80::/10` |
//! | never: IETF protocol assignments | `192.0.0.0/24` | `2001::/23` (Teredo, benchmarking and ORCHID included) |
//! | never: documentation | `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24` | `2001:db8::/32`, `3fff::/20` |
//! | never: 6to4 relay anycast | `192.88.99.0/24` | |
//! | never: benchmarking | `198.18.0.0/15` | |
//! | never: multicast | `224.0.0.0/4` | `ff00::/8` |
//! | never: broadcast | `255.255.255.255/32` | |
//! | never: reserved | `240.0.0.0/4` | |
//! | never: local-use NAT64 | | `64:ff9b:1::/48` |
//! | NAT64, 6to4 | | `64:ff9b::/96` and `2002::/16`: public when the IPv4 inside is public, never otherwise |
//! | public | everything else | `2000::/3`, the rest of global unicast |
//! | never: not global unicast | | everything else |

mod ip_net;
mod target;

pub use ip_net::{IpNet, IpNetError};
pub use target::{
    AddressClass, PrivateTargets, PrivateTargetsError, Range, TargetPolicy, TargetRefusal, classify,
};
