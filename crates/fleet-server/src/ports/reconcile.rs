//! What tells the reconciler to look again (Plan.md P10.5; ADR-0019, S37): a
//! hint after a commit or a stream's change, which never waits and never
//! fails.
//!
//! A hint carries no facts: the reconciler reads them from the database and
//! the live streams on its pass, and runs a full pass every 30 s besides, so
//! a hint that's lost costs only latency. Services send one after their own
//! commit, as they end a stream after theirs.

use core::fmt;

use fleet_core::id::AgentId;

/// What changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// Something stored: a bot, an account, a user.
    Store,
    /// An agent's stream was welcomed.
    Welcomed(AgentId),
    /// An agent reported what the reconciler waits for: a releasing bot's
    /// `Removed`, or a bot being stopped resting.
    Settled(AgentId),
    /// An agent's stream ended.
    Ended(AgentId),
}

/// Tells the reconciler to look again; see the [module docs](self).
pub trait Reconcile: Send + Sync + fmt::Debug {
    /// Notes `change`. Never waits; a hint that doesn't fit is folded into
    /// the next full pass.
    fn changed(&self, change: Change);
}

/// No reconciler: the hints go nowhere. For a server without the control
/// plane, and for the host's commands.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoReconcile;

impl Reconcile for NoReconcile {
    fn changed(&self, _change: Change) {}
}
