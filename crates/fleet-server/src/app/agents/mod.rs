//! The agents' use cases (Plan.md P10.4, P10.5, P10.6, P10.10; ADR-0019).
//!
//! - [`connections`]: admitting the certificate a connecting agent presents,
//!   recording its accepted hello, how its stream ended and a conflict over
//!   its identity, and checking the live streams again (P10.4, P10.5),
//!   behind the [`AgentConnections`](crate::ports::agent_connections::AgentConnections)
//!   port the gRPC port calls.
//! - [`assignments`]: the bots' assignments as the reconciler changes them,
//!   each change in its own audited transaction (P10.5, S36).
//! - [`session_grants`]: the answers to an agent's session requests, the
//!   one place Minecraft tokens leave the server (P10.6, S57–S61), behind
//!   the [`SessionGrants`](crate::ports::session_grants::SessionGrants)
//!   port the gRPC port calls.
//! - [`ingest`]: what agents report about the bots they hold, chat and
//!   sticky states written in batches and everything published on the bus
//!   (P10.8, S70), behind the [`Ingest`](crate::ports::ingest::Ingest) port
//!   the gRPC port calls.
//! - [`AgentService`]: the agents' list, disabling one (P10.10, S31), and
//!   releasing a disabled one's bots, from the server's host (P10.5, S32)
//!   or over the API (P11.9; ADR-0020, S49).
//!   - **The list** shows every agent, newest first, with its status
//!     (fleet-core's rule over the row, its latest unrevoked certificate and
//!     the live stream), when its stream was welcomed, and the later of the
//!     stored `last_seen_at` and the live stream's.
//!   - **Disabling** is one transaction: the acting user re-read and
//!     authorized again (an Admin or the Owner; or the server's host, whom
//!     nothing re-reads), the agent read, `enabled` off, every unrevoked
//!     certificate revoked, and `agent.disable`. Then its live stream ends,
//!     after the commit, so a stream this misses finds the commit at its
//!     hello's transaction. An agent disabled already is no change and no
//!     entry.
//!   - **Releasing** is one transaction too: over the API, the acting user
//!     re-read and authorized again (`ReleaseAgent`, Admin+); the agent read
//!     and refused while it's enabled; its bots and release records cleared,
//!     a `bot.detach` per bot and `agent.release` with the count and `via`,
//!     the actor and the IP over the API. After the commit, the sockets are
//!     told the bots and the reconciler hinted, so a running server places
//!     them at once. Nothing to release is no change and no entry. The
//!     operator vouches that the agent's host and its bots are stopped:
//!     nothing can prove it (S32).

pub mod assignments;
pub mod connections;
pub mod ingest;
pub mod session_grants;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::agent::{AgentStatus, status};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{AuthzError, Permission, ResourceContext, authorize};
use fleet_core::id::{AgentId, BotId};
use fleet_core::placement::DetachReason;
use fleet_core::system::Clock;
use tracing::{debug, info};

pub use assignments::AssignmentService;
pub use connections::AgentConnectionService;
pub use session_grants::SessionGrantService;

use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::agents::ListedAgent;
use crate::ports::live_agents::{LiveAgents, LiveStream, PermissionEnd};
use crate::ports::live_hints::{LiveHints, NoLiveHints};
use crate::ports::page::{Page, Paged};
use crate::ports::reconcile::{Change, NoReconcile, Reconcile};
use crate::ports::store::{Store, StoreError};

/// An agent as the list shows it; see the [module docs](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentView {
    /// Its row and its latest unrevoked certificate's expiry.
    pub listed: ListedAgent,
    /// Its status now.
    pub status: AgentStatus,
    /// When its live stream was welcomed, while it has one.
    pub connected_since: Option<DateTime<Utc>>,
    /// When the server last heard from it: the later of the stored time and
    /// the live stream's.
    pub last_seen_at: Option<DateTime<Utc>>,
}

/// When the server last heard from an agent: the later of the stored time
/// and its live stream's.
#[must_use]
pub(crate) fn last_seen(
    stored: Option<DateTime<Utc>>,
    live: Option<LiveStream>,
) -> Option<DateTime<Utc>> {
    match (stored, live) {
        (Some(stored), Some(live)) => Some(stored.max(live.last_seen)),
        (stored, live) => stored.or(live.map(|live| live.last_seen)),
    }
}

/// An agent [`AgentService::disable`] disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledAgent {
    /// The agent as the list shows it after.
    pub view: AgentView,
    /// How many certificates were revoked; none when the agent was disabled
    /// already, and nothing changed.
    pub revoked: Option<u64>,
}

/// Who releases a disabled agent's bots.
#[derive(Debug, Clone, Copy)]
pub enum Releaser<'a> {
    /// An Admin or the Owner, through the API (ADR-0020, S49).
    Admin(&'a ActingUser),
    /// The server's host, through `afkfleet-server agent release`.
    Host,
}

