//! The bots' DTOs (Plan.md P11.2, Appendix B, ADR-0019 S50, S54, S55): a bot
//! as `GET /bots` and `GET /bots/{id}` show it, and the two settings a user
//! changes, its server address and its mode.
//!
//! A bot is 1:1 with its account, and shows it as [`AccountResponse`] does,
//! under the same rule: who owns it, and since when, only for Admins, the
//! Owner and Manage holders. Which agent runs it is shown to Admins and the
//! Owner only, who manage agents. Its [`BotStatus`] is what the server knows
//! when it answers: its agent's latest report, that none has come yet, that
//! its agent is gone, or why it runs nowhere.
//!
//! Every value a later version may extend is an [`Open`] (ADR-0015): the
//! status's kind, the state and its reason, why a bot isn't placed, and how
//! its last session ended. Code that branches on one keeps a default branch.

use fleet_core::bot::{DesiredRunState, FailReason, LastState, PauseReason, UnplacedWhy};
use fleet_core::disconnect::{
    AccountRestriction as CoreAccountRestriction, ConflictKind,
    ConnectFailure as CoreConnectFailure, DisconnectReason, Kick, PermanentKind,
    SessionServerFailure as CoreSessionServerFailure, TranslationKey,
};
use fleet_core::id::{AgentId, BotId, ModeId};
use fleet_core::placement::Unplaced;
use fleet_core::value::{ServerAddress, ServerAddressError};
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::accounts::AccountResponse;
use crate::agents::{AGENT_NAME_MAX_CHARS, AgentStatus};
use crate::ids::id_schema;
use crate::int::integer_schema;
use crate::open::{Open, OpenEnum};
use crate::text::BoundedText;
use crate::time::Timestamp;

/// The characters of a server address: a full-length host, `:` and a port.
pub const SERVER_ADDRESS_MAX_CHARS: usize = ServerAddress::MAX_LEN;

/// The characters of a kick's translation key.
pub const KICK_KEY_MAX_CHARS: usize = TranslationKey::MAX_LEN;

/// The characters of a kick's message, which the Minecraft server wrote.
pub const KICK_MESSAGE_MAX_CHARS: usize = Kick::MAX_MESSAGE_LEN;

/// Whether a bot should run.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    /// It should run: the server places it once it can.
    Running,
    /// It should be stopped.
    Stopped,
}

impl OpenEnum for DesiredState {
    const ALL: &'static [Self] = &[Self::Running, Self::Stopped];

    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }
}

impl From<DesiredRunState> for DesiredState {
    fn from(desired: DesiredRunState) -> Self {
        match desired {
            DesiredRunState::Running => Self::Running,
            DesiredRunState::Stopped => Self::Stopped,
        }
    }
}

/// What a bot's status rests on.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotStatusKind {
    /// Its agent is online and reported it.
    Reported,
    /// Its agent is online and hasn't reported it yet.
    AwaitingReport,
    /// Its agent has no live stream: `state` is the last one known.
    Unknown,
    /// It runs nowhere: `why` says why.
    Unplaced,
}

impl OpenEnum for BotStatusKind {
    const ALL: &'static [Self] = &[
        Self::Reported,
        Self::AwaitingReport,
        Self::Unknown,
        Self::Unplaced,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::AwaitingReport => "awaiting_report",
            Self::Unknown => "unknown",
            Self::Unplaced => "unplaced",
        }
    }
}

/// A bot's lifecycle state (Plan.md Appendix E).
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotStateKind {
    /// Not running.
    Stopped,
    /// Waiting for its session.
    AwaitingSession,
    /// Joining its server.
    Connecting,
    /// On its server.
    Online,
    /// Waiting before it tries again.
    Backoff,
    /// Someone else is playing on its account: resume it when they're done.
    Paused,
    /// It can't go on without help: `reason` says why; reset it to try again.
    Failed,
    /// Leaving its server.
    Stopping,
}

impl OpenEnum for BotStateKind {
    const ALL: &'static [Self] = &[
        Self::Stopped,
        Self::AwaitingSession,
        Self::Connecting,
        Self::Online,
        Self::Backoff,
        Self::Paused,
        Self::Failed,
        Self::Stopping,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::AwaitingSession => "awaiting_session",
            Self::Connecting => "connecting",
            Self::Online => "online",
            Self::Backoff => "backoff",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Stopping => "stopping",
        }
    }
}

