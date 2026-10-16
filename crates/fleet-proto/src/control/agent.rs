//! What an agent sends the server.

use core::num::NonZeroU64;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::id::{BotId, ChatRequestId};
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};

use super::{bots_from_wire, bots_to_wire, request_number};
use crate::convert::{
    chat_message_from_wire, enum_from_wire, id_from_wire, id_to_wire, required, time_from_wire,
    time_to_wire,
};
use crate::generated as wire;
use crate::{ConvertError, Problem};

/// A message from an agent to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMessage {
    /// The first message on every stream.
    Hello(Hello),
    /// A sign of life.
    Heartbeat,
    /// A bot's new status.
    Status(BotSnapshot),
    /// Something that happened to a bot.
    Event(BotEvent),
    /// A bot asks for session credentials.
    SessionRequest(SessionRequest),
}

/// The first message on every stream: who the agent is, and what it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// The agent's name.
    pub name: AgentName,
    /// The agent's version, for people; the protocol version decides.
    pub agent_version: AgentVersion,
    /// The protocol version the agent speaks.
    pub protocol_version: u32,
    /// How many bots the agent may run at once.
    pub capacity: AgentCapacity,
    /// The status of every bot the agent runs. Reading a hello refuses more
    /// than [`MAX_BOTS`](super::MAX_BOTS) of them, or a bot that appears twice.
    pub running: Vec<BotSnapshot>,
}

/// Something that happened to one bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotEvent {
    /// The bot.
    pub bot_id: BotId,
    /// When it happened, by the agent's clock.
    pub at: DateTime<Utc>,
    /// What happened.
    pub kind: BotEventKind,
}

/// What happened in a [`BotEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotEventKind {
    /// The bot died; the agent respawns it.
    Died,
    /// The bot received a chat message. Its text came from a Minecraft server.
    ChatReceived(IncomingChat),
    /// A user's chat message was sent.
    ChatSent {
        /// The request the server sent it with.
        request: ChatRequestId,
    },
    /// A user's chat message wasn't sent. It isn't retried.
    ChatFailed {
        /// The request the server sent it with.
        request: ChatRequestId,
        /// Why it wasn't sent.
        reason: ChatFailedReason,
    },
    /// The bot's mode sent a chat message.
    ModeChatSent(ChatMessage),
    /// The bot paused or failed and needs a human. It comes right after the
    /// status it belongs to.
    Alert(BotNotification),
    /// The agent removed the bot; this is its last event. A paused or failed
    /// bot sends no status before it, so the server keeps that state
    /// (ADR-0013).
    Removed,
}

/// Why a user's chat message wasn't sent: the runtime's delivery failures and
/// its refusals, which the agent maps one to one (P10.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatFailedReason {
    /// The session ended before the message was sent.
    Disconnected,
    /// The server requires signed chat, and the session can't sign it.
    ChatUnavailable,
    /// The session didn't answer in time.
    TimedOut,
    /// The session was too busy.
    SessionBusy,
    /// The bot wasn't in a world.
    NotInWorld,
    /// The bot wasn't online.
    NotOnline,
    /// The bot's chat queue was full.
    QueueFull,
    /// The bot's chat rate limit was reached.
    RateLimited,
    /// The agent doesn't run the bot.
    UnknownBot,
    /// The agent was too busy to take the message.
    AgentBusy,
    /// The agent is shutting down.
    ShuttingDown,
}

/// The agent's number for one session request, counted from 1 by each agent
/// process. The server's answer repeats it, so the agent can drop an answer to
/// a request it has given up on (ADR-0019).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionRequestId(NonZeroU64);

impl SessionRequestId {
    /// Wraps a request number.
    #[must_use]
    pub const fn new(number: NonZeroU64) -> Self {
        Self(number)
    }

    /// Returns the request number.
    #[must_use]
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// A bot asks for session credentials for one connection attempt. It names no
/// account: the server knows which account the bot it assigned plays as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionRequest {
    /// The request's number.
    pub id: SessionRequestId,
    /// The bot that asks.
    pub bot_id: BotId,
    /// Whether the last session was rejected, so a cached one won't do.
    pub fresh: bool,
}

