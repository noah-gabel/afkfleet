//! What the server sends an agent.

use core::time::Duration;

use fleet_core::bot::{BotSpec, StickyState};
use fleet_core::chat::ChatMessage;
use fleet_core::id::{AgentId, BotId, ChatRequestId};
use fleet_core::mc::SessionCredentials;

use super::{SessionRequestId, bots_from_wire, request_number};
use crate::convert::{
    chat_message_from_wire, enum_from_wire, id_from_wire, id_to_wire, required, session_from_wire,
    session_to_wire,
};
use crate::generated as wire;
use crate::{ConvertError, Problem};

/// A message from the server to an agent.
#[derive(Debug, Clone)]
pub enum ServerMessage {
    /// The answer to a hello the server accepts.
    Welcome(Welcome),
    /// Every bot the agent should run, replacing what it runs now. Reading it
    /// refuses more than [`MAX_BOTS`](super::MAX_BOTS) of them, or a bot that
    /// appears twice.
    Reconcile(Vec<Assignment>),
    /// A bot the agent should run.
    Assign(Assignment),
    /// A bot the agent should stop running.
    Unassign(BotId),
    /// A new spec for a bot the agent runs.
    Update(BotSpec),
    /// A user's chat message for a bot to send.
    SendChat(SendChat),
    /// The session credentials a bot asked for.
    SessionGrant(SessionGrant),
    /// The answer to a session request the server can't grant.
    SessionDenied(SessionDenied),
    /// The answer to a hello the server refuses, or the last message of a
    /// stream whose permission ended; the stream then ends.
    Reject(RejectReason),
}

/// The answer to a hello the server accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Welcome {
    /// How often the agent sends a heartbeat: whole milliseconds, from
    /// [`Welcome::MIN_HEARTBEAT_INTERVAL`] to [`Welcome::MAX_HEARTBEAT_INTERVAL`].
    pub heartbeat_interval: Duration,
    /// The agent's ID: the one its certificate names, so both ends log the
    /// same.
    pub agent_id: AgentId,
}

impl Welcome {
    /// The shortest heartbeat interval the wire accepts.
    pub const MIN_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
    /// The longest heartbeat interval the wire accepts.
    pub const MAX_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(300);
}

/// A bot the agent should run.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// What the bot should be doing.
    pub spec: BotSpec,
    /// The paused or failed state the bot starts in, if the server stored one
    /// (ADR-0013).
    pub restore: Option<StickyState>,
}

/// A user's chat message for a bot to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendChat {
    /// The request, which the agent's answer repeats.
    pub request: ChatRequestId,
    /// The bot.
    pub bot_id: BotId,
    /// The message.
    pub message: ChatMessage,
}

/// The session credentials a bot asked for. The access token is a
/// `SecretString`, so its `Debug` never shows it.
#[derive(Debug, Clone)]
pub struct SessionGrant {
    /// The request's number.
    pub id: SessionRequestId,
    /// The bot that asked.
    pub bot_id: BotId,
    /// The credentials.
    pub credentials: SessionCredentials,
}

/// The answer to a session request the server can't grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionDenied {
    /// The request's number.
    pub id: SessionRequestId,
    /// The bot that asked.
    pub bot_id: BotId,
    /// Whether asking again later may help.
    pub retryable: bool,
}

/// Why the server refuses a hello or ends a stream (ADR-0019, S24, S26, S27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// The server doesn't speak the agent's protocol version.
    IncompatibleProtocol,
    /// The agent is disabled.
    AgentDisabled,
    /// The hello's name isn't the agent's.
    NameMismatch,
    /// The first message isn't a hello, or the hello isn't valid.
    InvalidHello,
    /// The agent has a live stream already.
    AlreadyConnected,
    /// The agent's certificate was revoked.
    CertificateRevoked,
    /// The agent's certificate expired.
    CertificateExpired,
}

impl TryFrom<ServerMessage> for wire::ServerMessage {
    type Error = ConvertError;

    fn try_from(message: ServerMessage) -> Result<Self, ConvertError> {
        use wire::server_message::Msg;
        let msg = match message {
            ServerMessage::Welcome(welcome) => Msg::Welcome(wire::Welcome {
                heartbeat_interval_ms: heartbeat_to_wire(welcome.heartbeat_interval)?,
                agent_id: id_to_wire(welcome.agent_id),
            }),
            ServerMessage::Reconcile(assignments) => Msg::Reconcile(wire::ReconcileFull {
                bots: assignments.into_iter().map(Into::into).collect(),
            }),
            ServerMessage::Assign(assignment) => Msg::Assign(assignment.into()),
            ServerMessage::Unassign(bot_id) => Msg::Unassign(wire::UnassignBot {
                bot_id: id_to_wire(bot_id),
            }),
            ServerMessage::Update(spec) => Msg::Update(wire::UpdateBotSpec {
                spec: Some(spec.into()),
            }),
            ServerMessage::SendChat(send) => Msg::SendChat(wire::SendChat {
                request_id: id_to_wire(send.request),
                bot_id: id_to_wire(send.bot_id),
                message: send.message.into(),
            }),
            ServerMessage::SessionGrant(grant) => Msg::SessionGrant(wire::SessionGrant {
                id: grant.id.get().get(),
                bot_id: id_to_wire(grant.bot_id),
                session: Some(session_to_wire(grant.credentials)?),
            }),
            ServerMessage::SessionDenied(denied) => Msg::SessionDenied(wire::SessionDenied {
                id: denied.id.get().get(),
                bot_id: id_to_wire(denied.bot_id),
                retryable: denied.retryable,
            }),
            ServerMessage::Reject(reason) => Msg::Reject(wire::Reject {
                reason: match reason {
                    RejectReason::IncompatibleProtocol => wire::RejectReason::IncompatibleProtocol,
                    RejectReason::AgentDisabled => wire::RejectReason::AgentDisabled,
                    RejectReason::NameMismatch => wire::RejectReason::NameMismatch,
                    RejectReason::InvalidHello => wire::RejectReason::InvalidHello,
                    RejectReason::AlreadyConnected => wire::RejectReason::AlreadyConnected,
                    RejectReason::CertificateRevoked => wire::RejectReason::CertificateRevoked,
                    RejectReason::CertificateExpired => wire::RejectReason::CertificateExpired,
                }
                .into(),
            }),
        };
        Ok(Self { msg: Some(msg) })
    }
}