impl From<LastState> for BotStateKind {
    fn from(state: LastState) -> Self {
        match state {
            LastState::Stopped => Self::Stopped,
            LastState::AwaitingSession => Self::AwaitingSession,
            LastState::Connecting => Self::Connecting,
            LastState::Online => Self::Online,
            LastState::Backoff => Self::Backoff,
            LastState::Paused(_) => Self::Paused,
            LastState::Failed(_) => Self::Failed,
            LastState::Stopping => Self::Stopping,
        }
    }
}

/// Why a bot is paused or failed.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotStateReason {
    /// Paused: the account logged in from somewhere else.
    DuplicateLogin,
    /// Failed: the server banned the account or its address.
    Banned,
    /// Failed: the account isn't on the server's whitelist.
    NotWhitelisted,
    /// Failed: the server runs another Minecraft version.
    WrongVersion,
    /// Failed: Mojang banned the account from multiplayer.
    AccountBanned,
    /// Failed: the account's settings don't allow multiplayer.
    MultiplayerDisabled,
    /// Failed: the server rejected the session twice; sign the account in again.
    Auth,
    /// Failed: no session could be issued for the account.
    SessionDenied,
    /// Failed: it crashed too often.
    CrashLoop,
}

impl OpenEnum for BotStateReason {
    const ALL: &'static [Self] = &[
        Self::DuplicateLogin,
        Self::Banned,
        Self::NotWhitelisted,
        Self::WrongVersion,
        Self::AccountBanned,
        Self::MultiplayerDisabled,
        Self::Auth,
        Self::SessionDenied,
        Self::CrashLoop,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateLogin => "duplicate_login",
            Self::Banned => "banned",
            Self::NotWhitelisted => "not_whitelisted",
            Self::WrongVersion => "wrong_version",
            Self::AccountBanned => "account_banned",
            Self::MultiplayerDisabled => "multiplayer_disabled",
            Self::Auth => "auth",
            Self::SessionDenied => "session_denied",
            Self::CrashLoop => "crash_loop",
        }
    }
}

impl BotStateReason {
    /// The reason of a paused or failed `state`; none for every other state.
    #[must_use]
    pub const fn of(state: LastState) -> Option<Self> {
        Some(match state {
            LastState::Paused(PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin,
            }) => Self::DuplicateLogin,
            LastState::Failed(FailReason::Permanent { kind }) => match kind {
                PermanentKind::Banned => Self::Banned,
                PermanentKind::NotWhitelisted => Self::NotWhitelisted,
                PermanentKind::WrongVersion => Self::WrongVersion,
                PermanentKind::AccountBanned => Self::AccountBanned,
                PermanentKind::MultiplayerDisabled => Self::MultiplayerDisabled,
            },
            LastState::Failed(FailReason::Auth) => Self::Auth,
            LastState::Failed(FailReason::SessionDenied) => Self::SessionDenied,
            LastState::Failed(FailReason::CrashLoop) => Self::CrashLoop,
            LastState::Stopped
            | LastState::AwaitingSession
            | LastState::Connecting
            | LastState::Online
            | LastState::Backoff
            | LastState::Stopping => return None,
        })
    }
}

/// Why a bot runs nowhere.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnplacedReason {
    /// It has no server address.
    NoServer,
    /// It should be stopped.
    Stopped,
    /// Its owner is disabled.
    OwnerDisabled,
    /// Its account waits for approval.
    Pending,
    /// Its account must be signed in again.
    ReauthRequired,
    /// Its account was refused.
    Refused,
    /// It's an offline account, and the server doesn't allow those now.
    OfflineNotAllowed,
    /// It may run, and waits for an agent with room.
    AwaitingAgent,
}

impl OpenEnum for UnplacedReason {
    const ALL: &'static [Self] = &[
        Self::NoServer,
        Self::Stopped,
        Self::OwnerDisabled,
        Self::Pending,
        Self::ReauthRequired,
        Self::Refused,
        Self::OfflineNotAllowed,
        Self::AwaitingAgent,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::NoServer => "no_server",
            Self::Stopped => "stopped",
            Self::OwnerDisabled => "owner_disabled",
            Self::Pending => "pending",
            Self::ReauthRequired => "reauth_required",
            Self::Refused => "refused",
            Self::OfflineNotAllowed => "offline_not_allowed",
            Self::AwaitingAgent => "awaiting_agent",
        }
    }
}