impl TryFrom<AgentMessage> for wire::AgentMessage {
    type Error = ConvertError;

    fn try_from(message: AgentMessage) -> Result<Self, ConvertError> {
        use wire::agent_message::Msg;
        let msg = match message {
            AgentMessage::Hello(hello) => Msg::Hello(wire::Hello {
                name: hello.name.into(),
                agent_version: hello.agent_version.into(),
                protocol_version: hello.protocol_version,
                capacity: hello.capacity.into(),
                running: bots_to_wire(hello.running)?,
            }),
            AgentMessage::Heartbeat => Msg::Heartbeat(wire::Heartbeat {}),
            AgentMessage::Status(snapshot) => Msg::Status(snapshot.try_into()?),
            AgentMessage::Event(event) => Msg::Event(event_to_wire(event)?),
            AgentMessage::SessionRequest(request) => Msg::SessionRequest(wire::SessionRequest {
                id: request.id.get().get(),
                bot_id: id_to_wire(request.bot_id),
                fresh: request.fresh,
            }),
        };
        Ok(Self { msg: Some(msg) })
    }
}

impl TryFrom<wire::AgentMessage> for AgentMessage {
    type Error = ConvertError;

    fn try_from(message: wire::AgentMessage) -> Result<Self, ConvertError> {
        use wire::agent_message::Msg;
        Ok(match required("AgentMessage.msg", message.msg)? {
            Msg::Hello(hello) => Self::Hello(hello_from_wire(hello)?),
            Msg::Heartbeat(wire::Heartbeat {}) => Self::Heartbeat,
            Msg::Status(snapshot) => Self::Status(snapshot.try_into()?),
            Msg::Event(event) => Self::Event(event_from_wire(event)?),
            Msg::SessionRequest(request) => Self::SessionRequest(SessionRequest {
                id: request_number("SessionRequest.id", request.id)?,
                bot_id: id_from_wire("SessionRequest.bot_id", &request.bot_id)?,
                fresh: request.fresh,
            }),
        })
    }
}

/// Reads a hello's fields; [`super::read_hello`] checks its version first.
pub(super) fn hello_from_wire(hello: wire::Hello) -> Result<Hello, ConvertError> {
    Ok(Hello {
        name: AgentName::try_from(hello.name.as_str())
            .map_err(|e| ConvertError::new("Hello.name", e.into()))?,
        agent_version: AgentVersion::try_from(hello.agent_version.as_str())
            .map_err(|e| ConvertError::new("Hello.agent_version", e.into()))?,
        protocol_version: hello.protocol_version,
        capacity: AgentCapacity::try_from(hello.capacity)
            .map_err(|e| ConvertError::new("Hello.capacity", e.into()))?,
        running: bots_from_wire("Hello.running", hello.running, |snapshot: &BotSnapshot| {
            snapshot.bot_id
        })?,
    })
}

fn event_to_wire(event: BotEvent) -> Result<wire::BotEvent, ConvertError> {
    use wire::bot_event::Kind;
    let kind = match event.kind {
        BotEventKind::Died => Kind::Died(wire::Unit {}),
        BotEventKind::ChatReceived(chat) => Kind::ChatReceived(chat.into()),
        BotEventKind::ChatSent { request } => Kind::ChatSent(wire::ChatSent {
            request_id: id_to_wire(request),
        }),
        BotEventKind::ChatFailed { request, reason } => Kind::ChatFailed(wire::ChatFailed {
            request_id: id_to_wire(request),
            reason: chat_failed_to_wire(reason).into(),
        }),
        BotEventKind::ModeChatSent(message) => Kind::ModeChatSent(wire::ModeChatSent {
            message: message.into(),
        }),
        BotEventKind::Alert(notification) => Kind::Alert(notification.into()),
        BotEventKind::Removed => Kind::Removed(wire::Unit {}),
    };
    Ok(wire::BotEvent {
        bot_id: id_to_wire(event.bot_id),
        at_unix_ms: Some(time_to_wire("BotEvent.at_unix_ms", event.at)?),
        kind: Some(kind),
    })
}

