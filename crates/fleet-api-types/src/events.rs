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
//! # The kinds and what each always carries (ADR-0020, R14, R28)
//! Every event about a bot, its chat or a mode reaches only a viewer who
//! may see it, and is built for them when it's sent: what it carries equals
//! what `GET /bots/{id}`, `GET /bots/{id}/chat` or `GET /modes` would answer
//! them at that moment (S32–S34). An event they may not see leaves no trace:
//! no frame, and nothing counts frames.
//! - `ready`, `resync`: only `type`.
//! - `bot`: `bot_id` and `bot`, the whole [`BotResponse`] this viewer's
//!   `GET /bots/{id}` answers now. It comes whenever what that answer shows
//!   may have changed, and may repeat an unchanged one.
//! - `bot_removed`: `bot_id`. The viewer may no longer see the bot, or it's
//!   gone.
//! - `bot_died`: `bot_id` and `at`, when the server heard it. The bot
//!   respawns on its own.
//! - `bot_alert`: `bot_id`, `at`, `state` (`paused` or `failed`) and its
//!   `reason`: the bot needs a human.
//! - `chat`: `bot_id` and `line`, the [`ChatLine`] this viewer's history
//!   shows for it; a user's line (`kind` `user`) also always carries its
//!   `request`.
//! - `chat_sent`: `bot_id` and `request`: the bot sent that user's line.
//! - `chat_failed`: `bot_id` and `request`: it didn't. Viewers with Control or
//!   more also get its `failure`, when the server knows one.
//! - `mode`: `mode_id` and `mode`, the [`ModeResponse`] this viewer's
//!   `GET /modes` lists for it now: created, or changed.
//! - `mode_removed`: `mode_id`. The viewer may no longer see the mode, or
//!   it's gone.

use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::{IntoParams, PartialSchema as _, ToSchema};

use crate::bots::{BotResponse, BotStateKind, BotStateReason};
use crate::chat::{ChatFailure, ChatLine, ChatLineKind};
use crate::ids::{BotId, ChatRequestId, ModeId, id_schema};
use crate::int::integer_schema;
use crate::mfa::TOKEN_MAX_CHARS;
use crate::modes::ModeResponse;
use crate::open::{Open, OpenEnum};
use crate::secret::SecretText;
use crate::time::Timestamp;

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
    /// A bot as the viewer's `GET /bots/{id}` shows it now.
    Bot,
    /// The viewer may no longer see a bot, or it's gone.
    BotRemoved,
    /// A bot died, and respawns on its own.
    BotDied,
    /// A bot paused or failed: it needs a human.
    BotAlert,
    /// A line of a bot's chat, as the viewer's history shows it.
    Chat,
    /// The bot sent a user's line.
    ChatSent,
    /// The bot didn't send a user's line.
    ChatFailed,
    /// A mode as the viewer's `GET /modes` lists it now.
    Mode,
    /// The viewer may no longer see a mode, or it's gone.
    ModeRemoved,
}

impl OpenEnum for EventMessageKind {
    const ALL: &'static [Self] = &[
        Self::Ready,
        Self::Resync,
        Self::Bot,
        Self::BotRemoved,
        Self::BotDied,
        Self::BotAlert,
        Self::Chat,
        Self::ChatSent,
        Self::ChatFailed,
        Self::Mode,
        Self::ModeRemoved,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Resync => "resync",
            Self::Bot => "bot",
            Self::BotRemoved => "bot_removed",
            Self::BotDied => "bot_died",
            Self::BotAlert => "bot_alert",
            Self::Chat => "chat",
            Self::ChatSent => "chat_sent",
            Self::ChatFailed => "chat_failed",
            Self::Mode => "mode",
            Self::ModeRemoved => "mode_removed",
        }
    }
}