impl From<UnplacedWhy> for UnplacedReason {
    fn from(why: UnplacedWhy) -> Self {
        match why {
            UnplacedWhy::Unqualified(why) => match why {
                Unplaced::NoServer => Self::NoServer,
                Unplaced::Stopped => Self::Stopped,
                Unplaced::OwnerDisabled => Self::OwnerDisabled,
                Unplaced::Pending => Self::Pending,
                Unplaced::ReauthRequired => Self::ReauthRequired,
                Unplaced::Refused => Self::Refused,
                Unplaced::OfflineNotAllowed => Self::OfflineNotAllowed,
            },
            UnplacedWhy::AwaitingAgent => Self::AwaitingAgent,
        }
    }
}

/// How a bot's last session ended.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectKind {
    /// The Minecraft server kicked it: `key` and `message` say how.
    Kicked,
    /// The connection closed without a reason.
    ConnectionClosed,
    /// It couldn't connect: `connect_failure` says why.
    ConnectFailed,
    /// The Minecraft server rejected its session.
    AuthRejected,
    /// Mojang refuses the account: `restriction` says how.
    AccountRestricted,
    /// Mojang's session server couldn't confirm the join:
    /// `session_server_failure` says why.
    SessionServerFailed,
    /// Its session crashed.
    SessionCrashed,
    /// Its session stopped ticking.
    WatchdogTimeout,
    /// The Minecraft server went quiet.
    LivenessTimeout,
    /// It couldn't respawn.
    RespawnFailed,
}

impl OpenEnum for DisconnectKind {
    const ALL: &'static [Self] = &[
        Self::Kicked,
        Self::ConnectionClosed,
        Self::ConnectFailed,
        Self::AuthRejected,
        Self::AccountRestricted,
        Self::SessionServerFailed,
        Self::SessionCrashed,
        Self::WatchdogTimeout,
        Self::LivenessTimeout,
        Self::RespawnFailed,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Kicked => "kicked",
            Self::ConnectionClosed => "connection_closed",
            Self::ConnectFailed => "connect_failed",
            Self::AuthRejected => "auth_rejected",
            Self::AccountRestricted => "account_restricted",
            Self::SessionServerFailed => "session_server_failed",
            Self::SessionCrashed => "session_crashed",
            Self::WatchdogTimeout => "watchdog_timeout",
            Self::LivenessTimeout => "liveness_timeout",
            Self::RespawnFailed => "respawn_failed",
        }
    }
}

/// Why a bot couldn't connect.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectFailure {
    /// The Minecraft server refused the connection.
    Refused,
    /// Connecting took too long.
    TimedOut,
    /// The server's name couldn't be resolved.
    Unresolvable,
    /// The agent couldn't start the session.
    HostUnavailable,
    /// Another network error.
    Other,
}

impl OpenEnum for ConnectFailure {
    const ALL: &'static [Self] = &[
        Self::Refused,
        Self::TimedOut,
        Self::Unresolvable,
        Self::HostUnavailable,
        Self::Other,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::TimedOut => "timed_out",
            Self::Unresolvable => "unresolvable",
            Self::HostUnavailable => "host_unavailable",
            Self::Other => "other",
        }
    }
}

impl From<CoreConnectFailure> for ConnectFailure {
    fn from(failure: CoreConnectFailure) -> Self {
        match failure {
            CoreConnectFailure::Refused => Self::Refused,
            CoreConnectFailure::TimedOut => Self::TimedOut,
            CoreConnectFailure::Unresolvable => Self::Unresolvable,
            CoreConnectFailure::HostUnavailable => Self::HostUnavailable,
            CoreConnectFailure::Other => Self::Other,
        }
    }
}

/// How Mojang refuses an account.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountRestriction {
    /// Banned from multiplayer.
    Banned,
    /// Its settings don't allow multiplayer.
    MultiplayerDisabled,
}

impl OpenEnum for AccountRestriction {
    const ALL: &'static [Self] = &[Self::Banned, Self::MultiplayerDisabled];

