//! [`ServerUrl`]: the server's address, which every request starts from
//! (ADR-0017).
//!
//! It's an origin only: a scheme, a host and an optional port. `https` works
//! for any host; plain `http` only for a literal loopback address
//! (127.0.0.0/8 or `[::1]`), for `just dev-server` and the tests, so a
//! password never crosses a network in clear. The name `localhost` is
//! refused for `http`, since name resolution could point it elsewhere.

use core::fmt;
use std::net::IpAddr;

use reqwest::Url;
use thiserror::Error;

/// The server's address; see the [module docs](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerUrl(Url);

/// Why a text isn't a [`ServerUrl`]. The message never repeats the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ServerUrlError {
    /// It doesn't parse as a URL.
    #[error("isn't a URL")]
    NotAUrl,
    /// Its scheme is neither `https` nor `http`.
    #[error("must start with https://")]
    Scheme,
    /// It has no host.
    #[error("has no host")]
    NoHost,
    /// It holds a user name or a password.
    #[error("must not hold a user name or password")]
    UserInfo,
    /// It has a path other than `/`.
    #[error("must not have a path: give only the server's address")]
    Path,
    /// It has a query, even an empty one.
    #[error("must not have a query")]
    Query,
    /// It has a fragment, even an empty one.
    #[error("must not have a fragment")]
    Fragment,
    /// It's plain `http` to a host other than a loopback address.
    #[error("plain http:// works only for a loopback address (127.0.0.1 or [::1]): use https://")]
    PlainHttp,
}