/// One frame of the live events socket: what happened, by its `type`. The
/// `events` module's docs say which frames come, which fields each kind
/// always carries, and how a client reads one. Every field but `type` is
/// left out where its kind doesn't carry it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct EventMessage {
    /// What happened.
    #[serde(rename = "type")]
    #[ts(as = "EventMessageKind")]
    #[schema(value_type = EventMessageKind)]
    pub kind: Open<EventMessageKind>,
    /// The bot it's about: every `bot…` and `chat…` kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub bot_id: Option<BotId>,
    /// `bot`: the bot as the viewer's `GET /bots/{id}` shows it now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub bot: Option<BotResponse>,
    /// `bot_died`, `bot_alert`: when the server heard it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub at: Option<Timestamp>,
    /// `bot_alert`: the state the bot rests in, `paused` or `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<BotStateKind>", optional)]
    #[schema(value_type = Option<BotStateKind>, nullable = false)]
    pub state: Option<Open<BotStateKind>>,
    /// `bot_alert`: why it rests there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<BotStateReason>", optional)]
    #[schema(value_type = Option<BotStateReason>, nullable = false)]
    pub reason: Option<Open<BotStateReason>>,
    /// `chat`: the line as the viewer's history shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub line: Option<ChatLine>,
    /// `chat_sent`, `chat_failed`, and `chat` for a line a user sent: the
    /// chat request, as `POST /bots/{id}/chat` answered it to its sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub request: Option<ChatRequestId>,
    /// `chat_failed`, for viewers with Control or more, when the server
    /// knows it: why the line failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<ChatFailure>", optional)]
    #[schema(value_type = Option<ChatFailure>, nullable = false)]
    pub failure: Option<Open<ChatFailure>>,
    /// The mode it's about: `mode` and `mode_removed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub mode_id: Option<ModeId>,
    /// `mode`: the mode as the viewer's `GET /modes` lists it now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub mode: Option<ModeResponse>,
}

impl EventMessage {
    /// A frame of `kind` without any other field.
    fn of(kind: EventMessageKind) -> Self {
        Self {
            kind: kind.into(),
            bot_id: None,
            bot: None,
            at: None,
            state: None,
            reason: None,
            line: None,
            request: None,
            failure: None,
            mode_id: None,
            mode: None,
        }
    }

    /// The first frame of every socket.
    #[must_use]
    pub fn ready() -> Self {
        Self::of(EventMessageKind::Ready)
    }

    /// Events were lost: fetch again.
    #[must_use]
    pub fn resync() -> Self {
        Self::of(EventMessageKind::Resync)
    }

    /// `bot`: `bot` as the viewer's `GET /bots/{id}` shows it.
    #[must_use]
    pub fn bot(bot: BotResponse) -> Self {
        Self {
            bot_id: Some(bot.id),
            bot: Some(bot),
            ..Self::of(EventMessageKind::Bot)
        }
    }

    /// `bot_removed`: the viewer may no longer see `bot`, or it's gone.
    #[must_use]
    pub fn bot_removed(bot: BotId) -> Self {
        Self {
            bot_id: Some(bot),
            ..Self::of(EventMessageKind::BotRemoved)
        }
    }

    /// `bot_died`: `bot` died, as the server heard `at`.
    #[must_use]
    pub fn bot_died(bot: BotId, at: Timestamp) -> Self {
        Self {
            bot_id: Some(bot),
            at: Some(at),
            ..Self::of(EventMessageKind::BotDied)
        }
    }

    /// `bot_alert`: `bot` rests in `state` for `reason`, as the server heard
    /// `at`.
    #[must_use]
    pub fn bot_alert(
        bot: BotId,
        at: Timestamp,
        state: BotStateKind,
        reason: BotStateReason,
    ) -> Self {
        Self {
            bot_id: Some(bot),
            at: Some(at),
            state: Some(state.into()),
            reason: Some(reason.into()),
            ..Self::of(EventMessageKind::BotAlert)
        }
    }

