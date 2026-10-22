//! The live events (Plan.md P11.5; ADR-0020, S28–S37): a WebSocket that
//! streams what happens to the bots and modes a user may see, so the app
//! patches what it fetched instead of polling.
//!
//! # Opening it
//! 1. `POST /api/v1/events/ticket` answers an [`EventTicketResponse`]: a
//!    ticket for one socket, which lives [`EVENT_TICKET_MAX_SECS`] seconds
//!    and belongs to the session that asked. It's a secret: never logged,
//!    never shown, asked for anew before every connect.
//! 2. `GET /api/v1/events?ticket=…` ([`EventsQuery`]), with the same
//!    session's `Authorization: Bearer …` header, upgrades to the socket.
//!    Every ticket that isn't a live one of this session gets the same 404;
//!    a user with too many sockets open gets 429 `too_many_connections`,
//!    with `Retry-After`.
//!
//! # Frames
//! - **The server sends** JSON text frames, one [`EventMessage`] each, and
//!   never a binary frame. The first is `ready`: the socket listens from
//!   then on, so the client fetches what it shows after it, and applies
//!   what arrives meanwhile on top.
//! - **`resync`:** the socket fell behind and events were lost; the client
//!   fetches what it shows again. It never says what or how much was lost.
//! - **Pings:** the server pings every 15 s. The client answers each with a
//!   pong (tungstenite does it on its own while the client reads) and sends
//!   nothing else but a close. Anything else (text, binary, a ping, a pong
//!   nobody asked for, a frame over 125 bytes) closes the socket with 1008;
//!   no pong for 45 s closes it with 4000. A client that hears no ping for
//!   30 s can take the server for gone.
//! - **Its life:** at most until the access token it opened with expires,
//!   and only while its session lives; then it closes with 4001, and the
//!   client asks for a new ticket, its refresh deciding whether the session
//!   lives. A client that doesn't read what the server sends is closed with
//!   4002. [`SocketClose`] lists every code the server closes with.
//!
//! # Reading a frame ([`EventMessage::received`])
//! 1. **A `type` this version doesn't know:** the frame is ignored.
//! 2. **A frame that doesn't read** (not JSON, no `type`, a field of the
//!    wrong type, a binary frame), or a known `type` without a field its
//!    kind always carries: the client treats it as `resync`.
//! 3. **The server only ever adds** kinds and optional fields: a field keeps
//!    its name and type, and a kind never loses a field it always carried.
//!
//! This version knows `ready` and `resync`; the bots', chat's and modes'
//! events arrive with P11.5's second group (ADR-0020, group D).

use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::{IntoParams, PartialSchema as _, ToSchema};

use crate::int::integer_schema;
use crate::mfa::TOKEN_MAX_CHARS;
use crate::open::{Open, OpenEnum};
use crate::secret::SecretText;

/// The longest a ticket lives, in seconds (§7.4: single use, 30 s).
pub const EVENT_TICKET_MAX_SECS: u32 = 30;

/// `POST /events/ticket`: a ticket for one live events socket.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct EventTicketResponse {
    /// The ticket, for `GET /api/v1/events?ticket=…` with the same session's
    /// access token, once, within `expires_in` seconds.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub ticket: SecretText<TOKEN_MAX_CHARS>,
    /// How many seconds the ticket has left.
    #[schema(schema_with = integer_schema::<0, EVENT_TICKET_MAX_SECS>)]
    pub expires_in: u32,
}

/// `GET /events?ticket=…`: which ticket opens the socket. A parameter that
/// isn't this one, or one given twice, is refused (400).
#[derive(Debug, Serialize, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    /// The ticket from `POST /events/ticket`. It has no rule of its own: any
    /// value that isn't a live ticket of the session gets the same 404, one
    /// too long included (ADR-0020, R17).
    #[garde(skip)]
    pub ticket: SecretText<TOKEN_MAX_CHARS>,
}

impl IntoParams for EventsQuery {
    fn into_params(_parameter_in_provider: impl Fn() -> Option<ParameterIn>) -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("ticket")
                .parameter_in(ParameterIn::Query)
                .required(Required::True)
                .description(Some(
                    "A ticket from `POST /api/v1/events/ticket`, issued to the same session within 30 s, and never used before.",
                ))
                .schema(Some(SecretText::<TOKEN_MAX_CHARS>::schema()))
                .build(),
        ]
    }
}

/// What an `EventMessage` says.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventMessageKind {
    /// The socket listens: the client fetches what it shows now.
    Ready,
    /// The socket fell behind and events were lost: the client fetches what
    /// it shows again.
    Resync,
}

impl OpenEnum for EventMessageKind {
    const ALL: &'static [Self] = &[Self::Ready, Self::Resync];

    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Resync => "resync",
        }
    }
}

/// One frame of the live events socket: what happened, by its `type`. The
/// `events` module's docs say which frames come and how a client reads one.
///
/// Every kind carries only `type` in this version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct EventMessage {
    /// What happened.
    #[serde(rename = "type")]
    #[ts(as = "EventMessageKind")]
    #[schema(value_type = EventMessageKind)]
    pub kind: Open<EventMessageKind>,
}