/// A disabled agent whose bots [`AgentService::release`] released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedAgent {
    /// The bots released, in ID order; none when it held none.
    pub bots: Vec<BotId>,
    /// The agent as the list shows it after.
    pub view: AgentView,
}

/// Why a disabled agent's bots weren't released.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseAgentError {
    /// There's no such agent.
    #[error("the agent doesn't exist")]
    NotFound,
    /// The agent is enabled: it may still run its bots, and can say so
    /// itself. Disable it first.
    #[error("the agent is enabled")]
    Enabled,
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("the actor may not release agents")]
    Denied(#[source] AuthzError),
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The bots were released and the release committed, but the agent
    /// couldn't be read again after it.
    #[error("the agent's bots were released, but the agent couldn't be read again")]
    ReleasedUnread {
        /// The bots released, in ID order.
        bots: Vec<BotId>,
        /// Why the agent couldn't be read.
        #[source]
        error: StoreError,
    },
}

/// Who disables an agent.
#[derive(Debug, Clone, Copy)]
pub enum Disabler<'a> {
    /// An Admin or the Owner, through the API.
    Admin(&'a ActingUser),
    /// The server's host, through `afkfleet-server agent disable`.
    Host,
}

/// Why an agent wasn't disabled.
#[derive(Debug, thiserror::Error)]
pub enum DisableAgentError {
    /// There's no such agent.
    #[error("the agent doesn't exist")]
    NotFound,
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("the actor may not disable agents")]
    Denied(#[source] AuthzError),
    /// The audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The agents' list and disabling; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct AgentService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    live: Arc<dyn LiveAgents>,
    audit: AuditService,
    live_hints: Arc<dyn LiveHints>,
    reconcile: Arc<dyn Reconcile>,
}