    /// `chat`: `line` of `bot`'s chat, a user's line with its `request`.
    #[must_use]
    pub fn chat(bot: BotId, line: ChatLine, request: Option<ChatRequestId>) -> Self {
        Self {
            bot_id: Some(bot),
            line: Some(line),
            request,
            ..Self::of(EventMessageKind::Chat)
        }
    }

    /// `chat_sent`: `bot` sent the line of `request`.
    #[must_use]
    pub fn chat_sent(bot: BotId, request: ChatRequestId) -> Self {
        Self {
            bot_id: Some(bot),
            request: Some(request),
            ..Self::of(EventMessageKind::ChatSent)
        }
    }

    /// `chat_failed`: `bot` didn't send the line of `request`, for `failure`
    /// when this viewer may know it.
    #[must_use]
    pub fn chat_failed(bot: BotId, request: ChatRequestId, failure: Option<ChatFailure>) -> Self {
        Self {
            bot_id: Some(bot),
            request: Some(request),
            failure: failure.map(Open::from),
            ..Self::of(EventMessageKind::ChatFailed)
        }
    }

    /// `mode`: `mode` as the viewer's `GET /modes` lists it.
    #[must_use]
    pub fn mode(mode: ModeResponse) -> Self {
        Self {
            mode_id: Some(mode.id),
            mode: Some(mode),
            ..Self::of(EventMessageKind::Mode)
        }
    }

    /// `mode_removed`: the viewer may no longer see `mode`, or it's gone.
    #[must_use]
    pub fn mode_removed(mode: ModeId) -> Self {
        Self {
            mode_id: Some(mode),
            ..Self::of(EventMessageKind::ModeRemoved)
        }
    }
}