impl EventMessage {
    /// The first frame of every socket.
    #[must_use]
    pub fn ready() -> Self {
        Self {
            kind: EventMessageKind::Ready.into(),
        }
    }

    /// Events were lost: fetch again.
    #[must_use]
    pub fn resync() -> Self {
        Self {
            kind: EventMessageKind::Resync.into(),
        }
    }
}

/// What a client makes of a frame's text, by the module's reading rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// An event this version knows, with every field its kind carries.
    Event(EventMessage),
    /// A kind only a newer server knows: ignore it.
    Ignored,
    /// A frame that doesn't read, or a known kind without its fields: treat
    /// it as `resync`.
    Unreadable,
}

impl EventMessage {
    /// What a client makes of this frame, read from a text frame, by the
    /// [module docs](self)' rules: an event it knows with every field its
    /// kind carries, a kind only a newer server knows, or a known kind
    /// without its fields. A frame that doesn't read as an `EventMessage` at
    /// all is [`Received::Unreadable`] too; reading it is the client's.
    #[must_use]
    pub fn received(self) -> Received {
        match self.kind.known() {
            None => Received::Ignored,
            // Neither carries more than its type.
            Some(EventMessageKind::Ready | EventMessageKind::Resync) => Received::Event(self),
        }
    }
}

/// Why the server closed a socket: its close code and reason (ADR-0020, R9).
/// The codes are the contract; each reason is a fixed sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketClose {
    /// The server is shutting down (1001): reconnect with backoff.
    ShuttingDown,
    /// No pong for the idle timeout (4000): reconnect with backoff.
    Idle,
    /// The session ended, or the access token the socket opened with
    /// expired (4001): ask for a new ticket; the refresh decides whether the
    /// session lives.
    Unauthenticated,
    /// The client didn't read what the server sent in time (4002):
    /// reconnect with backoff.
    TooSlow,
    /// The client sent a frame the server doesn't take (1008).
    RefusedFrame,
    /// A read the socket needed failed (1013): reconnect with backoff.
    Busy,
}

impl SocketClose {
    /// Every close, each exactly once.
    pub const ALL: [Self; 6] = [
        Self::ShuttingDown,
        Self::Idle,
        Self::Unauthenticated,
        Self::TooSlow,
        Self::RefusedFrame,
        Self::Busy,
    ];

