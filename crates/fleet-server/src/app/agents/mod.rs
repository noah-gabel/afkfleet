//! The agents' use cases (Plan.md P10.4, P10.5, P10.10; ADR-0019).
//!
//! - [`connections`]: admitting the certificate a connecting agent presents,
//!   recording its accepted hello, how its stream ended and a conflict over
//!   its identity, and checking the live streams again (P10.4, P10.5),
//!   behind the [`AgentConnections`](crate::ports::agent_connections::AgentConnections)
//!   port the gRPC port calls.
//! - [`AgentService`]: the agents' list and disabling one (P10.10, S31).
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

pub mod connections;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::agent::{AgentStatus, status};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{AuthzError, Permission, ResourceContext, authorize};
use fleet_core::id::AgentId;
use fleet_core::system::Clock;
use tracing::{debug, info};

pub use connections::AgentConnectionService;

use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::agents::ListedAgent;
use crate::ports::live_agents::{LiveAgents, PermissionEnd};
use crate::ports::page::{Page, Paged};
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

/// An agent [`AgentService::disable`] disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledAgent {
    /// The agent as the list shows it after.
    pub view: AgentView,
    /// How many certificates were revoked; none when the agent was disabled
    /// already, and nothing changed.
    pub revoked: Option<u64>,
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
        }
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
        let last_seen_at = match (listed.agent.last_seen_at, live) {
            (Some(stored), Some(live)) => Some(stored.max(live.last_seen)),
            (stored, live) => stored.or(live.map(|live| live.last_seen)),
        };
        AgentView {
            status,
            connected_since: live.map(|live| live.connected_at),
            last_seen_at,
            listed,
        }
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
