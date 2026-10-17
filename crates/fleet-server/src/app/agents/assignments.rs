//! [`AssignmentService`]: the bots' assignments to agents, as the reconciler
//! changes them (Plan.md P10.5; ADR-0019, S32, S36, S40, S43).
//!
//! Every change of a bot's agent is its own short transaction with one audit
//! entry, no actor: the server's reconciler decided. Nothing is written when
//! nothing changes: a reconnect with the same assignments records nothing.
//! The changes are conditional, so a pass that read an older state can't
//! undo a newer one: a bot is assigned only from no agent, and cleared only
//! from the agent that no longer runs it.

use std::sync::Arc;

use async_trait::async_trait;
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::bot::LastReport;
use fleet_core::id::{AgentId, BotId};
use fleet_core::placement::{DetachReason, UnassignReason};
use fleet_core::system::Clock;

use crate::app::audit::AuditService;
use crate::ports::assignments::{AssignError, Assignments};
use crate::ports::audit::NewAuditEntry;
use crate::ports::bots::Fleet;
use crate::ports::store::{Store, StoreError};

/// The assignments' changes; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct AssignmentService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    audit: AuditService,
}

impl AssignmentService {
    /// The service over `store`, stamping with `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            store,
            audit,
        }
    }
}

impl AssignmentService {
    /// An entry of `action` about `bot` and `agent`, with no actor.
    fn entry(
        &self,
        action: AuditAction,
        outcome: AuditOutcome,
        bot: BotId,
        metadata: AuditMetadata,
    ) -> NewAuditEntry {
        self.audit
            .entry(action, outcome)
            .target(AuditTarget::Bot(bot))
            .metadata(metadata)
    }
}

#[async_trait]
impl Assignments for AssignmentService {
    async fn fleet(&self) -> Result<Fleet, StoreError> {
        self.store.bots().fleet().await
    }

    async fn assign(&self, bot: BotId, agent: AgentId) -> Result<bool, AssignError> {
        let metadata = AuditMetadata::new().id("agent", agent)?;
        let mut tx = self.store.write().await?;
        if !tx.bots().assign(bot, agent).await? {
            // Nothing changed; dropping the transaction writes nothing.
            return Ok(false);
        }
        tx.commit(self.entry(
            AuditAction::BOT_ASSIGN,
            AuditOutcome::Success,
            bot,
            metadata,
        ))
        .await?;
        Ok(true)
    }

    async fn unassign(
        &self,
        bot: BotId,
        agent: AgentId,
        reason: UnassignReason,
        last: Option<LastReport>,
    ) -> Result<bool, AssignError> {
        let metadata = AuditMetadata::new()
            .id("agent", agent)?
            .text("reason", reason.as_str())?;
        let mut tx = self.store.write().await?;
        let names_agent = tx
            .bots()
            .find(bot)
            .await?
            .is_some_and(|row| row.agent == Some(agent));
        if !names_agent {
            return Ok(false);
        }
        if let Some(last) = last {
            tx.bots().record_state(bot, agent, &last).await?;
        }
        tx.commit(self.entry(
            AuditAction::BOT_UNASSIGN,
            AuditOutcome::Success,
            bot,
            metadata,
        ))
        .await?;
        Ok(true)
    }

    async fn detach(
        &self,
        bot: BotId,
        agent: AgentId,
        reason: DetachReason,
        last: Option<LastReport>,
    ) -> Result<bool, AssignError> {
        let metadata = AuditMetadata::new()
            .id("agent", agent)?
            .text("reason", reason.as_str())?;
        let mut tx = self.store.write().await?;
        // While the row still names the agent, so the report lands first.
        if let Some(last) = last {
            tx.bots().record_state(bot, agent, &last).await?;
        }
        if !tx.bots().detach(bot, agent).await? {
            return Ok(false);
        }
        tx.commit(self.entry(
            AuditAction::BOT_DETACH,
            AuditOutcome::Success,
            bot,
            metadata,
        ))
        .await?;
        Ok(true)
    }

    async fn stray(
        &self,
        bot: BotId,
        agent: AgentId,
        assigned_to: Option<AgentId>,
    ) -> Result<bool, AssignError> {
        let mut metadata = AuditMetadata::new().id("agent", agent)?;
        if let Some(assigned_to) = assigned_to {
            metadata = metadata.id("assigned_to", assigned_to)?;
        }
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        if !tx.bots().stray(bot, agent, now).await? {
            return Ok(false);
        }
        tx.commit(self.entry(AuditAction::BOT_STRAY, AuditOutcome::Failure, bot, metadata))
            .await?;
        Ok(true)
    }
}