    /// Its close code.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::ShuttingDown => 1001,
            Self::Idle => 4000,
            Self::Unauthenticated => 4001,
            Self::TooSlow => 4002,
            Self::RefusedFrame => 1008,
            Self::Busy => 1013,
        }
    }

    /// Its reason: a fixed sentence, never runtime data, at most 123 bytes
    /// as a close frame allows.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::ShuttingDown => "The server is shutting down.",
            Self::Idle => "The app stopped answering the server's pings.",
            // `unauthorized`'s text, as the uniform 401 (R9).
            Self::Unauthenticated => "Authentication is required.",
            Self::TooSlow => "The app fell too far behind reading updates.",
            Self::RefusedFrame => {
                "Only pongs answering the server's pings, and a close, are accepted."
            }
            // `busy`'s text.
            Self::Busy => "The server is busy. Try again shortly.",
        }
    }

    /// The close with `code`, if the server closes with it.
    #[must_use]
    pub fn of_code(code: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|close| close.code() == code)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    #[test]
    fn the_kinds_wire_names_are_their_serde_names() {
        for kind in EventMessageKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                json!(kind.as_str()),
                "{kind:?}"
            );
        }
        assert_eq!(
            EventMessageKind::ALL
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>(),
            ["ready", "resync"]
        );
    }

    #[test]
    fn ready_and_resync_carry_only_their_type() {
        assert_eq!(
            serde_json::to_value(EventMessage::ready()).unwrap(),
            json!({ "type": "ready" })
        );
        assert_eq!(
            serde_json::to_value(EventMessage::resync()).unwrap(),
            json!({ "type": "resync" })
        );
    }

    /// A frame's text as a message, as a client reads it.
    fn message(frame: &str) -> EventMessage {
        serde_json::from_str(frame).unwrap()
    }

    #[rstest]
    #[case::ready(r#"{"type":"ready"}"#, EventMessage::ready())]
    #[case::resync(r#"{"type":"resync"}"#, EventMessage::resync())]
    fn a_known_event_reads_back(#[case] frame: &str, #[case] event: EventMessage) {
        assert_eq!(message(frame).received(), Received::Event(event));
    }

    #[test]
    fn an_unknown_type_is_ignored() {
        let frame = r#"{"type":"bot_teleported","bot_id":"x"}"#;

        assert_eq!(message(frame).received(), Received::Ignored);
    }

    #[test]
    fn a_field_only_a_newer_server_sends_is_ignored() {
        assert_eq!(
            message(r#"{"type":"ready","since":"2026-10-22T00:00:00.000Z"}"#).received(),
            Received::Event(EventMessage::ready())
        );
    }

    #[rstest]
    #[case::not_json("ready")]
    #[case::empty("")]
    #[case::no_type(r#"{"kind":"ready"}"#)]
    #[case::type_not_a_string(r#"{"type":1}"#)]
    #[case::type_null(r#"{"type":null}"#)]
    #[case::an_array(r#"[{"type":"ready"}]"#)]
    #[case::cut_off(r#"{"type":"rea"#)]
    fn a_frame_without_a_string_type_doesnt_read_as_a_message(#[case] frame: &str) {
        assert!(
            serde_json::from_str::<EventMessage>(frame).is_err(),
            "{frame}"
        );
    }

    #[rstest]
    #[case::shutting_down(SocketClose::ShuttingDown, 1001, "The server is shutting down.")]
    #[case::idle(
        SocketClose::Idle,
        4000,
        "The app stopped answering the server's pings."
    )]
    #[case::unauthenticated(SocketClose::Unauthenticated, 4001, "Authentication is required.")]
    #[case::too_slow(
        SocketClose::TooSlow,
        4002,
        "The app fell too far behind reading updates."
    )]
    #[case::refused_frame(
        SocketClose::RefusedFrame,
        1008,
        "Only pongs answering the server's pings, and a close, are accepted."
    )]
    #[case::busy(SocketClose::Busy, 1013, "The server is busy. Try again shortly.")]
    fn each_close_has_its_code_and_approved_reason(
        #[case] close: SocketClose,
        #[case] code: u16,
        #[case] reason: &str,
    ) {
        assert_eq!(close.code(), code);
        assert_eq!(close.reason(), reason);
        assert_eq!(SocketClose::of_code(code), Some(close));
    }

    #[test]
    fn every_reason_fits_a_close_frame_and_reads_as_written() {
        for close in SocketClose::ALL {
            assert!(close.reason().len() <= 123, "{close:?}");
            assert_eq!(
                crate::BoundedText::<123>::new(close.reason()).as_str(),
                close.reason(),
                "{close:?}"
            );
        }
    }

    #[test]
    fn the_reasons_that_repeat_an_error_code_repeat_its_text() {
        assert_eq!(
            SocketClose::Unauthenticated.reason(),
            crate::ErrorCode::Unauthorized.message()
        );
        assert_eq!(SocketClose::Busy.reason(), crate::ErrorCode::Busy.message());
    }

    #[test]
    fn a_code_the_server_never_closes_with_is_none() {
        for code in [1000, 1006, 1011, 4003, 0] {
            assert_eq!(SocketClose::of_code(code), None, "{code}");
        }
    }

    #[test]
    fn a_ticket_response_never_shows_its_ticket() {
        let response = EventTicketResponse {
            ticket: SecretText::new("afk_wt_TICKETTICKET".to_owned()),
            expires_in: 30,
        };
        let query: EventsQuery =
            serde_json::from_value(json!({ "ticket": "afk_wt_QUERYQUERY" })).unwrap();

        let shown = format!("{response:?} {query:?}");

        assert!(!shown.contains("TICKET"), "{shown}");
        assert!(!shown.contains("QUERY"), "{shown}");
    }

    #[test]
    fn the_ticket_response_round_trips_and_ignores_a_newer_field() {
        let sent = json!({ "ticket": "afk_wt_x", "expires_in": 30 });

        let back: EventTicketResponse = serde_json::from_value(sent.clone()).unwrap();
        let newer: EventTicketResponse =
            serde_json::from_value(json!({ "ticket": "afk_wt_x", "expires_in": 3, "new": 1 }))
                .unwrap();

        assert_eq!(serde_json::to_value(back).unwrap(), sent);
        assert_eq!(newer.expires_in, 3);
    }

    #[test]
    fn the_query_refuses_an_unknown_or_missing_parameter() {
        let parse = |json| serde_json::from_value::<EventsQuery>(json);

        assert!(parse(json!({ "ticket": "afk_wt_x", "since": "1" })).is_err());
        assert!(parse(json!({})).is_err());
        assert!(parse(json!({ "ticket": 1 })).is_err());
    }

    #[test]
    fn a_ticket_too_long_for_its_cap_still_reads_and_breaks_no_rule() {
        let query: EventsQuery =
            serde_json::from_value(json!({ "ticket": "x".repeat(TOKEN_MAX_CHARS + 1) })).unwrap();

        assert!(query.validate().is_ok());
    }

    #[test]
    fn the_ticket_lives_as_long_as_fleet_core_says() {
        assert_eq!(
            u64::from(EVENT_TICKET_MAX_SECS),
            fleet_core::auth::EVENT_TICKET_TTL.as_secs()
        );
    }

    #[test]
    fn the_query_documents_one_required_ticket_parameter() {
        let parameters = EventsQuery::into_params(|| None);

        let shown = serde_json::to_value(&parameters).unwrap();

        assert_eq!(shown[0]["name"], "ticket");
        assert_eq!(shown[0]["in"], "query");
        assert_eq!(shown[0]["required"], true);
        assert_eq!(shown[0]["schema"]["maxLength"], TOKEN_MAX_CHARS);
        assert_eq!(parameters.len(), 1);
    }
}