impl AgentService {
    /// The service over `store`, stamping with `clock`, ending streams
    /// through `live`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>, live: Arc<dyn LiveAgents>) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            store,
            live,
            audit,
            live_hints: Arc::new(NoLiveHints),
            reconcile: Arc::new(NoReconcile),
        }
    }

    /// The service, telling `reconcile` to look again after a release, so
    /// a running server places the bots at once (ADR-0020, S49). The host's
    /// command keeps none: the running server's next pass finds them.
    #[must_use]
    pub fn with_reconcile(self, reconcile: Arc<dyn Reconcile>) -> Self {
        Self { reconcile, ..self }
    }

    /// The service, telling the live events sockets through `live_hints`
    /// which bots a disable or a release changed, once its commit returned
    /// (ADR-0020, S42).
    #[must_use]
    pub fn with_live_hints(self, live_hints: Arc<dyn LiveHints>) -> Self {
        Self { live_hints, ..self }
    }

    /// The agents of `page`, newest first, and the next page's cursor.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn list(&self, page: Page<AgentId>) -> Result<Paged<AgentView, AgentId>, StoreError> {
        let rows = self.store.agents().list(page).await?;
        let now = self.clock.now();
        let listed = Paged::split(rows, page.limit, |listed| listed.agent.id);
        Ok(Paged {
            items: listed
                .items
                .into_iter()
                .map(|listed| self.view(listed, now))
                .collect(),
            next: listed.next,
        })
    }

    /// `listed` as the list shows it at `now`, with its live stream.
    fn view(&self, listed: ListedAgent, now: DateTime<Utc>) -> AgentView {
        let live = self.live.live(listed.agent.id);
        let status = status(listed.agent.enabled, listed.not_after, live.is_some(), now);
        AgentView {
            status,
            connected_since: live.map(|live| live.connected_at),
            last_seen_at: last_seen(listed.agent.last_seen_at, live),
            listed,
        }
    }

    /// Releases the disabled agent `id`'s bots for `by`: every bot assigned
    /// to it and every release record naming it cleared, in one transaction
    /// with a `bot.detach` per bot and `agent.release`, so the bots can be
    /// placed elsewhere (ADR-0019, S32, R25; ADR-0020, S49). The operator
    /// vouches that the agent's host and its bots are stopped: nothing can
    /// prove it. The agent as the list shows it after, with the bots
    /// released, in ID order; none is no change and no entry. See the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`ReleaseAgentError`].
    pub async fn release(
        &self,
        by: Releaser<'_>,
        id: AgentId,
    ) -> Result<ReleasedAgent, ReleaseAgentError> {
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        if let Releaser::Admin(acting) = by {
            let Some(rechecked) = recheck(tx.as_mut(), acting).await? else {
                debug!(user_id = %acting.actor.id, "an agent's release was refused: the session is gone");
                return Err(ReleaseAgentError::SessionGone);
            };
            authorize(
                &rechecked.actor,
                Permission::ReleaseAgent,
                &ResourceContext::Global,
            )
            .map_err(ReleaseAgentError::Denied)?;
        }
        let agent = tx
            .agents()
            .find(id)
            .await?
            .ok_or(ReleaseAgentError::NotFound)?;
        if agent.enabled {
            return Err(ReleaseAgentError::Enabled);
        }
        let bots = tx.bots().release_all(id).await?;
        if bots.is_empty() {
            // Nothing to release: dropping the transaction writes nothing.
            drop(tx);
        } else {
            for bot in &bots {
                let metadata = AuditMetadata::new()
                    .id("agent", id)?
                    .text("reason", DetachReason::AgentReleased.as_str())?;
                let entry = self
                    .audit
                    .entry(AuditAction::BOT_DETACH, AuditOutcome::Success)
                    .target(AuditTarget::Bot(*bot))
                    .metadata(metadata);
                tx.audit().record(&entry).await?;
            }
            let metadata =
                AuditMetadata::new().int("count", i64::try_from(bots.len()).unwrap_or(i64::MAX))?;
            let entry = self
                .audit
                .entry(AuditAction::AGENT_RELEASE, AuditOutcome::Success)
                .target(AuditTarget::Agent(id));
            let entry = match by {
                Releaser::Admin(acting) => entry
                    .actor(acting.actor.id)
                    .ip(acting.ip)
                    .metadata(metadata.text("via", "http")?),
                Releaser::Host => entry.metadata(metadata.text("via", "cli")?),
            };
            tx.commit(entry).await?;
            self.live_hints.bots(&bots);
            self.reconcile.changed(Change::Store);
            info!(agent_id = %id, agent = %agent.name, bots = bots.len(), "a disabled agent's bots were released");
        }
        let listed = match self.store.agents().listed(id).await {
            Ok(listed) => listed.ok_or(ReleaseAgentError::NotFound)?,
            // Committed already: whoever called says the bots went.
            Err(error) => return Err(ReleaseAgentError::ReleasedUnread { bots, error }),
        };
        Ok(ReleasedAgent {
            bots,
            view: self.view(listed, now),
        })
    }

    /// Disables agent `id` for `by` and ends its stream: the agent as the
    /// list shows it after, and what changed. See the [module docs](self).
    ///
    /// # Errors
    /// [`DisableAgentError`].
    pub async fn disable(
        &self,
        by: Disabler<'_>,
        id: AgentId,
    ) -> Result<DisabledAgent, DisableAgentError> {
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        if let Disabler::Admin(acting) = by {
            let Some(rechecked) = recheck(tx.as_mut(), acting).await? else {
                debug!(user_id = %acting.actor.id, "an agent's disable was refused: the session is gone");
                return Err(DisableAgentError::SessionGone);
            };
            authorize(
                &rechecked.actor,
                Permission::DisableAgent,
                &ResourceContext::Global,
            )
            .map_err(DisableAgentError::Denied)?;
        }
        let agent = tx
            .agents()
            .find(id)
            .await?
            .ok_or(DisableAgentError::NotFound)?;
        let revoked = if tx.agents().disable(id).await? {
            let revoked = tx.agents().revoke_certs(id, now).await?;
            let bots = tx.bots().assigned_to(id).await?;
            let metadata = AuditMetadata::new()
                .text("name", agent.name.as_str())?
                .int(
                    "certificates_revoked",
                    i64::try_from(revoked).unwrap_or(i64::MAX),
                )?;
            let entry = self
                .audit
                .entry(AuditAction::AGENT_DISABLE, AuditOutcome::Success)
                .target(AuditTarget::Agent(id));
            let entry = match by {
                Disabler::Admin(acting) => entry
                    .actor(acting.actor.id)
                    .ip(acting.ip)
                    .metadata(metadata.text("via", "http")?),
                Disabler::Host => entry.metadata(metadata.text("via", "cli")?),
            };
            tx.commit(entry).await?;
            self.live_hints.bots(&bots);
            info!(agent_id = %id, agent = %agent.name, certificates_revoked = revoked, "an agent was disabled");
            Some(revoked)
        } else {
            // Disabled already: nothing to change, and dropping the
            // transaction writes nothing.
            drop(tx);
            None
        };
        // After the commit: a stream that this misses finds the commit at
        // its hello's transaction (R13). From the host, the running
        // server's sweep ends it (U8).
        let _ = self.live.end(id, PermissionEnd::Disabled);
        let listed = self
            .store
            .agents()
            .listed(id)
            .await?
            .ok_or(DisableAgentError::NotFound)?;
        Ok(DisabledAgent {
            view: self.view(listed, now),
            revoked,
        })
    }
}