    fn as_str(self) -> &'static str {
        match self {
            Self::Banned => "banned",
            Self::MultiplayerDisabled => "multiplayer_disabled",
        }
    }
}

impl From<CoreAccountRestriction> for AccountRestriction {
    fn from(restriction: CoreAccountRestriction) -> Self {
        match restriction {
            CoreAccountRestriction::Banned => Self::Banned,
            CoreAccountRestriction::MultiplayerDisabled => Self::MultiplayerDisabled,
        }
    }
}

/// Why Mojang's session server couldn't confirm a join.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionServerFailure {
    /// It couldn't be reached.
    Unreachable,
    /// It limited the requests.
    RateLimited,
    /// It took too long.
    TimedOut,
    /// It answered something unexpected.
    Unexpected,
}

impl OpenEnum for SessionServerFailure {
    const ALL: &'static [Self] = &[
        Self::Unreachable,
        Self::RateLimited,
        Self::TimedOut,
        Self::Unexpected,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::RateLimited => "rate_limited",
            Self::TimedOut => "timed_out",
            Self::Unexpected => "unexpected",
        }
    }
}

impl From<CoreSessionServerFailure> for SessionServerFailure {
    fn from(failure: CoreSessionServerFailure) -> Self {
        match failure {
            CoreSessionServerFailure::Unreachable => Self::Unreachable,
            CoreSessionServerFailure::RateLimited => Self::RateLimited,
            CoreSessionServerFailure::TimedOut => Self::TimedOut,
            CoreSessionServerFailure::Unexpected => Self::Unexpected,
        }
    }
}

/// How a bot's last session ended. A kick's key and message come from the
/// Minecraft server: cleaned and capped, and shown as plain text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct LastDisconnect {
    /// How.
    #[ts(as = "DisconnectKind")]
    #[schema(value_type = DisconnectKind)]
    pub kind: Open<DisconnectKind>,
    /// `kicked`: the translation key, when the server sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<KICK_KEY_MAX_CHARS>::schema, nullable = false)]
    pub key: Option<BoundedText<KICK_KEY_MAX_CHARS>>,
    /// `kicked`: the message, as the server wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<KICK_MESSAGE_MAX_CHARS>::schema, nullable = false)]
    pub message: Option<BoundedText<KICK_MESSAGE_MAX_CHARS>>,
    /// `connect_failed`: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<ConnectFailure>", optional)]
    #[schema(value_type = Option<ConnectFailure>, nullable = false)]
    pub connect_failure: Option<Open<ConnectFailure>>,
    /// `account_restricted`: how.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<AccountRestriction>", optional)]
    #[schema(value_type = Option<AccountRestriction>, nullable = false)]
    pub restriction: Option<Open<AccountRestriction>>,
    /// `session_server_failed`: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<SessionServerFailure>", optional)]
    #[schema(value_type = Option<SessionServerFailure>, nullable = false)]
    pub session_server_failure: Option<Open<SessionServerFailure>>,
}

impl From<&DisconnectReason> for LastDisconnect {
    fn from(reason: &DisconnectReason) -> Self {
        let bare = |kind: DisconnectKind| Self {
            kind: kind.into(),
            key: None,
            message: None,
            connect_failure: None,
            restriction: None,
            session_server_failure: None,
        };
        match reason {
            DisconnectReason::Kicked(kick) => Self {
                key: kick.key().map(|key| BoundedText::new(key.as_str())),
                message: Some(BoundedText::new(kick.message())),
                ..bare(DisconnectKind::Kicked)
            },
            DisconnectReason::ConnectionClosed => bare(DisconnectKind::ConnectionClosed),
            DisconnectReason::ConnectFailed { failure } => Self {
                connect_failure: Some(ConnectFailure::from(*failure).into()),
                ..bare(DisconnectKind::ConnectFailed)
            },
            DisconnectReason::AuthRejected => bare(DisconnectKind::AuthRejected),
            DisconnectReason::AccountRestricted { restriction } => Self {
                restriction: Some(AccountRestriction::from(*restriction).into()),
                ..bare(DisconnectKind::AccountRestricted)
            },
            DisconnectReason::SessionServerFailed { failure } => Self {
                session_server_failure: Some(SessionServerFailure::from(*failure).into()),
                ..bare(DisconnectKind::SessionServerFailed)
            },
            DisconnectReason::SessionCrashed => bare(DisconnectKind::SessionCrashed),
            DisconnectReason::WatchdogTimeout => bare(DisconnectKind::WatchdogTimeout),
            DisconnectReason::LivenessTimeout => bare(DisconnectKind::LivenessTimeout),
            DisconnectReason::RespawnFailed => bare(DisconnectKind::RespawnFailed),
        }
    }
}