impl ServerUrl {
    /// Parses `text` as the server's address.
    ///
    /// # Errors
    /// [`ServerUrlError`] names the first rule `text` breaks.
    pub fn parse(text: &str) -> Result<Self, ServerUrlError> {
        let url = Url::parse(text).map_err(|_| ServerUrlError::NotAUrl)?;
        let plain = match url.scheme() {
            "https" => false,
            "http" => true,
            _ => return Err(ServerUrlError::Scheme),
        };
        let Some(host) = url.host_str() else {
            return Err(ServerUrlError::NoHost);
        };
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ServerUrlError::UserInfo);
        }
        if url.path() != "/" {
            return Err(ServerUrlError::Path);
        }
        if url.query().is_some() {
            return Err(ServerUrlError::Query);
        }
        if url.fragment().is_some() {
            return Err(ServerUrlError::Fragment);
        }
        // The URL parser writes an IP host in its canonical form, an IPv6
        // one in brackets; a name never parses as an address.
        let loopback = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(is_loopback);
        if plain && !loopback {
            return Err(ServerUrlError::PlainHttp);
        }
        Ok(Self(url))
    }

    /// The address as text, such as `https://example.com/`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The URL of the API route `/api/v1/<segments…>`.
    pub(crate) fn api(&self, segments: &[&str]) -> Url {
        self.join(["api", "v1"].iter().chain(segments))
    }

    /// The live events socket's URL, `/api/v1/events` with `ticket` in its
    /// query, on the server's own scheme: its upgrade is an HTTP/1.1 request
    /// through reqwest (ADR-0020, U25, R80). It holds the ticket, so it's
    /// never shown.
    pub(crate) fn events(&self, ticket: &str) -> Url {
        let mut url = self.api(&["events"]);
        url.query_pairs_mut().append_pair("ticket", ticket);
        url
    }

    /// The URL of the health route `/health/<which>`.
    pub(crate) fn health(&self, which: &str) -> Url {
        self.join(["health", which].iter())
    }

    /// The origin with `segments` as its path. Each segment is escaped, so
    /// an ID can't add a segment of its own.
    fn join<'a>(&self, segments: impl Iterator<Item = &'a &'a str>) -> Url {
        let mut url = self.0.clone();
        // An http(s) URL always has a path to extend; `parse` refused every
        // other scheme.
        if let Ok(mut path) = url.path_segments_mut() {
            path.clear().extend(segments);
        }
        url
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether `ip` is a loopback address: 127.0.0.0/8 or `::1`.
fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain(
        "http://127.0.0.1:8080",
        "http://127.0.0.1:8080/api/v1/events?ticket=afk_wt_x-y_z"
    )]
    #[case::tls(
        "https://example.com",
        "https://example.com/api/v1/events?ticket=afk_wt_x-y_z"
    )]
    fn the_events_url_keeps_its_servers_scheme_and_takes_the_ticket(
        #[case] server: &str,
        #[case] events: &str,
    ) {
        let url = ServerUrl::parse(server).unwrap().events("afk_wt_x-y_z");

        assert_eq!(url.as_str(), events);
    }

    #[rstest]
    #[case::https("https://example.com", "https://example.com/")]
    #[case::https_slash("https://example.com/", "https://example.com/")]
    #[case::https_port("https://example.com:8443", "https://example.com:8443/")]
    #[case::https_ip("https://192.0.2.10", "https://192.0.2.10/")]
    #[case::uppercase("HTTPS://EXAMPLE.COM", "https://example.com/")]
    #[case::http_loopback("http://127.0.0.1:8080", "http://127.0.0.1:8080/")]
    #[case::http_loopback_range("http://127.1.2.3:8080", "http://127.1.2.3:8080/")]
    #[case::http_loopback_v6("http://[::1]:8080", "http://[::1]:8080/")]
    fn an_origin_is_accepted(#[case] text: &str, #[case] shown: &str) {
        let url = ServerUrl::parse(text).unwrap();

        assert_eq!(url.as_str(), shown);
        assert_eq!(url.to_string(), shown);
    }

    #[rstest]
    #[case::empty("", ServerUrlError::NotAUrl)]
    #[case::no_scheme("example.com", ServerUrlError::NotAUrl)]
    #[case::ftp("ftp://example.com", ServerUrlError::Scheme)]
    #[case::ws("wss://example.com", ServerUrlError::Scheme)]
    #[case::file("file:///etc/passwd", ServerUrlError::Scheme)]
    #[case::user("https://alice@example.com", ServerUrlError::UserInfo)]
    #[case::password("https://alice:hunter2@example.com", ServerUrlError::UserInfo)]
    #[case::path("https://example.com/afkfleet", ServerUrlError::Path)]
    #[case::api_path("https://example.com/api/v1", ServerUrlError::Path)]
    #[case::query("https://example.com/?x=1", ServerUrlError::Query)]
    #[case::empty_query("https://example.com/?", ServerUrlError::Query)]
    #[case::fragment("https://example.com/#top", ServerUrlError::Fragment)]
    #[case::http_remote("http://example.com", ServerUrlError::PlainHttp)]
    #[case::http_lan("http://192.0.2.10:8080", ServerUrlError::PlainHttp)]
    #[case::http_localhost("http://localhost:8080", ServerUrlError::PlainHttp)]
    #[case::http_mapped("http://[::ffff:127.0.0.1]:8080", ServerUrlError::PlainHttp)]
    #[case::http_unspecified("http://0.0.0.0:8080", ServerUrlError::PlainHttp)]
    fn anything_else_is_refused_without_repeating_it(
        #[case] text: &str,
        #[case] error: ServerUrlError,
    ) {
        let refused = ServerUrl::parse(text).unwrap_err();

        assert_eq!(refused, error, "{text}");
        assert!(!refused.to_string().contains("alice"), "{text}");
        assert!(!refused.to_string().contains("hunter2"), "{text}");
    }

    #[test]
    fn routes_are_built_on_the_origin() {
        let url = ServerUrl::parse("https://example.com:8443").unwrap();

        assert_eq!(
            url.api(&["users", "01900000-0000-7000-8000-000000000001"])
                .as_str(),
            "https://example.com:8443/api/v1/users/01900000-0000-7000-8000-000000000001"
        );
        assert_eq!(
            url.health("ready").as_str(),
            "https://example.com:8443/health/ready"
        );
    }

    #[test]
    fn a_segment_cant_add_a_segment_of_its_own() {
        let url = ServerUrl::parse("https://example.com").unwrap();

        assert_eq!(
            url.api(&["users", "../../health/live"]).as_str(),
            "https://example.com/api/v1/users/..%2F..%2Fhealth%2Flive"
        );
    }
}