/// What a client makes of a frame's text, by the module's reading rules.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// An event this version knows, with every field its kind carries.
    Event(Box<EventMessage>),
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
        let Some(kind) = self.kind.known() else {
            return Received::Ignored;
        };
        let bot = self.bot_id.is_some();
        let carried = match kind {
            EventMessageKind::Ready | EventMessageKind::Resync => true,
            EventMessageKind::Bot => bot && self.bot.is_some(),
            EventMessageKind::BotRemoved => bot,
            EventMessageKind::BotDied => bot && self.at.is_some(),
            EventMessageKind::BotAlert => {
                bot && self.at.is_some() && self.state.is_some() && self.reason.is_some()
            }
            // A user's line always carries its request (R28).
            EventMessageKind::Chat => self.line.as_ref().is_some_and(|line| {
                bot && (line.kind.known() != Some(ChatLineKind::User) || self.request.is_some())
            }),
            EventMessageKind::ChatSent | EventMessageKind::ChatFailed => {
                bot && self.request.is_some()
            }
            EventMessageKind::Mode => self.mode_id.is_some() && self.mode.is_some(),
            EventMessageKind::ModeRemoved => self.mode_id.is_some(),
        };
        if carried {
            Received::Event(Box::new(self))
        } else {
            Received::Unreadable
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
            [
                "ready",
                "resync",
                "bot",
                "bot_removed",
                "bot_died",
                "bot_alert",
                "chat",
                "chat_sent",
                "chat_failed",
                "mode",
                "mode_removed"
            ]
        );
    }

    const BOT: &str = "0190a5b0-0000-7000-8000-000000000001";
    const MODE: &str = "0190a5b0-0000-7000-8000-000000000002";
    const REQUEST: &str = "0190a5b0-0000-7000-8000-000000000003";
    const AT: &str = "2027-01-15T08:00:00.000Z";

    fn bot_id() -> BotId {
        serde_json::from_value(json!(BOT)).unwrap()
    }

    fn request() -> ChatRequestId {
        serde_json::from_value(json!(REQUEST)).unwrap()
    }

    fn at() -> Timestamp {
        serde_json::from_value(json!(AT)).unwrap()
    }

    fn a_bot() -> BotResponse {
        serde_json::from_value(json!({
            "id": BOT,
            "account": {
                "id": "0190a5b0-0000-7000-8000-000000000004",
                "name": "AfkBot1",
                "kind": "offline",
                "status": "active",
                "level": "view"
            },
            "mode_id": MODE,
            "desired_state": "stopped",
            "status": { "kind": "unplaced", "why": "stopped", "state": "stopped" }
        }))
        .unwrap()
    }

    fn a_line() -> ChatLine {
        serde_json::from_value(json!({
            "id": 41,
            "at": AT,
            "direction": "out",
            "kind": "user",
            "text": "/login",
            "truncated": false,
            "arguments_hidden": true,
            "mine": true
        }))
        .unwrap()
    }

    /// A system message the bot received: no user sent it, so it has no
    /// request.
    fn a_system_line() -> ChatLine {
        serde_json::from_value(json!({
            "id": 40,
            "at": AT,
            "direction": "in",
            "kind": "system",
            "text": "Welcome",
            "truncated": false,
            "arguments_hidden": false,
            "mine": false
        }))
        .unwrap()
    }

    #[test]
    fn a_users_line_without_its_request_is_unreadable() {
        let line = EventMessage::chat(bot_id(), a_line(), None);
        let frame = serde_json::to_string(&line).unwrap();

        assert_eq!(message(&frame).received(), Received::Unreadable, "{frame}");
    }

    fn a_mode() -> ModeResponse {
        serde_json::from_value(json!({
            "id": MODE,
            "name": "home",
            "visibility": "shared",
            "ownership": "someone_else",
            "editable": false,
            "deletable": false,
            "visibility_editable": false,
            "version": 2,
            "created_at": AT,
            "updated_at": AT,
            "steps": []
        }))
        .unwrap()
    }

    /// One frame of every kind, with every field it can carry.
    fn every_kind() -> Vec<EventMessage> {
        vec![
            EventMessage::ready(),
            EventMessage::resync(),
            EventMessage::bot(a_bot()),
            EventMessage::bot_removed(bot_id()),
            EventMessage::bot_died(bot_id(), at()),
            EventMessage::bot_alert(
                bot_id(),
                at(),
                BotStateKind::Paused,
                BotStateReason::DuplicateLogin,
            ),
            EventMessage::chat(bot_id(), a_line(), Some(request())),
            EventMessage::chat_sent(bot_id(), request()),
            EventMessage::chat_failed(bot_id(), request(), Some(ChatFailure::ChatUnavailable)),
            EventMessage::mode(a_mode()),
            EventMessage::mode_removed(serde_json::from_value(json!(MODE)).unwrap()),
        ]
    }

    /// The fields a frame's JSON holds, `type` first.
    fn fields(message: &EventMessage) -> Vec<String> {
        let value = serde_json::to_value(message).unwrap();
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort_by_key(|key| (key != "type", key.clone()));
        keys
    }

    #[test]
    fn each_kind_carries_exactly_its_fields() {
        let expected: &[&[&str]] = &[
            &["type"],
            &["type"],
            &["type", "bot", "bot_id"],
            &["type", "bot_id"],
            &["type", "at", "bot_id"],
            &["type", "at", "bot_id", "reason", "state"],
            &["type", "bot_id", "line", "request"],
            &["type", "bot_id", "request"],
            &["type", "bot_id", "failure", "request"],
            &["type", "mode", "mode_id"],
            &["type", "mode_id"],
        ];

        let shown: Vec<Vec<String>> = every_kind().iter().map(fields).collect();

        assert_eq!(shown, expected);
    }

    #[test]
    fn each_kinds_values_are_on_the_wire() {
        let alert = serde_json::to_value(EventMessage::bot_alert(
            bot_id(),
            at(),
            BotStateKind::Failed,
            BotStateReason::Banned,
        ))
        .unwrap();
        let failed = serde_json::to_value(EventMessage::chat_failed(
            bot_id(),
            request(),
            Some(ChatFailure::RateLimited),
        ))
        .unwrap();
        let bot = serde_json::to_value(EventMessage::bot(a_bot())).unwrap();
        let mode = serde_json::to_value(EventMessage::mode(a_mode())).unwrap();

        assert_eq!(
            alert,
            json!({
                "type": "bot_alert", "bot_id": BOT, "at": AT,
                "state": "failed", "reason": "banned"
            })
        );
        assert_eq!(
            failed,
            json!({
                "type": "chat_failed", "bot_id": BOT, "request": REQUEST,
                "failure": "rate_limited"
            })
        );
        assert_eq!(bot["bot_id"], json!(BOT));
        assert_eq!(bot["bot"], serde_json::to_value(a_bot()).unwrap());
        assert_eq!(mode["mode_id"], json!(MODE));
        assert_eq!(mode["mode"], serde_json::to_value(a_mode()).unwrap());
    }

    #[test]
    fn every_kind_with_its_fields_is_an_event() {
        for sent in every_kind() {
            let frame = serde_json::to_string(&sent).unwrap();

            assert_eq!(
                message(&frame).received(),
                Received::Event(Box::new(sent)),
                "{frame}"
            );
        }
    }

    #[test]
    fn a_failure_left_out_and_a_line_without_its_request_are_events() {
        let failed = EventMessage::chat_failed(bot_id(), request(), None);
        let line = EventMessage::chat(bot_id(), a_system_line(), None);

        for sent in [failed, line] {
            let frame = serde_json::to_string(&sent).unwrap();

            assert_eq!(
                message(&frame).received(),
                Received::Event(Box::new(sent)),
                "{frame}"
            );
        }
    }

    #[rstest]
    #[case::bot(2, "bot_id")]
    #[case::bot_without_its_bot(2, "bot")]
    #[case::bot_removed(3, "bot_id")]
    #[case::bot_died(4, "bot_id")]
    #[case::bot_died_without_its_time(4, "at")]
    #[case::bot_alert(5, "bot_id")]
    #[case::bot_alert_without_its_time(5, "at")]
    #[case::bot_alert_without_its_state(5, "state")]
    #[case::bot_alert_without_its_reason(5, "reason")]
    #[case::chat(6, "bot_id")]
    #[case::chat_without_its_line(6, "line")]
    #[case::chat_sent(7, "bot_id")]
    #[case::chat_sent_without_its_request(7, "request")]
    #[case::chat_failed(8, "bot_id")]
    #[case::chat_failed_without_its_request(8, "request")]
    #[case::mode(9, "mode_id")]
    #[case::mode_without_its_mode(9, "mode")]
    #[case::mode_removed(10, "mode_id")]
    fn a_known_kind_missing_its_field_is_unreadable(#[case] kind: usize, #[case] field: &str) {
        let mut value = serde_json::to_value(&every_kind()[kind]).unwrap();
        value.as_object_mut().unwrap().remove(field).unwrap();

        let read: EventMessage = serde_json::from_value(value).unwrap();

        assert_eq!(read.received(), Received::Unreadable);
    }

    #[test]
    fn a_field_of_the_wrong_type_doesnt_read() {
        let frame = json!({ "type": "bot_died", "bot_id": BOT, "at": 7 });

        assert!(serde_json::from_value::<EventMessage>(frame).is_err());
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
        assert_eq!(message(frame).received(), Received::Event(Box::new(event)));
    }

    #[test]
    fn an_unknown_type_is_ignored() {
        let frame =
            r#"{"type":"bot_teleported","bot_id":"0190a5b0-0000-7000-8000-000000000001","to":"x"}"#;

        assert_eq!(message(frame).received(), Received::Ignored);
    }

    #[test]
    fn a_field_only_a_newer_server_sends_is_ignored() {
        assert_eq!(
            message(r#"{"type":"ready","since":"2026-10-22T00:00:00.000Z"}"#).received(),
            Received::Event(Box::new(EventMessage::ready()))
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