impl TryFrom<wire::ServerMessage> for ServerMessage {
    type Error = ConvertError;

    fn try_from(message: wire::ServerMessage) -> Result<Self, ConvertError> {
        use wire::server_message::Msg;
        Ok(match required("ServerMessage.msg", message.msg)? {
            Msg::Welcome(welcome) => Self::Welcome(Welcome {
                heartbeat_interval: heartbeat_from_wire(welcome.heartbeat_interval_ms)?,
                agent_id: id_from_wire("Welcome.agent_id", &welcome.agent_id)?,
            }),
            Msg::Reconcile(reconcile) => Self::Reconcile(bots_from_wire(
                "ReconcileFull.bots",
                reconcile.bots,
                |assignment: &Assignment| assignment.spec.id,
            )?),
            Msg::Assign(assign) => Self::Assign(assign.try_into()?),
            Msg::Unassign(unassign) => {
                Self::Unassign(id_from_wire("UnassignBot.bot_id", &unassign.bot_id)?)
            }
            Msg::Update(update) => {
                Self::Update(required("UpdateBotSpec.spec", update.spec)?.try_into()?)
            }
            Msg::SendChat(send) => Self::SendChat(SendChat {
                request: id_from_wire("SendChat.request_id", &send.request_id)?,
                bot_id: id_from_wire("SendChat.bot_id", &send.bot_id)?,
                message: chat_message_from_wire("SendChat.message", &send.message)?,
            }),
            Msg::SessionGrant(grant) => Self::SessionGrant(SessionGrant {
                id: request_number("SessionGrant.id", grant.id)?,
                bot_id: id_from_wire("SessionGrant.bot_id", &grant.bot_id)?,
                credentials: session_from_wire(required("SessionGrant.session", grant.session)?)?,
            }),
            Msg::SessionDenied(denied) => Self::SessionDenied(SessionDenied {
                id: request_number("SessionDenied.id", denied.id)?,
                bot_id: id_from_wire("SessionDenied.bot_id", &denied.bot_id)?,
                retryable: denied.retryable,
            }),
            Msg::Reject(reject) => {
                const FIELD: &str = "Reject.reason";
                Self::Reject(match enum_from_wire(FIELD, reject.reason)? {
                    wire::RejectReason::IncompatibleProtocol => RejectReason::IncompatibleProtocol,
                    wire::RejectReason::AgentDisabled => RejectReason::AgentDisabled,
                    wire::RejectReason::NameMismatch => RejectReason::NameMismatch,
                    wire::RejectReason::InvalidHello => RejectReason::InvalidHello,
                    wire::RejectReason::AlreadyConnected => RejectReason::AlreadyConnected,
                    wire::RejectReason::CertificateRevoked => RejectReason::CertificateRevoked,
                    wire::RejectReason::CertificateExpired => RejectReason::CertificateExpired,
                    wire::RejectReason::Unspecified => {
                        return Err(ConvertError::new(FIELD, Problem::Missing));
                    }
                })
            }
        })
    }
}

impl From<Assignment> for wire::AssignBot {
    fn from(assignment: Assignment) -> Self {
        Self {
            spec: Some(assignment.spec.into()),
            restore: assignment.restore.map(Into::into),
        }
    }
}

impl TryFrom<wire::AssignBot> for Assignment {
    type Error = ConvertError;

    fn try_from(assign: wire::AssignBot) -> Result<Self, ConvertError> {
        Ok(Self {
            spec: required("AssignBot.spec", assign.spec)?.try_into()?,
            restore: assign.restore.map(TryInto::try_into).transpose()?,
        })
    }
}

const HEARTBEAT: &str = "Welcome.heartbeat_interval_ms";

/// Writes a heartbeat interval in whole milliseconds; anything finer is
/// dropped, and an interval outside the wire's range is refused.
fn heartbeat_to_wire(interval: Duration) -> Result<u64, ConvertError> {
    u64::try_from(interval.as_millis())
        .ok()
        .filter(|millis| heartbeat_in_range(*millis))
        .ok_or(ConvertError::new(HEARTBEAT, Problem::OutOfRange))
}

fn heartbeat_from_wire(millis: u64) -> Result<Duration, ConvertError> {
    if heartbeat_in_range(millis) {
        Ok(Duration::from_millis(millis))
    } else {
        Err(ConvertError::new(HEARTBEAT, Problem::OutOfRange))
    }
}

fn heartbeat_in_range(millis: u64) -> bool {
    let millis = Duration::from_millis(millis);
    (Welcome::MIN_HEARTBEAT_INTERVAL..=Welcome::MAX_HEARTBEAT_INTERVAL).contains(&millis)
}