/// What the server knows of a bot's state when it answers (ADR-0019, S41).
/// `kind` says which of the other fields are there:
/// - `reported`: `state` (with `reason` when paused or failed) and `since`,
///   when it entered it; `online_since` while online, `attempt` while it
///   tries, `restarting` while stopping, and `last_disconnect`.
/// - `awaiting_report`: `since`, when the wait began.
/// - `unknown`: `agent_last_seen_at`, and the last known `state` (with
///   `reason`) and `since`, when the server received it.
/// - `unplaced`: `why`, and `state`: `paused` or `failed` (with `reason`)
///   when it rests there, otherwise `stopped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotStatus {
    /// What the status rests on.
    #[ts(as = "BotStatusKind")]
    #[schema(value_type = BotStatusKind)]
    pub kind: Open<BotStatusKind>,
    /// The state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<BotStateKind>", optional)]
    #[schema(value_type = Option<BotStateKind>, nullable = false)]
    pub state: Option<Open<BotStateKind>>,
    /// Why it's paused or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<BotStateReason>", optional)]
    #[schema(value_type = Option<BotStateReason>, nullable = false)]
    pub reason: Option<Open<BotStateReason>>,
    /// Since when, as `kind` says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub since: Option<Timestamp>,
    /// When it went online, while it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub online_since: Option<Timestamp>,
    /// Which attempt of its run this is, from 1, while it tries or is online.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<1, { u32::MAX }>, nullable = false)]
    pub attempt: Option<u32>,
    /// While stopping: whether it starts again once it has left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub restarting: Option<bool>,
    /// How its last session ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub last_disconnect: Option<LastDisconnect>,
    /// `unplaced`: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<UnplacedReason>", optional)]
    #[schema(value_type = Option<UnplacedReason>, nullable = false)]
    pub why: Option<Open<UnplacedReason>>,
    /// `unknown`: when the server last heard from its agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub agent_last_seen_at: Option<Timestamp>,
}

impl BotStatus {
    /// A status of `kind` with none of the other fields yet.
    #[must_use]
    pub fn of(kind: BotStatusKind) -> Self {
        Self {
            kind: kind.into(),
            state: None,
            reason: None,
            since: None,
            online_since: None,
            attempt: None,
            restarting: None,
            last_disconnect: None,
            why: None,
            agent_last_seen_at: None,
        }
    }
}

/// The agent a bot is assigned to: for Admins and the Owner only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotAgent {
    /// Its ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: AgentId,
    /// Its name.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<AGENT_NAME_MAX_CHARS>::schema)]
    pub name: BoundedText<AGENT_NAME_MAX_CHARS>,
    /// Its status.
    #[ts(as = "AgentStatus")]
    #[schema(value_type = AgentStatus)]
    pub status: Open<AgentStatus>,
}

/// A bot as `GET /bots` and `GET /bots/{id}` show it to the caller, and as
/// every bot write answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotResponse {
    /// The bot's ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: BotId,
    /// Its account, as `GET /accounts/{id}` shows it to the caller: `level`
    /// says what they may do with the bot.
    pub account: AccountResponse,
    /// The Minecraft server it joins, once one is set: for callers with
    /// Control or higher on its account only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<SERVER_ADDRESS_MAX_CHARS>::schema, nullable = false)]
    pub server_address: Option<BoundedText<SERVER_ADDRESS_MAX_CHARS>>,
    /// Its mode's ID; `GET /modes` names the modes the caller may see.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub mode_id: ModeId,
    /// Whether it should run.
    #[ts(as = "DesiredState")]
    #[schema(value_type = DesiredState)]
    pub desired_state: Open<DesiredState>,
    /// What the server knows of its state.
    pub status: BotStatus,
    /// The agent it's assigned to: for Admins and the Owner only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub agent: Option<BotAgent>,
}