fn event_from_wire(event: wire::BotEvent) -> Result<BotEvent, ConvertError> {
    use wire::bot_event::Kind;
    let bot_id = id_from_wire("BotEvent.bot_id", &event.bot_id)?;
    let at = time_from_wire("BotEvent.at_unix_ms", event.at_unix_ms)?;
    let kind = match required("BotEvent.kind", event.kind)? {
        Kind::Died(wire::Unit {}) => BotEventKind::Died,
        Kind::ChatReceived(chat) => BotEventKind::ChatReceived(chat.try_into()?),
        Kind::ChatSent(sent) => BotEventKind::ChatSent {
            request: id_from_wire("ChatSent.request_id", &sent.request_id)?,
        },
        Kind::ChatFailed(failed) => BotEventKind::ChatFailed {
            request: id_from_wire("ChatFailed.request_id", &failed.request_id)?,
            reason: chat_failed_from_wire(failed.reason)?,
        },
        Kind::ModeChatSent(sent) => BotEventKind::ModeChatSent(chat_message_from_wire(
            "ModeChatSent.message",
            &sent.message,
        )?),
        Kind::Alert(notification) => BotEventKind::Alert(notification.try_into()?),
        Kind::Removed(wire::Unit {}) => BotEventKind::Removed,
    };
    Ok(BotEvent { bot_id, at, kind })
}

const fn chat_failed_to_wire(reason: ChatFailedReason) -> wire::ChatFailedReason {
    match reason {
        ChatFailedReason::Disconnected => wire::ChatFailedReason::Disconnected,
        ChatFailedReason::ChatUnavailable => wire::ChatFailedReason::ChatUnavailable,
        ChatFailedReason::TimedOut => wire::ChatFailedReason::TimedOut,
        ChatFailedReason::SessionBusy => wire::ChatFailedReason::SessionBusy,
        ChatFailedReason::NotInWorld => wire::ChatFailedReason::NotInWorld,
        ChatFailedReason::NotOnline => wire::ChatFailedReason::NotOnline,
        ChatFailedReason::QueueFull => wire::ChatFailedReason::QueueFull,
        ChatFailedReason::RateLimited => wire::ChatFailedReason::RateLimited,
        ChatFailedReason::UnknownBot => wire::ChatFailedReason::UnknownBot,
        ChatFailedReason::AgentBusy => wire::ChatFailedReason::AgentBusy,
        ChatFailedReason::ShuttingDown => wire::ChatFailedReason::ShuttingDown,
    }
}

fn chat_failed_from_wire(number: i32) -> Result<ChatFailedReason, ConvertError> {
    const FIELD: &str = "ChatFailed.reason";
    Ok(match enum_from_wire(FIELD, number)? {
        wire::ChatFailedReason::Disconnected => ChatFailedReason::Disconnected,
        wire::ChatFailedReason::ChatUnavailable => ChatFailedReason::ChatUnavailable,
        wire::ChatFailedReason::TimedOut => ChatFailedReason::TimedOut,
        wire::ChatFailedReason::SessionBusy => ChatFailedReason::SessionBusy,
        wire::ChatFailedReason::NotInWorld => ChatFailedReason::NotInWorld,
        wire::ChatFailedReason::NotOnline => ChatFailedReason::NotOnline,
        wire::ChatFailedReason::QueueFull => ChatFailedReason::QueueFull,
        wire::ChatFailedReason::RateLimited => ChatFailedReason::RateLimited,
        wire::ChatFailedReason::UnknownBot => ChatFailedReason::UnknownBot,
        wire::ChatFailedReason::AgentBusy => ChatFailedReason::AgentBusy,
        wire::ChatFailedReason::ShuttingDown => ChatFailedReason::ShuttingDown,
        wire::ChatFailedReason::Unspecified => {
            return Err(ConvertError::new(FIELD, Problem::Missing));
        }
    })
}
