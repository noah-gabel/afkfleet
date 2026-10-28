//! [`ServerKey`]: which Minecraft server an address names, for the settings
//! every bot on that server shares (ADR-0020, S74).
//!
//! The key is the host and a port that's always written. An address without
//! a port is the same server as the one with the default port, since
//! fleet-mc always hands azalea the port and azalea's resolver tries a
//! server's SRV record exactly when the port is 25565, given or not. An
//! IPv4-mapped IPv6 address is its IPv4, as the agent's target check judges
//! it. A host name and an IP address are always different servers: nothing
//! here resolves a name.

use core::fmt;
use core::net::IpAddr;
use core::str::FromStr;

use super::{Host, ServerAddress, ServerAddressError};

/// Why text isn't a [`ServerKey`]'s stored form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServerKeyError {
    /// The text isn't a valid server address.
    #[error("the text isn't a valid server address: {0}")]
    Address(#[from] ServerAddressError),
    /// The text is an address, but not written the way its key is: the port
    /// left out, upper case, or an IP address in another form.
    #[error("the text isn't a server key's canonical form")]
    NotCanonical,
}

/// A Minecraft server's identity: its host and port, the port always
/// written. Addresses that fleet-mc hands azalea alike have the same key.
///
/// It's built from a [`ServerAddress`] and can't fail; reading a stored key
/// back with [`TryFrom<&str>`] accepts only the text [`fmt::Display`] writes,
/// never a form it would have to repair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerKey {
    host: Host,
    port: u16,
}

impl ServerKey {
    /// Returns the host: a lowercase domain name, or an IP address that's
    /// never IPv4-mapped.
    #[must_use]
    pub const fn host(&self) -> &Host {
        &self.host
    }

    /// Returns the port, the default one when the address gave none.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The host as text, an IPv6 address without brackets: what an audit
    /// entry records beside the port.
    #[must_use]
    pub fn host_text(&self) -> String {
        match &self.host {
            Host::Domain(name) => name.clone(),
            Host::Ip(ip) => ip.to_string(),
        }
    }
}

impl From<&ServerAddress> for ServerKey {
    fn from(address: &ServerAddress) -> Self {
        let host = match address.host() {
            Host::Domain(name) => Host::Domain(name.clone()),
            Host::Ip(ip) => Host::Ip(canonical(*ip)),
        };
        Self {
            host,
            port: address.port(),
        }
    }
}

impl fmt::Display for ServerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Host::Domain(name) => write!(f, "{name}:{}", self.port),
            Host::Ip(IpAddr::V4(ip)) => write!(f, "{ip}:{}", self.port),
            Host::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]:{}", self.port),
        }
    }
}

impl TryFrom<&str> for ServerKey {
    type Error = ServerKeyError;

    fn try_from(text: &str) -> Result<Self, ServerKeyError> {
        let key = Self::from(&ServerAddress::try_from(text)?);
        if key.to_string() == text {
            Ok(key)
        } else {
            Err(ServerKeyError::NotCanonical)
        }
    }
}

impl FromStr for ServerKey {
    type Err = ServerKeyError;

    fn from_str(text: &str) -> Result<Self, ServerKeyError> {
        Self::try_from(text)
    }
}

