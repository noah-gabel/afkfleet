//! What the app asks of the agents' live streams (Plan.md P10.5, P10.6,
//! P10.10, P11.2; ADR-0019, S27, S31, S41, S48, S57, R22): to end one whose
//! permission ended, what the server knows of one that lives and of a bot it
//! holds, a bot's latest report kept in memory, a user's lifecycle command
//! for the stream that holds a bot, and the answer to a session request for
//! the stream that asked.
//!
//! The gRPC port's `AgentRegistry` implements it, so the app reaches the
//! streams without knowing the port; a service calls [`LiveAgents::end`]
//! after its own commit, so a stream it misses finds the commit at its
//! hello's transaction.

use core::fmt;
use core::num::NonZeroU64;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotSnapshot, LastReport, LastState, Lifecycle};
use fleet_core::id::{AgentId, BotId};
use fleet_core::mc::SessionCredentials;

/// Why an agent's permission to stream ended (S27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionEnd {
    /// The agent was disabled.
    Disabled,
    /// The certificate it connected with was revoked.
    CertificateRevoked,
    /// The certificate it connected with expired.
    CertificateExpired,
}

/// An agent's live stream, as the server knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveStream {
    /// When its hello was accepted, by the server's clock.
    pub connected_at: DateTime<Utc>,
    /// When the server last heard from the agent on it, by its clock.
    pub last_seen: DateTime<Utc>,
}

/// A bot an agent's live stream holds assigned, as the server knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldBot {
    /// Since when the stream holds it, by the server's clock.
    pub since: DateTime<Utc>,
    /// Its latest report on this stream, if any.
    pub report: Option<BotSnapshot>,
    /// The stream's generation: a command goes only to this stream (S48).
    pub generation: u64,
}

/// A bot's latest report from a stream that held it or listed it, which the
/// server keeps in memory and writes only in transactions it commits anyway
/// (ADR-0019, S40).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestReport {
    /// The stream's agent.
    pub agent: AgentId,
    /// The stream's generation.
    pub generation: u64,
    /// What it reported.
    pub snapshot: BotSnapshot,
    /// When the server received it, by its own clock.
    pub at: DateTime<Utc>,
}

impl LatestReport {
    /// The report as `bots.last_state` stores it.
    #[must_use]
    pub fn last(&self) -> LastReport {
        LastReport {
            state: LastState::from(self.snapshot.state),
            at: self.at,
        }
    }
}

/// Why a lifecycle command didn't go to a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandRefused {
    /// No live stream of the agent holds the bot assigned.
    #[error("no live stream holds the bot")]
    NotHeld,
    /// The stream ended meanwhile.
    #[error("the stream ended")]
    NotConnected,
    /// The stream's queue was full, which ends the stream: the agent
    /// connects again and resyncs.
    #[error("the stream's queue is full")]
    QueueFull,
    /// The message couldn't be written, which a command never is.
    #[error("the command can't be written")]
    Unsendable,
}

/// The answer to a session request, for the stream that asked (P10.6;
/// ADR-0019, S3, S57).
#[derive(Debug, Clone)]
pub struct SessionAnswer {
    /// The agent's own number of the request it answers.
    pub number: NonZeroU64,
    /// The bot the request asked for.
    pub bot: BotId,
    /// A grant or a denial.
    pub outcome: SessionOutcome,
}

/// What a session request gets.
#[derive(Debug, Clone)]
pub enum SessionOutcome {
    /// The bot's session: an online account's token, or an offline
    /// account's name.
    Grant(SessionCredentials),
    /// No session; `retryable` tells the agent whether asking again later
    /// may help, and a refusal that isn't fails the bot for good.
    Denied {
        /// Whether asking again later may help.
        retryable: bool,
    },
}

/// Why a session request's answer didn't go to its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnswerRefused {
    /// A grant's stream doesn't hold the bot assigned any more.
    #[error("the stream doesn't hold the bot")]
    NotHeld,
    /// The stream ended, or another one replaced it.
    #[error("the stream ended")]
    NotConnected,
    /// The stream's queue was full, which ends the stream.
    #[error("the stream's queue is full")]
    QueueFull,
    /// The answer couldn't be written, or is larger than an agent reads.
    #[error("the answer can't be written")]
    Unsendable,
}

/// The agents' live streams; see the [module docs](self).
pub trait LiveAgents: Send + Sync + fmt::Debug {
    /// Ends `agent`'s stream for `reason`, its last message the matching
    /// `Reject`, and nothing queued goes out after this call; or, while its
    /// hello is being accepted, makes sure it's never welcomed. Whether
    /// there was one.
    fn end(&self, agent: AgentId, reason: PermissionEnd) -> bool;

    /// `agent`'s live stream, if it has one.
    fn live(&self, agent: AgentId) -> Option<LiveStream>;

    /// `bot`, if `agent`'s live stream holds it assigned: since when, and its
    /// latest report on that stream (never an earlier stream's).
    fn held(&self, agent: AgentId, bot: BotId) -> Option<HeldBot>;

    /// `bot`'s latest report kept in memory, from whichever stream sent it:
    /// with the stored state, what the reconciler restores the bot from.
    fn latest(&self, bot: BotId) -> Option<LatestReport>;

    /// Takes `bot`'s latest report out of memory: a Resume or a Reset that
    /// clears the bot's stored state does, inside its transaction, so no
    /// placement after its commit restores what it cleared (S48).
    fn take_latest(&self, bot: BotId) -> Option<LatestReport>;

    /// Puts `report` back as its bot's latest, unless a report came in
    /// meanwhile: the clear that took it didn't commit.
    fn put_back_latest(&self, report: LatestReport);

    /// Sends `command` for `bot` through generation `generation` of
    /// `agent`'s live stream, the one found holding it: checked and queued
    /// at once, and only while that stream lives, isn't ended and holds the
    /// bot assigned; never queued for later, so a command dies with its
    /// stream (S48, S49).
    ///
    /// # Errors
    /// [`CommandRefused`]: `NotConnected` for another, an ended or no
    /// stream; `NotHeld` when that stream doesn't hold the bot assigned.
    fn command(
        &self,
        agent: AgentId,
        generation: u64,
        bot: BotId,
        command: Lifecycle,
    ) -> Result<(), CommandRefused>;

    /// Sends `answer` through generation `generation` of `agent`'s live
    /// stream, the one that asked: checked and queued at once, only while
    /// that stream lives and isn't ended, and a grant only while it holds
    /// the bot assigned; never through another stream of the agent, whose
    /// request numbers start again at 1 (S57).
    ///
    /// # Errors
    /// [`AnswerRefused`]: `NotConnected` for another, an ended or no stream;
    /// `NotHeld` when a grant's stream doesn't hold the bot assigned.
    fn answer_session(
        &self,
        agent: AgentId,
        generation: u64,
        answer: SessionAnswer,
    ) -> Result<(), AnswerRefused>;
}