/// `GET /bots`: one page of the bots the caller may see, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotPage {
    /// The bots.
    pub items: Vec<BotResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub next: Option<BotId>,
}

/// `PATCH /bots/{id}`: the Minecraft server the bot joins. Needs Manage on
/// the bot and a step-up. Where the address points isn't checked here:
/// that's the agent's, on the address it resolves before it connects
/// (ADR-0019, S46).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BotPatchRequest {
    /// The server: a host name or an IP address, and an optional port
    /// (`mc.example.com`, `192.0.2.10:25566`, `[2001:db8::1]:25565`).
    #[garde(custom(valid_server_address))]
    #[schema(schema_with = BoundedText::<SERVER_ADDRESS_MAX_CHARS>::schema)]
    pub server_address: String,
}

/// The garde rule of a server address: a valid `ServerAddress`. The message
/// names the rule, never the text.
fn valid_server_address<C>(address: &str, _context: &C) -> garde::Result {
    ServerAddress::try_from(address)
        .map(drop)
        .map_err(|error| garde::Error::new(server_address_message(error)))
}

/// The field message of each way an address can be invalid.
const fn server_address_message(error: ServerAddressError) -> &'static str {
    match error {
        ServerAddressError::Empty => "must not be empty",
        ServerAddressError::TooLong { .. } => {
            "must be at most 259 characters, with a host of at most 253"
        }
        ServerAddressError::Scheme | ServerAddressError::Path | ServerAddressError::UserInfo => {
            "must be only a host and an optional port"
        }
        ServerAddressError::InvalidHost => "must have a domain name or an IP address as its host",
        ServerAddressError::InvalidPort => "must have a port from 1 to 65535",
    }
}