/// `ip` as a key holds it: an IPv4-mapped IPv6 address as its IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn key(address: &str) -> ServerKey {
        ServerKey::from(&address.parse::<ServerAddress>().unwrap())
    }

    #[rstest]
    #[case::domain("mc.example.com", "mc.example.com:25565")]
    #[case::domain_and_port("mc.example.com:25570", "mc.example.com:25570")]
    #[case::the_default_port_given("mc.example.com:25565", "mc.example.com:25565")]
    #[case::upper_case("Mc.Example.COM", "mc.example.com:25565")]
    #[case::localhost("localhost", "localhost:25565")]
    #[case::ipv4("192.0.2.10", "192.0.2.10:25565")]
    #[case::ipv4_and_port("192.0.2.10:25566", "192.0.2.10:25566")]
    #[case::ipv6("2001:db8::1", "[2001:db8::1]:25565")]
    #[case::ipv6_upper_case_and_port("[2001:DB8::1]:25570", "[2001:db8::1]:25570")]
    #[case::ipv6_long_form("[2001:0db8:0000:0000:0000:0000:0000:0001]", "[2001:db8::1]:25565")]
    #[case::mapped("[::ffff:192.0.2.10]", "192.0.2.10:25565")]
    #[case::mapped_and_port("[::ffff:192.0.2.10]:25566", "192.0.2.10:25566")]
    #[case::ipv4_compatible_stays_ipv6("[::192.0.2.10]", "[::c000:20a]:25565")]
    fn an_address_keys_as_its_host_and_its_port(#[case] address: &str, #[case] text: &str) {
        assert_eq!(key(address).to_string(), text);
    }

    #[rstest]
    #[case::the_default_port("mc.example.com", "mc.example.com:25565")]
    #[case::case("MC.EXAMPLE.COM:25565", "mc.example.com")]
    #[case::mapped("[::ffff:192.0.2.10]:25565", "192.0.2.10")]
    #[case::ipv6_forms("[2001:0db8::0001]", "[2001:db8::1]:25565")]
    fn addresses_that_dial_the_same_server_have_one_key(#[case] one: &str, #[case] other: &str) {
        assert_eq!(key(one), key(other));
    }

    #[rstest]
    #[case::a_name_and_its_address("localhost", "127.0.0.1")]
    #[case::another_port("mc.example.com:25566", "mc.example.com")]
    #[case::another_host("play.example.com", "mc.example.com")]
    #[case::compatible_and_mapped("[::192.0.2.10]", "192.0.2.10")]
    fn addresses_of_different_servers_have_different_keys(#[case] one: &str, #[case] other: &str) {
        assert_ne!(key(one), key(other));
    }

    #[rstest]
    #[case::domain("Mc.Example.COM:25570", "mc.example.com", 25_570)]
    #[case::ipv4("192.0.2.10", "192.0.2.10", 25_565)]
    #[case::ipv6("[2001:db8::1]:25566", "2001:db8::1", 25_566)]
    #[case::mapped("[::ffff:192.0.2.10]", "192.0.2.10", 25_565)]
    fn the_host_and_port_are_read_apart(
        #[case] address: &str,
        #[case] host: &str,
        #[case] port: u16,
    ) {
        let key = key(address);

        assert_eq!((key.host_text().as_str(), key.port()), (host, port));
    }

    #[test]
    fn a_mapped_address_holds_its_ipv4() {
        assert_eq!(
            key("[::ffff:192.0.2.10]").host(),
            &Host::Ip("192.0.2.10".parse().unwrap())
        );
    }

    #[rstest]
    #[case::domain("mc.example.com:25565")]
    #[case::localhost("localhost:25565")]
    #[case::ipv4("192.0.2.10:25566")]
    #[case::ipv6("[2001:db8::1]:25570")]
    fn a_keys_text_reads_back_as_the_key(#[case] text: &str) {
        let read = ServerKey::try_from(text).unwrap();

        assert_eq!(read.to_string(), text);
        assert_eq!(text.parse::<ServerKey>(), Ok(read));
    }

    #[rstest]
    #[case::no_port("mc.example.com")]
    #[case::upper_case("MC.example.com:25565")]
    #[case::mapped("[::ffff:192.0.2.10]:25565")]
    #[case::ipv6_upper_case("[2001:DB8::1]:25565")]
    #[case::ipv6_long_form("[2001:0db8::1]:25565")]
    #[case::ipv6_without_brackets("2001:db8::1")]
    fn text_that_isnt_written_as_a_key_is_refused(#[case] text: &str) {
        assert_eq!(ServerKey::try_from(text), Err(ServerKeyError::NotCanonical));
    }

    #[rstest]
    #[case::empty("", ServerAddressError::Empty)]
    #[case::a_scheme("tcp://mc.example.com:25565", ServerAddressError::Scheme)]
    #[case::port_zero("mc.example.com:0", ServerAddressError::InvalidPort)]
    fn text_that_isnt_an_address_is_refused_as_one(
        #[case] text: &str,
        #[case] error: ServerAddressError,
    ) {
        assert_eq!(
            ServerKey::try_from(text),
            Err(ServerKeyError::Address(error))
        );
    }

    #[test]
    fn errors_never_quote_the_text() {
        let shown = [
            ServerKey::try_from("SECRET.example.com"),
            ServerKey::try_from("tcp://SECRET"),
        ]
        .map(|read| read.unwrap_err().to_string());

        assert!(
            shown.iter().all(|text| !text.contains("SECRET")),
            "{shown:?}"
        );
    }

    /// Valid addresses of every form: domains with or without a port and in
    /// any case, IPv4, and IPv6 (mapped ones among them), bracketed with or
    /// without a port.
    fn address() -> impl Strategy<Value = String> {
        let port = proptest::option::of(1..=u16::MAX);
        let label = "[a-zA-Z][a-zA-Z0-9-]{0,10}[a-zA-Z0-9]";
        let domain =
            (proptest::collection::vec(label, 1..4), port.clone()).prop_map(|(labels, port)| {
                let host = labels.join(".");
                port.map_or(host.clone(), |port| format!("{host}:{port}"))
            });
        let v4 = (any::<core::net::Ipv4Addr>(), port.clone())
            .prop_map(|(ip, port)| port.map_or(ip.to_string(), |port| format!("{ip}:{port}")));
        let v6 = (
            prop_oneof![
                any::<core::net::Ipv6Addr>(),
                any::<core::net::Ipv4Addr>().prop_map(|ip| ip.to_ipv6_mapped()),
            ],
            port,
        )
            .prop_map(|(ip, port)| port.map_or(format!("[{ip}]"), |port| format!("[{ip}]:{port}")));
        prop_oneof![domain, v4, v6]
    }

    proptest! {
        #[test]
        fn a_keys_text_always_reads_back_as_the_same_key(text in address()) {
            let key = key(&text);

            prop_assert_eq!(ServerKey::try_from(key.to_string().as_str()), Ok(key.clone()));
            prop_assert_eq!(ServerKey::from(&key.to_string().parse::<ServerAddress>().unwrap()), key);
        }

        #[test]
        fn reading_a_key_never_panics(text in any::<String>()) {
            let _ = ServerKey::try_from(text.as_str());
        }
    }
}
