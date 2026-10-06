//! [`ServerAddress`]: the Minecraft server a bot connects to.

use core::fmt;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Why text isn't a valid [`ServerAddress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServerAddressError {
    /// The address is empty.
    #[error("the address is empty")]
    Empty,
    /// The address, or its host, is too long.
    #[error("the address is too long ({len} bytes); a host may have at most {max} characters", max = ServerAddress::MAX_HOST_LEN)]
    TooLong {
        /// The length of the input, or of the host part, in bytes.
        len: usize,
    },
    /// The address starts with a scheme such as `tcp://`.
    #[error("the address has a scheme; give only the host and an optional port")]
    Scheme,
    /// The address has a path, query or fragment.
    #[error("the address has a path; give only the host and an optional port")]
    Path,
    /// The address has user info (`user@`).
    #[error("the address has user info; give only the host and an optional port")]
    UserInfo,
    /// The host isn't a valid domain name or IP address.
    #[error("the host isn't a valid domain name or IP address")]
    InvalidHost,
    /// The port isn't a number from 1 to 65535.
    #[error("the port isn't a number from 1 to 65535")]
    InvalidPort,
}

/// The host part of a [`ServerAddress`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    /// A domain name, in lowercase ASCII (e.g. `mc.example.com` or `localhost`).
    Domain(String),
    /// An IPv4 or IPv6 address.
    Ip(IpAddr),
}

/// A Minecraft server's address: a host and an optional port.
///
/// The host is a domain name or an IP address; IPv6 addresses need brackets
/// when a port follows (`[2001:db8::1]:25565`). Domain names are lowercase
/// ASCII: internationalized names, `_`, empty labels and a trailing dot are
/// rejected, and so is a last label that doesn't start with a letter, which
/// some resolvers would read as an IP address (`2130706433`, `0x7f000001`).
/// There's no scheme, path or user info.
///
/// The address remembers whether a port was given; [`ServerAddress::port`]
/// falls back to [`ServerAddress::DEFAULT_PORT`]. Loopback and private
/// addresses are allowed, because development uses `localhost` (ADR-0010).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServerAddress {
    host: Host,
    port: Option<u16>,
}

impl ServerAddress {
    /// Minecraft's default port.
    pub const DEFAULT_PORT: u16 = 25_565;
    /// The longest host name DNS allows.
    pub const MAX_HOST_LEN: usize = 253;

    /// Returns the host.
    #[must_use]
    pub const fn host(&self) -> &Host {
        &self.host
    }

    /// Returns the port, or [`ServerAddress::DEFAULT_PORT`] if none was given.
    #[must_use]
    pub const fn port(&self) -> u16 {
        match self.port {
            Some(port) => port,
            None => Self::DEFAULT_PORT,
        }
    }

    /// Returns the port only if the address gave one.
    #[must_use]
    pub const fn explicit_port(&self) -> Option<u16> {
        self.port
    }
}

/// The longest input worth parsing: a full-length host, `:` and a 5-digit port.
const MAX_INPUT_LEN: usize = ServerAddress::MAX_HOST_LEN + 6;

impl TryFrom<&str> for ServerAddress {
    type Error = ServerAddressError;

    fn try_from(text: &str) -> Result<Self, ServerAddressError> {
        if text.is_empty() {
            return Err(ServerAddressError::Empty);
        }
        if text.len() > MAX_INPUT_LEN {
            return Err(ServerAddressError::TooLong { len: text.len() });
        }
        if text.contains("://") {
            return Err(ServerAddressError::Scheme);
        }
        if text.contains(['/', '?', '#']) {
            return Err(ServerAddressError::Path);
        }
        if text.contains('@') {
            return Err(ServerAddressError::UserInfo);
        }
        let (host, port) = split_host_and_port(text)?;
        let port = port.map(parse_port).transpose()?;
        Ok(Self { host, port })
    }
}

/// Splits `text` into the parsed host and the unparsed port, if there is one.
fn split_host_and_port(text: &str) -> Result<(Host, Option<&str>), ServerAddressError> {
    if let Some(rest) = text.strip_prefix('[') {
        let (inside, after) = rest
            .split_once(']')
            .ok_or(ServerAddressError::InvalidHost)?;
        let ip = parse_ipv6(inside)?;
        let port = if after.is_empty() {
            None
        } else {
            Some(
                after
                    .strip_prefix(':')
                    .ok_or(ServerAddressError::InvalidPort)?,
            )
        };
        return Ok((ip, port));
    }
    match text.split_once(':') {
        None => Ok((parse_host(text)?, None)),
        Some((host, port)) if !port.contains(':') => Ok((parse_host(host)?, Some(port))),
        // More than one colon: only a bare IPv6 address, which can't carry a port.
        Some(_) => Ok((parse_ipv6(text)?, None)),
    }
}

fn parse_ipv6(text: &str) -> Result<Host, ServerAddressError> {
    text.parse::<Ipv6Addr>()
        .map(|ip| Host::Ip(IpAddr::V6(ip)))
        .map_err(|_| ServerAddressError::InvalidHost)
}