/// `PUT /bots/{id}/mode`: the mode the bot runs. Needs Control on the bot,
/// or Manage when the mode sends a command off the allowlist, and the mode
/// must be one the caller may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BotModeRequest {
    /// The mode's ID.
    #[garde(skip)]
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub mode_id: ModeId,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const BOT: &str = "018bcfe5-6800-7101-8101-010101010102";
    const MODE: &str = "01a14202-2800-7000-8000-000000000001";

    #[test]
    fn the_limits_are_the_core_types_lengths() {
        assert_eq!(SERVER_ADDRESS_MAX_CHARS, 259);
        assert_eq!(KICK_KEY_MAX_CHARS, 128);
        assert_eq!(KICK_MESSAGE_MAX_CHARS, 1024);
    }

    #[rstest]
    #[case(DesiredRunState::Running, DesiredState::Running)]
    #[case(DesiredRunState::Stopped, DesiredState::Stopped)]
    fn every_desired_state_has_its_core_name(
        #[case] core: DesiredRunState,
        #[case] expected: DesiredState,
    ) {
        assert_eq!(DesiredState::from(core), expected);
        assert_eq!(expected.as_str(), core.as_str());
    }

    /// Each state reads as the name the server stores it under, and only a
    /// paused or failed one has a reason, under its stored name too.
    #[test]
    fn every_state_and_reason_has_its_stored_name() {
        for state in LastState::ALL {
            assert_eq!(
                BotStateKind::from(state).as_str(),
                state.as_str(),
                "{state:?}"
            );
            assert_eq!(
                BotStateReason::of(state).map(BotStateReason::as_str),
                state.reason(),
                "{state:?}"
            );
        }
    }

    #[test]
    fn every_reason_and_state_is_reached() {
        let kinds: Vec<BotStateKind> = LastState::ALL.into_iter().map(BotStateKind::from).collect();
        let reasons: Vec<BotStateReason> = LastState::ALL
            .into_iter()
            .filter_map(BotStateReason::of)
            .collect();

        for kind in BotStateKind::ALL {
            assert!(kinds.contains(kind), "{kind:?}");
        }
        for reason in BotStateReason::ALL {
            assert!(reasons.contains(reason), "{reason:?}");
        }
    }

    #[test]
    fn every_unplaced_reason_has_its_core_name() {
        for why in Unplaced::ALL {
            assert_eq!(
                UnplacedReason::from(UnplacedWhy::Unqualified(why)).as_str(),
                why.as_str(),
                "{why:?}"
            );
        }
        assert_eq!(
            UnplacedReason::from(UnplacedWhy::AwaitingAgent),
            UnplacedReason::AwaitingAgent
        );
    }

    #[rstest]
    #[case(CoreConnectFailure::Refused, "refused")]
    #[case(CoreConnectFailure::TimedOut, "timed_out")]
    #[case(CoreConnectFailure::Unresolvable, "unresolvable")]
    #[case(CoreConnectFailure::HostUnavailable, "host_unavailable")]
    #[case(CoreConnectFailure::Other, "other")]
    fn every_connect_failure_has_its_name(#[case] failure: CoreConnectFailure, #[case] name: &str) {
        let reason = DisconnectReason::ConnectFailed { failure };

        let shown = LastDisconnect::from(&reason);

        assert_eq!(shown.kind.as_str(), "connect_failed");
        assert_eq!(shown.connect_failure.as_ref().map(Open::as_str), Some(name));
    }

    #[rstest]
    #[case(CoreAccountRestriction::Banned, "banned")]
    #[case(CoreAccountRestriction::MultiplayerDisabled, "multiplayer_disabled")]
    fn every_restriction_has_its_name(
        #[case] restriction: CoreAccountRestriction,
        #[case] name: &str,
    ) {
        let reason = DisconnectReason::AccountRestricted { restriction };

        let shown = LastDisconnect::from(&reason);

        assert_eq!(shown.kind.as_str(), "account_restricted");
        assert_eq!(shown.restriction.as_ref().map(Open::as_str), Some(name));
    }

    #[rstest]
    #[case(CoreSessionServerFailure::Unreachable, "unreachable")]
    #[case(CoreSessionServerFailure::RateLimited, "rate_limited")]
    #[case(CoreSessionServerFailure::TimedOut, "timed_out")]
    #[case(CoreSessionServerFailure::Unexpected, "unexpected")]
    fn every_session_server_failure_has_its_name(
        #[case] failure: CoreSessionServerFailure,
        #[case] name: &str,
    ) {
        let reason = DisconnectReason::SessionServerFailed { failure };

        let shown = LastDisconnect::from(&reason);

        assert_eq!(shown.kind.as_str(), "session_server_failed");
        assert_eq!(
            shown.session_server_failure.as_ref().map(Open::as_str),
            Some(name)
        );
    }

    #[rstest]
    #[case(DisconnectReason::ConnectionClosed, "connection_closed")]
    #[case(DisconnectReason::AuthRejected, "auth_rejected")]
    #[case(DisconnectReason::SessionCrashed, "session_crashed")]
    #[case(DisconnectReason::WatchdogTimeout, "watchdog_timeout")]
    #[case(DisconnectReason::LivenessTimeout, "liveness_timeout")]
    #[case(DisconnectReason::RespawnFailed, "respawn_failed")]
    fn a_bare_end_has_only_its_kind(#[case] reason: DisconnectReason, #[case] name: &str) {
        let shown = LastDisconnect::from(&reason);

        assert_eq!(
            serde_json::to_value(shown).unwrap(),
            json!({ "kind": name })
        );
    }

    #[test]
    fn a_kick_shows_its_key_and_its_cleaned_message() {
        let reason = DisconnectReason::kicked(
            Some("multiplayer.disconnect.banned"),
            "You are banned\u{0007} from this server",
        );

        let shown = LastDisconnect::from(&reason);

        assert_eq!(
            serde_json::to_value(shown).unwrap(),
            json!({
                "kind": "kicked",
                "key": "multiplayer.disconnect.banned",
                "message": "You are banned from this server"
            })
        );
    }

    #[test]
    fn a_kick_without_a_key_shows_only_its_message() {
        let reason = DisconnectReason::kicked(None, "Server closed");

        let shown = LastDisconnect::from(&reason);

        assert_eq!(
            serde_json::to_value(shown).unwrap(),
            json!({ "kind": "kicked", "message": "Server closed" })
        );
    }

    fn bot_json() -> serde_json::Value {
        json!({
            "id": BOT,
            "account": {
                "id": "018bcfe5-6800-7101-8101-010101010101",
                "name": "AfkBot1",
                "kind": "microsoft",
                "status": "active",
                "level": "manage",
                "owner": { "id": "018bcfe5-6800-7202-8202-020202020202", "username": "alice" },
                "linked_at": "2027-01-15T08:00:00.000Z"
            },
            "server_address": "mc.example.com",
            "mode_id": MODE,
            "desired_state": "running",
            "status": {
                "kind": "reported",
                "state": "online",
                "since": "2027-01-16T09:00:05.120Z",
                "online_since": "2027-01-16T09:00:05.120Z",
                "attempt": 1,
                "last_disconnect": { "kind": "kicked", "message": "Server closed" }
            },
            "agent": {
                "id": "018bcfe5-6800-7303-8303-030303030303",
                "name": "agent-1",
                "status": "online"
            }
        })
    }

    #[test]
    fn a_bot_round_trips_with_every_field_and_without_the_optional_ones() {
        let full = bot_json();
        let bare = json!({
            "id": BOT,
            "account": {
                "id": "018bcfe5-6800-7101-8101-010101010101",
                "name": "AfkBot1",
                "kind": "offline",
                "status": "active",
                "level": "view"
            },
            "mode_id": MODE,
            "desired_state": "stopped",
            "status": { "kind": "unplaced", "state": "stopped", "why": "no_server" }
        });

        for json in [full, bare] {
            let back: BotResponse = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(serde_json::to_value(back).unwrap(), json);
        }
    }

    #[test]
    fn a_status_of_a_kind_has_nothing_else() {
        assert_eq!(
            serde_json::to_value(BotStatus::of(BotStatusKind::AwaitingReport)).unwrap(),
            json!({ "kind": "awaiting_report" })
        );
    }

    fn patch_problems(address: &str) -> Vec<(String, String)> {
        let request = BotPatchRequest {
            server_address: address.to_owned(),
        };
        request.validate().map_or_else(
            |report| {
                report
                    .iter()
                    .map(|(path, error)| (path.to_string(), error.message().to_owned()))
                    .collect()
            },
            |()| Vec::new(),
        )
    }

    #[rstest]
    #[case::domain("mc.example.com")]
    #[case::with_port("mc.example.com:25566")]
    #[case::ipv4("192.0.2.10")]
    #[case::ipv6("[2001:db8::1]:25565")]
    #[case::localhost("localhost")]
    fn a_patch_with_a_valid_address_is_valid(#[case] address: &str) {
        assert_eq!(patch_problems(address), Vec::<(String, String)>::new());
    }

    #[rstest]
    #[case::empty("", "must not be empty")]
    #[case::too_long(&"a".repeat(300), "must be at most 259 characters, with a host of at most 253")]
    #[case::scheme("tcp://mc.example.com", "must be only a host and an optional port")]
    #[case::path("mc.example.com/play", "must be only a host and an optional port")]
    #[case::user_info("alice@mc.example.com", "must be only a host and an optional port")]
    #[case::host(
        "mc_example.com",
        "must have a domain name or an IP address as its host"
    )]
    #[case::port("mc.example.com:0", "must have a port from 1 to 65535")]
    fn a_patch_with_an_invalid_address_names_the_rule_on_its_field(
        #[case] address: &str,
        #[case] message: &str,
    ) {
        assert_eq!(
            patch_problems(address),
            [("server_address".to_owned(), message.to_owned())]
        );
    }

    #[test]
    fn a_patch_never_echoes_its_address() {
        let address = "secret_label.example.com";

        let problems = patch_problems(address);

        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems
                .iter()
                .all(|(_, message)| !message.contains("secret_label"))
        );
    }

    #[test]
    fn the_requests_refuse_unknown_fields() {
        let patch = serde_json::from_value::<BotPatchRequest>(
            json!({ "server_address": "mc.example.com", "auto_start": true }),
        );
        let mode = serde_json::from_value::<BotModeRequest>(
            json!({ "mode_id": MODE, "server_address": "x" }),
        );

        assert!(patch.is_err());
        assert!(mode.is_err());
    }

    #[test]
    fn a_mode_request_reads_only_a_version_7_id() {
        let v4 = serde_json::from_value::<BotModeRequest>(
            json!({ "mode_id": "550e8400-e29b-41d4-a716-446655440000" }),
        );
        let v7: BotModeRequest = serde_json::from_value(json!({ "mode_id": MODE })).unwrap();

        assert!(v4.is_err());
        assert_eq!(v7.mode_id.to_string(), MODE);
    }
}
