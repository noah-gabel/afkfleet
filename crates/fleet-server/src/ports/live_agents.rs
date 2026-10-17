//! What the app asks of the agents' live streams (Plan.md P10.5, P10.10;
//! ADR-0019, S27, S31, S41, R22): to end one whose permission ended, what the
//! server knows of one that lives, and of a bot it holds.
//!
//! The gRPC port's `AgentRegistry` implements it, so the app reaches the
//! streams without knowing the port; a service calls [`LiveAgents::end`]
//! after its own commit, so a stream it misses finds the commit at its
//! hello's transaction.

use core::fmt;

use chrono::{DateTime, Utc};
use fleet_core::bot::BotSnapshot;
use fleet_core::id::{AgentId, BotId};

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
}
