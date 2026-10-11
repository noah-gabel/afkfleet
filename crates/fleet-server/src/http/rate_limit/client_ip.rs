//! Who a request comes from (Plan.md P7.10, §7.4, ADR-0016): [`client_ip`]
//! resolves the client's address once per request, in the middleware
//! stack, and [`ClientIp`] hands it to the limiters and the handlers.
//!
//! - **The peer** is the socket's address (`ConnectInfo`, which `serve`
//!   provides through `into_make_service_with_connect_info`), in its
//!   canonical form.
//! - **`X-Forwarded-For` counts only when the peer is a trusted proxy**
//!   (`[http] trusted_proxies`). All its headers form one list in order;
//!   the list is walked from the right past trusted proxies, and the first
//!   address that isn't one is the client. The left-most entry is never
//!   trusted for that: the client writes it. An entry that isn't a plain IP
//!   address stops the walk at the last trusted hop, so nothing left of it
//!   is read. If every entry is a trusted proxy, the left-most one is the
//!   client.
//! - **`Forwarded` and `X-Real-IP` are never read.**

use std::net::IpAddr;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName};

use super::net::TrustedProxies;
use crate::http::error::ApiError;

/// The header a proxy adds the address it received a request from to.
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// The client a request comes from, in canonical form: the full address,
/// as the audit log records it. The middleware stack puts it into every
/// request's extensions; a handler takes it as an extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

/// A request reached the stack without its peer's address: `serve` must use
/// `into_make_service_with_connect_info`. A wiring bug, answered as an
/// internal error.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error(
    "the request has no peer address: the server must be served with into_make_service_with_connect_info"
)]
pub struct MissingPeerAddress;

/// A handler or a route layer asked for the client's address on a request
/// that didn't pass the middleware stack. A wiring bug, answered as an
/// internal error.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("the request has no client address: it didn't pass the middleware stack")]
pub struct MissingClientIp;

/// The client's address, from the socket's `peer` and the request's
/// `headers`, in canonical form:
/// - **An untrusted peer is the client;** its `X-Forwarded-For` is ignored.
/// - **Behind a trusted proxy** (`proxies`), all `X-Forwarded-For` headers
///   form one list in order, walked from the right past trusted proxies:
///   the first address that isn't one is the client, never the left-most
///   entry, which the client writes.
/// - **An entry that isn't a plain IP address** (garbage, `unknown`, a port)
///   stops the walk at the last trusted hop; nothing left of it is read.
/// - **If every entry is a trusted proxy,** the left-most one is the client.
#[must_use]
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, proxies: &TrustedProxies) -> IpAddr {
    let peer = peer.to_canonical();
    if !proxies.contains(peer) {
        return peer;
    }
    // The address the walk has reached: the peer, then each trusted hop.
    let mut client = peer;
    for value in headers.get_all(X_FORWARDED_FOR).iter().rev() {
        let Ok(text) = value.to_str() else {
            return client;
        };
        for entry in text.rsplit(',') {
            let Ok(address) = entry.trim().parse::<IpAddr>() else {
                return client;
            };
            let address = address.to_canonical();
            if !proxies.contains(address) {
                return address;
            }
            client = address;
        }
    }
    client
}

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        core::future::ready(
            parts
                .extensions
                .get::<Self>()
                .copied()
                .ok_or_else(|| ApiError::internal(MissingClientIp)),
        )
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use rstest::rstest;

    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// The test's proxies: `198.51.100.0/24` and `2001:db8:ffff::/48`.
    fn proxies() -> TrustedProxies {
        TrustedProxies::new(vec![
            "198.51.100.0/24".parse().unwrap(),
            "2001:db8:ffff::/48".parse().unwrap(),
        ])
    }

    /// Headers with one `X-Forwarded-For` per value, in order.
    fn forwarded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(X_FORWARDED_FOR, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[rstest]
    #[case::untrusted_peer_without_header("192.0.2.10", &[], "192.0.2.10")]
    #[case::untrusted_peer_header_ignored("192.0.2.10", &["203.0.113.5"], "192.0.2.10")]
    #[case::trusted_peer_without_header("198.51.100.1", &[], "198.51.100.1")]
    #[case::trusted_peer_names_the_client("198.51.100.1", &["192.0.2.10"], "192.0.2.10")]
    #[case::the_right_most_untrusted_wins(
        "198.51.100.1",
        &["203.0.113.5, 192.0.2.10"],
        "192.0.2.10"
    )]
    #[case::trusted_hops_are_skipped(
        "198.51.100.1",
        &["203.0.113.5, 192.0.2.10, 198.51.100.7, 198.51.100.8"],
        "192.0.2.10"
    )]
    #[case::several_headers_are_one_list(
        "198.51.100.1",
        &["203.0.113.5, 192.0.2.10", "198.51.100.7"],
        "192.0.2.10"
    )]
    #[case::all_trusted_gives_the_left_most(
        "198.51.100.1",
        &["198.51.100.9, 198.51.100.7"],
        "198.51.100.9"
    )]
    #[case::garbage_stops_at_the_last_trusted_hop(
        "198.51.100.1",
        &["192.0.2.10, garbage, 198.51.100.7"],
        "198.51.100.7"
    )]
    #[case::unknown_stops_at_the_peer("198.51.100.1", &["unknown"], "198.51.100.1")]
    #[case::a_port_stops_the_walk("198.51.100.1", &["192.0.2.10:443"], "198.51.100.1")]
    #[case::an_empty_entry_stops_the_walk("198.51.100.1", &["192.0.2.10, "], "198.51.100.1")]
    #[case::an_empty_header_stops_the_walk("198.51.100.1", &[""], "198.51.100.1")]
    #[case::spaces_are_trimmed("198.51.100.1", &["  192.0.2.10  "], "192.0.2.10")]
    #[case::mapped_peer_is_trusted("::ffff:198.51.100.1", &["192.0.2.10"], "192.0.2.10")]
    #[case::mapped_peer_untrusted("::ffff:192.0.2.10", &["203.0.113.5"], "192.0.2.10")]
    #[case::mapped_entry_is_canonical(
        "198.51.100.1",
        &["::ffff:192.0.2.10"],
        "192.0.2.10"
    )]
    #[case::mapped_trusted_entry_is_skipped(
        "198.51.100.1",
        &["192.0.2.10, ::ffff:198.51.100.7"],
        "192.0.2.10"
    )]
    #[case::ipv6_proxy_and_client(
        "2001:db8:ffff::1",
        &["2001:db8:1::5"],
        "2001:db8:1::5"
    )]
    fn the_client_is_the_right_most_untrusted_address(
        #[case] peer: &str,
        #[case] values: &[&str],
        #[case] client: &str,
    ) {
        assert_eq!(
            client_ip(ip(peer), &forwarded(values), &proxies()),
            ip(client)
        );
    }

    #[test]
    fn a_header_that_isnt_text_stops_the_walk() {
        let mut headers = forwarded(&["203.0.113.5"]);
        headers.append(X_FORWARDED_FOR, HeaderValue::from_bytes(b"\xff").unwrap());

        assert_eq!(
            client_ip(ip("198.51.100.1"), &headers, &proxies()),
            ip("198.51.100.1")
        );
    }

    #[test]
    fn without_trusted_proxies_the_peer_is_always_the_client() {
        let headers = forwarded(&["192.0.2.10"]);

        assert_eq!(
            client_ip(ip("198.51.100.1"), &headers, &TrustedProxies::default()),
            ip("198.51.100.1")
        );
    }
}