/// Parses an IPv4 address or a domain name.
fn parse_host(text: &str) -> Result<Host, ServerAddressError> {
    if text.len() > ServerAddress::MAX_HOST_LEN {
        return Err(ServerAddressError::TooLong { len: text.len() });
    }
    if let Ok(ip) = text.parse::<Ipv4Addr>() {
        return Ok(Host::Ip(IpAddr::V4(ip)));
    }
    parse_domain(text).map(Host::Domain)
}

/// Checks a domain name and returns it in lowercase.
///
/// Labels are 1–63 characters of `a`–`z`, `0`–`9` and `-`, without a hyphen at
/// either end. The last label must start with a letter: no top-level domain
/// starts with a digit, and resolvers that accept the old `inet_aton` forms
/// would read `1.2.3`, `2130706433` or `0x7f000001` as an IPv4 address.
fn parse_domain(text: &str) -> Result<String, ServerAddressError> {
    if !text.is_ascii() {
        return Err(ServerAddressError::InvalidHost);
    }
    let name = text.to_ascii_lowercase();
    if !name.split('.').all(is_valid_label) {
        return Err(ServerAddressError::InvalidHost);
    }
    let last_label = name.rsplit('.').next().unwrap_or_default();
    if !last_label.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(ServerAddressError::InvalidHost);
    }
    Ok(name)
}

fn is_valid_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// Parses a port: 1–5 ASCII digits, from 1 to 65535.
fn parse_port(text: &str) -> Result<u16, ServerAddressError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ServerAddressError::InvalidPort);
    }
    match text.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(ServerAddressError::InvalidPort),
    }
}

impl TryFrom<String> for ServerAddress {
    type Error = ServerAddressError;

