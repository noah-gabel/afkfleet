//! What the reconciler asks of the store (Plan.md P10.5; ADR-0019, S36, S37,
//! S40, S43, R22): everything it plans from, in one read, and each change of a
//! bot's agent, in its own audited transaction.
//!
//! The gRPC port's reconciler calls it through this trait, not the service
//! itself, so a test can run the reconciler on paused time with a fake behind
//! it; sqlx's worker threads can't run on paused time.

use core::fmt;

use async_trait::async_trait;
use fleet_core::audit::AuditError;
use fleet_core::bot::LastReport;
use fleet_core::id::{AgentId, BotId};
use fleet_core::placement::{DetachReason, UnassignReason};

use super::bots::Fleet;
use super::store::StoreError;

/// A bot just assigned, as its assignment's transaction read its row: a
/// new placement's restore comes from this, never from the pass's earlier
/// read, which a Resume or a Reset may have cleared meanwhile (ADR-0019,
/// S48).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assigned {
    /// The bot's stored last state when it was assigned.
    pub last: Option<LastReport>,
}

/// Why a change wasn't written.
#[derive(Debug, thiserror::Error)]
pub enum AssignError {
    /// The database failed or is busy.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
}

impl AssignError {
    /// Whether the database was busy: asking again soon may help.
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        matches!(self, Self::Store(StoreError::Busy))
    }
}

/// What the reconciler asks of the store; see the [module docs](self).
#[async_trait]
pub trait Assignments: Send + Sync + fmt::Debug {
    /// Every bot, every open release record and the enabled agents, from one
    /// snapshot.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn fleet(&self) -> Result<Fleet, StoreError>;

    /// Assigns `bot` to `agent` while it has no agent and `agent` is
    /// enabled, with `bot.assign`: the bot as the assignment's transaction
    /// read it, or `None` when nothing changed, and then nothing is written.
    ///
    /// # Errors
    /// [`AssignError`].
    async fn assign(&self, bot: BotId, agent: AgentId) -> Result<Option<Assigned>, AssignError>;

    /// Records the decision to take `bot` off `agent` for `reason`, with its
    /// latest report `last`, while its row names `agent`: `bot.unassign`.
    /// The row keeps the agent until it no longer runs the bot. Whether its
    /// row named the agent; nothing is written when it didn't.
    ///
    /// # Errors
    /// [`AssignError`].
    async fn unassign(
        &self,
        bot: BotId,
        agent: AgentId,
        reason: UnassignReason,
        last: Option<LastReport>,
    ) -> Result<bool, AssignError>;

    /// Clears `bot`'s assignment to `agent`, and `agent`'s release record of
    /// it, for `reason`, storing `last` first: `bot.detach`. Whether either
    /// changed; nothing is written when neither did.
    ///
    /// # Errors
    /// [`AssignError`].
    async fn detach(
        &self,
        bot: BotId,
        agent: AgentId,
        reason: DetachReason,
        last: Option<LastReport>,
    ) -> Result<bool, AssignError>;

    /// Persists the fence for `bot`, which `agent`'s hello listed while its
    /// row names `assigned_to`: a release record and `bot.stray`. Whether one
    /// was written; nothing is when one exists, or nothing says which account
    /// the bot plays.
    ///
    /// # Errors
    /// [`AssignError`].
    async fn stray(
        &self,
        bot: BotId,
        agent: AgentId,
        assigned_to: Option<AgentId>,
    ) -> Result<bool, AssignError>;
}
