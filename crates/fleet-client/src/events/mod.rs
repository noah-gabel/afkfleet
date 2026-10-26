//! The live events socket (Plan.md P11.5, P11.8; ADR-0020, S28, S36, S53):
//! how one of its frames reads, [`read`], and the socket itself,
//! [`LiveEvents`], which `Client::live_events` makes. Its ticket and the
//! session's access token it opens with never leave the client: each connect
//! spends a new ticket on an upgrade of the client's own HTTP connection
//! (U25), so no WebSocket client's handshake logs either, and the reconnect
//! policy's waits and constants are this module's too (S54).

mod live;
pub(crate) mod policy;
pub(crate) mod upgrade;

use fleet_api_types::events::EventMessage;
pub use fleet_api_types::events::{Received, SocketClose};
pub use live::{Disconnect, LiveEnd, LiveEvents, LiveStop, LiveUpdate, RefetchReason};
pub use policy::{
    CLOSE_TIMEOUT, READY_TIMEOUT, RECONNECT_BASE, RECONNECT_MAX, RETRY_AFTER_MAX, SILENCE_TIMEOUT,
    STABLE_AFTER,
};

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
}