    fn try_from(text: String) -> Result<Self, ServerAddressError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for ServerAddress {
    type Err = ServerAddressError;

    fn from_str(text: &str) -> Result<Self, ServerAddressError> {
        Self::try_from(text)
    }
}

impl From<ServerAddress> for String {
    fn from(address: ServerAddress) -> Self {
        address.to_string()
    }
}

impl fmt::Display for ServerAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Host::Domain(name) => f.write_str(name)?,
            Host::Ip(IpAddr::V4(ip)) => write!(f, "{ip}")?,
            Host::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]")?,
        }
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn parse(text: &str) -> Result<ServerAddress, ServerAddressError> {
        ServerAddress::try_from(text)
    }

    fn domain(name: &str) -> Host {
        Host::Domain(name.to_owned())
    }

    fn ip(text: &str) -> Host {
        Host::Ip(text.parse().unwrap())
    }

    #[rstest]
    #[case::domain("example.com", domain("example.com"), None, "example.com")]
    #[case::lowercased_with_port(
        "Example.COM:25566",
        domain("example.com"),
        Some(25_566),
        "example.com:25566"
    )]
    #[case::localhost("localhost", domain("localhost"), None, "localhost")]
    #[case::hyphen_inside_a_label(
        "a-b.example.com",
        domain("a-b.example.com"),
        None,
        "a-b.example.com"
    )]
    #[case::ipv4("192.0.2.10", ip("192.0.2.10"), None, "192.0.2.10")]
    #[case::ipv4_lowest_port("192.0.2.10:1", ip("192.0.2.10"), Some(1), "192.0.2.10:1")]
    #[case::highest_port(
        "mc.example.com:65535",
        domain("mc.example.com"),
        Some(65_535),
        "mc.example.com:65535"
    )]
    #[case::bracketed_ipv6_with_port(
        "[2001:db8::1]:25565",
        ip("2001:db8::1"),
        Some(25_565),
        "[2001:db8::1]:25565"
    )]
    #[case::bracketed_ipv6("[::1]", ip("::1"), None, "[::1]")]
    #[case::bare_ipv6("2001:db8::1", ip("2001:db8::1"), None, "[2001:db8::1]")]
    fn accepts_and_normalizes(
        #[case] input: &str,
        #[case] host: Host,
        #[case] explicit_port: Option<u16>,
        #[case] display: &str,
    ) {
        let address = parse(input).unwrap();

        assert_eq!(address.host(), &host);
        assert_eq!(address.explicit_port(), explicit_port);
        assert_eq!(address.port(), explicit_port.unwrap_or(25_565));
        assert_eq!(address.to_string(), display);
    }

    #[test]
    fn accepts_a_63_char_label_and_a_253_char_host() {
        let label = "a".repeat(63);
        assert!(parse(&format!("{label}.com")).is_ok());

        // 4 labels of 63 characters and 3 dots make 255; trim to exactly 253.
        let host = format!("{label}.{label}.{label}.{}", "b".repeat(61));
        assert_eq!(host.len(), 253);
        assert!(parse(&host).is_ok());
    }

    #[test]
    fn rejects_a_254_char_host() {
        let label = "a".repeat(63);
        let host = format!("{label}.{label}.{label}.{}", "b".repeat(62));
        assert_eq!(parse(&host), Err(ServerAddressError::TooLong { len: 254 }));
    }

    #[test]
    fn rejects_oversized_input_before_parsing() {
        let input = "a".repeat(10_000);
        assert_eq!(
            parse(&input),
            Err(ServerAddressError::TooLong { len: 10_000 })
        );
    }

    #[rstest]
    #[case::empty("", ServerAddressError::Empty)]
    #[case::scheme("tcp://example.com", ServerAddressError::Scheme)]
    #[case::minecraft_scheme("minecraft://example.com:25565", ServerAddressError::Scheme)]
    #[case::path("example.com/path", ServerAddressError::Path)]
    #[case::query("example.com?x=1", ServerAddressError::Path)]
    #[case::fragment("example.com#top", ServerAddressError::Path)]
    #[case::user_info("user@example.com", ServerAddressError::UserInfo)]
    #[case::underscore("exa_mple.com", ServerAddressError::InvalidHost)]
    #[case::non_ascii("bücher.example", ServerAddressError::InvalidHost)]
    #[case::label_of_64_chars(&format!("{}.com", "a".repeat(64)), ServerAddressError::InvalidHost)]
    #[case::leading_hyphen("-example.com", ServerAddressError::InvalidHost)]
    #[case::trailing_hyphen("example-.com", ServerAddressError::InvalidHost)]
    #[case::empty_label("example..com", ServerAddressError::InvalidHost)]
    #[case::leading_dot(".example.com", ServerAddressError::InvalidHost)]
    #[case::trailing_dot("example.com.", ServerAddressError::InvalidHost)]
    #[case::space("example .com", ServerAddressError::InvalidHost)]
    #[case::newline("example.com\n", ServerAddressError::InvalidHost)]
    #[case::numeric_last_label("1.2.3", ServerAddressError::InvalidHost)]
    #[case::ipv4_as_a_number("2130706433", ServerAddressError::InvalidHost)]
    #[case::hex_ipv4("0x7f.1", ServerAddressError::InvalidHost)]
    #[case::hex_ipv4_as_a_number("0x7f000001", ServerAddressError::InvalidHost)]
    #[case::hex_last_part("127.0.0.0x1", ServerAddressError::InvalidHost)]
    #[case::last_label_starts_with_a_digit("example.1com", ServerAddressError::InvalidHost)]
    #[case::ipv4_out_of_range("999.1.1.1", ServerAddressError::InvalidHost)]
    #[case::empty_host(":25565", ServerAddressError::InvalidHost)]
    #[case::unclosed_bracket("[2001:db8::1", ServerAddressError::InvalidHost)]
    #[case::domain_in_brackets("[example.com]:25565", ServerAddressError::InvalidHost)]
    #[case::ipv6_with_an_unbracketed_port("::1:25565", ServerAddressError::InvalidHost)]
    #[case::junk_after_brackets("[::1]x", ServerAddressError::InvalidPort)]
    #[case::empty_port("example.com:", ServerAddressError::InvalidPort)]
    #[case::port_zero("example.com:0", ServerAddressError::InvalidPort)]
    #[case::port_too_high("example.com:65536", ServerAddressError::InvalidPort)]
    #[case::signed_port("example.com:+80", ServerAddressError::InvalidPort)]
    #[case::word_port("example.com:abc", ServerAddressError::InvalidPort)]
    #[case::space_in_port("example.com:2556 5", ServerAddressError::InvalidPort)]
    fn rejects(#[case] input: &str, #[case] error: ServerAddressError) {
        assert_eq!(parse(input), Err(error));
    }

    #[test]
    fn deserialize_rejects_an_invalid_address() {
        assert!(serde_json::from_str::<ServerAddress>("\"tcp://example.com\"").is_err());
    }

    #[test]
    fn serializes_the_normalized_form() {
        let address = parse("Example.COM:25566").unwrap();
        assert_eq!(
            serde_json::to_string(&address).unwrap(),
            "\"example.com:25566\""
        );
    }

    fn domain_name() -> impl Strategy<Value = String> {
        "[a-z]([a-z0-9-]{0,8}[a-z0-9])?(\\.[a-z]([a-z0-9-]{0,8}[a-z0-9])?){0,3}"
    }

    fn valid_address() -> impl Strategy<Value = String> {
        let host = prop_oneof![
            domain_name(),
            any::<Ipv4Addr>().prop_map(|ip| ip.to_string()),
            any::<Ipv6Addr>().prop_map(|ip| format!("[{ip}]")),
        ];
        (host, proptest::option::of(1..=u16::MAX)).prop_map(|(host, port)| match port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        })
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = parse(&text);
        }

        #[test]
        fn never_panics_on_address_like_text(text in "[a-z0-9.:\\[\\]/@_-]{0,300}") {
            let _ = parse(&text);
        }

        #[test]
        fn valid_addresses_round_trip(text in valid_address()) {
            let address = parse(&text).unwrap();
            prop_assert_eq!(address.to_string(), text);
            prop_assert_eq!(parse(&address.to_string()), Ok(address));
        }
    }
}
