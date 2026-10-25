//! The live events socket (Plan.md P11.5; ADR-0020, S28, S36, R16, U15): how
//! one of its frames reads, [`read`], and, with the `dev-console` feature
//! only, what the dev console opens it with, `EventsHandshake`.
//!
//! `Client::events_handshake` asks the server for a ticket and hands back
//! the socket's URL, the ticket in its query, and the `Authorization` value
//! of the session the ticket belongs to: the server takes the ticket only
//! with that session's access token. Both are secrets, and nothing here
//! shows either. Whoever opens the socket with them must also keep its
//! WebSocket client's logs from them: tokio-tungstenite 0.29's client logs
//! the whole handshake request, the URL's ticket and `Authorization`
//! included, at `trace` (tungstenite's "Request:" line, target
//! `tungstenite::handshake::client`), and a header marked sensitive doesn't
//! change that. tungstenite's `debug` line with the URL is in its blocking
//! client's `connect_to_some`, which tokio-tungstenite never calls.

pub(crate) mod policy;
pub(crate) mod upgrade;

use fleet_api_types::events::EventMessage;
pub use fleet_api_types::events::{Received, SocketClose};
pub use policy::{
    CLOSE_TIMEOUT, READY_TIMEOUT, RECONNECT_BASE, RECONNECT_MAX, RETRY_AFTER_MAX, SILENCE_TIMEOUT,
    STABLE_AFTER,
};
#[cfg(feature = "dev-console")]
use secrecy::SecretString;

/// What a live events socket opens with; see the [module docs](self). Its
/// `Debug` shows neither secret. The ticket is single-use and lives 30 s:
/// ask for a new handshake before every connect. Only with the
/// `dev-console` feature (ADR-0020, U15).
#[cfg(feature = "dev-console")]
#[derive(Debug)]
pub struct EventsHandshake {
    url: SecretString,
    authorization: SecretString,
}

#[cfg(feature = "dev-console")]
impl EventsHandshake {
    /// The handshake to `url` with `access_token`'s session.
    pub(crate) fn new(url: &str, access_token: &SecretString) -> Self {
        use secrecy::ExposeSecret as _;

        Self {
            url: SecretString::from(url),
            authorization: SecretString::from(format!("Bearer {}", access_token.expose_secret())),
        }
    }

    /// The socket's URL, `ws://` or `wss://` as the server's address is
    /// `http://` or `https://`, with the ticket in its query: never log it.
    #[must_use]
    pub fn url(&self) -> &SecretString {
        &self.url
    }

    /// The handshake's `Authorization` header value, `Bearer` and the
    /// session's access token: mark the header sensitive, never log it.
    #[must_use]
    pub fn authorization(&self) -> &SecretString {
        &self.authorization
    }
}

/// What a client makes of one text frame, by the protocol's reading rules
/// (`fleet_api_types::events`): an event it knows, a kind only a newer
/// server knows (ignore it), or a frame that doesn't read (take it for a
/// `resync`).
#[must_use]
pub fn read(frame: &str) -> Received {
    serde_json::from_str::<EventMessage>(frame).map_or(Received::Unreadable, EventMessage::received)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "dev-console")]
    use secrecy::ExposeSecret as _;

    use super::*;

    #[test]
    fn a_known_frame_reads_as_its_event() {
        assert_eq!(
            read(r#"{"type":"ready"}"#),
            Received::Event(Box::new(EventMessage::ready()))
        );
        assert_eq!(
            read(r#"{"type":"resync","since":1}"#),
            Received::Event(Box::new(EventMessage::resync()))
        );
    }

    #[test]
    fn a_kind_only_a_newer_server_knows_is_ignored() {
        assert_eq!(read(r#"{"type":"bot_teleported"}"#), Received::Ignored);
    }

    #[test]
    fn a_frame_that_doesnt_read_is_unreadable() {
        for frame in ["", "ready", r#"{"type":1}"#, r#"{"kind":"ready"}"#, "[]"] {
            assert_eq!(read(frame), Received::Unreadable, "{frame}");
        }
    }

    #[cfg(feature = "dev-console")]
    #[test]
    fn a_handshake_shows_neither_secret() {
        let handshake = EventsHandshake::new(
            "ws://127.0.0.1:8080/api/v1/events?ticket=afk_wt_TICKETTICKET",
            &SecretString::from("afk_at_ACCESSACCESS"),
        );

        let shown = format!("{handshake:?}");

        assert!(!shown.contains("TICKET"), "{shown}");
        assert!(!shown.contains("ACCESS"), "{shown}");
        assert_eq!(
            handshake.authorization().expose_secret(),
            "Bearer afk_at_ACCESSACCESS"
        );
    }
}
