//! The live events socket's handshake (ADR-0020, U25, S54): the HTTP/1.1
//! upgrade fleet-client sends through its own reqwest client, so the socket's
//! TLS is reqwest's, and the checks its 101 must pass before a frame is read.
//!
//! tungstenite's client handshake never runs (its `trace` line logs the whole
//! request, the ticket and the bearer included): the request is reqwest's,
//! with the bearer marked sensitive, and the answer is checked here against
//! RFC 6455's rules, then the connection is framed as the client's end.

use reqwest::header::{
    CONNECTION, HeaderMap, HeaderValue, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_EXTENSIONS,
    SEC_WEBSOCKET_PROTOCOL, UPGRADE,
};
use secrecy::SecretString;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

use crate::MAX_RESPONSE_BYTES;
use crate::transport::UpgradeRequest;
use crate::url::ServerUrl;

/// The longest frame and message the socket reads: a frame the server would
/// never send ends the socket rather than filling memory (S54).
pub(crate) const MAX_EVENT_FRAME: usize = MAX_RESPONSE_BYTES;

/// The upgrade to the live events socket of `server`, spending `ticket`,
/// with the session's `bearer` and a fresh `Sec-WebSocket-Key`.
pub(crate) fn request(server: &ServerUrl, ticket: &str, bearer: SecretString) -> UpgradeRequest {
    UpgradeRequest {
        url: server.events(ticket),
        bearer,
        key: generate_key(),
    }
}

/// Why a 101 doesn't open the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handshake {
    /// No `Upgrade: websocket`.
    NoUpgrade,
    /// No `Connection: Upgrade`.
    NoConnection,
    /// A `Sec-WebSocket-Accept` that isn't the key's, or none.
    Accept,
    /// An extension or a subprotocol the client never offered.
    Unoffered,
}

/// Whether a 101's `headers` answer the upgrade that offered `key`: an
/// upgrade to `websocket`, the key's own accept, and nothing the client
/// didn't offer (RFC 6455, 4.1).
pub(crate) fn checked(headers: &HeaderMap, key: &str) -> Result<(), Handshake> {
    let names = |name, token: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|part| part.trim().eq_ignore_ascii_case(token))
    };
    if !names(UPGRADE, "websocket") {
        return Err(Handshake::NoUpgrade);
    }
    if !names(CONNECTION, "upgrade") {
        return Err(Handshake::NoConnection);
    }
    let expected = derive_accept_key(key.as_bytes());
    if headers.get(SEC_WEBSOCKET_ACCEPT).map(HeaderValue::as_bytes) != Some(expected.as_bytes()) {
        return Err(Handshake::Accept);
    }
    if headers.contains_key(SEC_WEBSOCKET_EXTENSIONS)
        || headers.contains_key(SEC_WEBSOCKET_PROTOCOL)
    {
        return Err(Handshake::Unoffered);
    }
    Ok(())
}

/// The socket's limits: frames and messages of at most [`MAX_EVENT_FRAME`].
pub(crate) fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(MAX_EVENT_FRAME))
        .max_message_size(Some(MAX_EVENT_FRAME))
}

/// `socket`, the connection a checked 101 handed over, framed as the client's
/// end of the WebSocket.
pub(crate) async fn framed<S>(socket: S) -> WebSocketStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    WebSocketStream::from_raw_socket(socket, Role::Client, Some(config())).await
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use secrecy::ExposeSecret as _;

    use super::*;

    /// A 101's headers that answer `key` as they should.
    fn answering(key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(
            SEC_WEBSOCKET_ACCEPT,
            derive_accept_key(key.as_bytes()).parse().unwrap(),
        );
        headers
    }

    #[test]
    fn an_upgrade_spends_its_ticket_with_the_bearer_and_a_fresh_key() {
        let server = ServerUrl::parse("https://example.com").unwrap();

        let first = request(&server, "afk_wt_one", SecretString::from("afk_at_one"));
        let second = request(&server, "afk_wt_two", SecretString::from("afk_at_one"));

        assert_eq!(
            first.url.as_str(),
            "https://example.com/api/v1/events?ticket=afk_wt_one"
        );
        assert_eq!(first.bearer.expose_secret(), "afk_at_one");
        assert_eq!(first.key.len(), 24, "16 random bytes in base64");
        assert_ne!(first.key, second.key);
        assert!(!format!("{first:?}").contains("afk_"), "{first:?}");
    }

    #[test]
    fn a_101_that_answers_the_key_checks_out_whatever_the_case() {
        let key = generate_key();
        let mut headers = answering(&key);
        headers.insert(UPGRADE, HeaderValue::from_static("WebSocket"));
        headers.insert(CONNECTION, HeaderValue::from_static("keep-alive, upgrade"));

        assert_eq!(checked(&headers, &key), Ok(()));
    }

    #[rstest]
    #[case::no_upgrade(UPGRADE, None, Handshake::NoUpgrade)]
    #[case::another_upgrade(UPGRADE, Some("h2c"), Handshake::NoUpgrade)]
    #[case::no_connection(CONNECTION, None, Handshake::NoConnection)]
    #[case::no_accept(SEC_WEBSOCKET_ACCEPT, None, Handshake::Accept)]
    #[case::a_wrong_accept(
        SEC_WEBSOCKET_ACCEPT,
        Some("dGhlIHNhbXBsZSBub25jZQ=="),
        Handshake::Accept
    )]
    #[case::an_extension(
        SEC_WEBSOCKET_EXTENSIONS,
        Some("permessage-deflate"),
        Handshake::Unoffered
    )]
    #[case::a_subprotocol(SEC_WEBSOCKET_PROTOCOL, Some("chat"), Handshake::Unoffered)]
    fn a_101_that_doesnt_answer_the_upgrade_is_refused(
        #[case] name: reqwest::header::HeaderName,
        #[case] value: Option<&'static str>,
        #[case] refused: Handshake,
    ) {
        let key = generate_key();
        let mut headers = answering(&key);
        match value {
            Some(value) => {
                headers.insert(name, HeaderValue::from_static(value));
            }
            None => {
                headers.remove(name);
            }
        }

        assert_eq!(checked(&headers, &key), Err(refused));
    }

    #[test]
    fn the_socket_reads_frames_and_messages_of_at_most_a_mebibyte() {
        let config = config();

        assert_eq!(config.max_frame_size, Some(1024 * 1024));
        assert_eq!(config.max_message_size, Some(1024 * 1024));
    }
}
