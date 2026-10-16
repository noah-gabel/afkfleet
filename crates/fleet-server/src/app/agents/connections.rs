//! [`AgentConnectionService`]: who may open a `Connect` stream, by the
//! certificate they present (Plan.md P10.4; ADR-0019, S1, S24, S29, S30).
//!
//! - **Admission** reads the presented certificate's row by its fingerprint,
//!   then its agent, and applies fleet-core's rule: unrevoked, unexpired by
//!   the server's clock, of an enabled agent. Only then is the certificate
//!   parsed, and its SAN must name the row's agent. Every refusal is one
//!   [`AdmitError::Refused`].
//! - **Refusals of a certificate the server knows** are `agent.connect`
//!   Failures with their reason and the certificate's fingerprint, at most
//!   [`MAX_AUDITED_CONNECT_REFUSALS`] per agent in any
//!   [`CONNECT_REFUSAL_WINDOW`], counted on the read pool first and again in
//!   the transaction; past the cap nothing is written. A certificate no
//!   agent holds, which only someone with the CA key can make, is an `error`
//!   line, never an entry.
//! - **An accepted hello** is one transaction: the certificate and the
//!   agent are read again, the hello's capacity and version and the time are
//!   recorded, and `agent.connect` commits, before `Welcome` is sent.
//! - **A stream's end** records its last message's time and
//!   `agent.disconnect` with its reason.
//! - **Logs** name the request, the agent and the reason, never the peer's
//!   address or the certificate's bytes.

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::agent::{
    CONNECT_REFUSAL_WINDOW, CertificateFacts, ConnectRefusal, MAX_AUDITED_CONNECT_REFUSALS, admit,
};
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::id::{AgentId, RequestId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock;
use fleet_core::time::sub;
use fleet_core::value::{AgentCapacity, AgentVersion};
use tracing::{debug, error, info, warn};

use crate::app::audit::AuditService;
use crate::ports::agent_connections::{AdmitError, Admitted, AgentConnections, Ended, Presented};
use crate::ports::agents::{AgentCert, AgentHello};
use crate::ports::pki::AgentNames;
use crate::ports::store::{Store, StoreError};

/// The connections' use case; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct AgentConnectionService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    names: Arc<dyn AgentNames>,
    audit: AuditService,
}

/// A refusal of a certificate the server knows, as it's recorded.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    request: RequestId,
    agent: AgentId,
    fingerprint: CertFingerprint,
    peer: Option<IpAddr>,
    reason: ConnectRefusal,
}

impl AgentConnectionService {
    /// The service over `store`, stamping with `clock`, reading
    /// certificates' SANs with `names`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>, names: Arc<dyn AgentNames>) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            store,
            names,
            audit,
        }
    }

    /// Whether `cert`, of an agent enabled or not as `enabled` says, admits
    /// it at `now`.
    fn judge(cert: &AgentCert, enabled: bool, now: DateTime<Utc>) -> Result<(), ConnectRefusal> {
        admit(
            CertificateFacts {
                revoked: cert.revoked_at.is_some(),
                not_after: cert.not_after,
                agent_enabled: enabled,
            },
            now,
        )
    }

    /// Records `refusal` as an `agent.connect` Failure while its agent has
    /// fewer than [`MAX_AUDITED_CONNECT_REFUSALS`] in the last
    /// [`CONNECT_REFUSAL_WINDOW`], counted on the read pool first and again
    /// in the transaction, and logs it. A failure to record is logged too:
    /// the refusal stands either way.
    async fn refuse(&self, refusal: Refusal) {
        let Refusal {
            request,
            agent,
            reason,
            ..
        } = refusal;
        match self.record(refusal).await {
            Ok(true) if reason == ConnectRefusal::SanMismatch => error!(
                request_id = %request,
                agent_id = %agent,
                reason = reason.as_str(),
                "a certificate whose SAN doesn't name its agent was refused"
            ),
            Ok(true) => info!(
                request_id = %request,
                agent_id = %agent,
                reason = reason.as_str(),
                "an agent's connection was refused"
            ),
            Ok(false) => debug!(
                request_id = %request,
                agent_id = %agent,
                reason = reason.as_str(),
                "an agent's connection was refused; its refusals today are recorded already"
            ),
            Err(failure) => warn!(
                request_id = %request,
                agent_id = %agent,
                reason = reason.as_str(),
                error = %failure,
                "an agent's connection was refused, but the refusal couldn't be recorded"
            ),
        }
    }

    /// Writes [`refuse`](Self::refuse)'s entry; whether it was written.
    async fn record(&self, refusal: Refusal) -> Result<bool, RecordError> {
        let target = AuditTarget::Agent(refusal.agent);
        let since = sub(self.clock.now(), CONNECT_REFUSAL_WINDOW);
        let action = AuditAction::AGENT_CONNECT;
        if self
            .store
            .audit()
            .failures_on_since(target, &action, since)
            .await?
            >= MAX_AUDITED_CONNECT_REFUSALS
        {
            return Ok(false);
        }
        let metadata = AuditMetadata::new()
            .text("reason", refusal.reason.as_str())?
            .text("fingerprint", &refusal.fingerprint.to_string())?;
        let mut tx = self.store.write().await?;
        // Counted where it's written, so two refusals can't both take the
        // last place.
        if tx.audit().failures_on_since(target, &action, since).await?
            >= MAX_AUDITED_CONNECT_REFUSALS
        {
            return Ok(false);
        }
        let mut entry = self
            .audit
            .entry(action, AuditOutcome::Failure)
            .target(target)
            .metadata(metadata);
        if let Some(peer) = refusal.peer {
            entry = entry.ip(peer);
        }
        tx.commit(entry).await?;
        Ok(true)
    }

    /// The hello's transaction; see [`AgentConnections::hello`].
    async fn record_hello(
        &self,
        admitted: &Admitted,
        capacity: AgentCapacity,
        version: &AgentVersion,
    ) -> Result<Result<(), ConnectRefusal>, RecordError> {
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let cert = tx.agents().cert(admitted.fingerprint).await?;
        let agent = tx.agents().find(admitted.agent).await?;
        let judged = match (cert, agent) {
            (Some(cert), Some(agent)) if cert.agent == admitted.agent => {
                Self::judge(&cert, agent.enabled, now)
            }
            // A row that's gone, or names another agent now, admits nobody.
            _ => Err(ConnectRefusal::Revoked),
        };
        if let Err(reason) = judged {
            return Ok(Err(reason));
        }
        let hello = AgentHello {
            agent: admitted.agent,
            capacity,
            version: version.clone(),
            at: now,
        };
        if !tx.agents().record_hello(&hello).await? {
            return Ok(Err(ConnectRefusal::Disabled));
        }
        let metadata = AuditMetadata::new()
            .text("fingerprint", &admitted.fingerprint.to_string())?
            .text("version", version.as_str())?
            .int("capacity", i64::from(u32::from(capacity)))?;
        let mut entry = self
            .audit
            .entry(AuditAction::AGENT_CONNECT, AuditOutcome::Success)
            .target(AuditTarget::Agent(admitted.agent))
            .metadata(metadata);
        if let Some(peer) = admitted.peer {
            entry = entry.ip(peer);
        }
        tx.commit(entry).await?;
        Ok(Ok(()))
    }

    /// The end's transaction; see [`AgentConnections::ended`].
    async fn record_end(&self, admitted: &Admitted, ended: Ended) -> Result<(), RecordError> {
        let metadata = AuditMetadata::new()
            .text("reason", ended.reason.as_str())?
            .time("connected_at", ended.connected_at)?;
        let mut tx = self.store.write().await?;
        tx.agents()
            .record_seen(admitted.agent, ended.last_seen)
            .await?;
        let mut entry = self
            .audit
            .entry(AuditAction::AGENT_DISCONNECT, AuditOutcome::Success)
            .target(AuditTarget::Agent(admitted.agent))
            .metadata(metadata);
        if let Some(peer) = admitted.peer {
            entry = entry.ip(peer);
        }
        tx.commit(entry).await?;
        Ok(())
    }
}

/// Why an entry wasn't written.
#[derive(Debug, thiserror::Error)]
enum RecordError {
    /// The database failed or is busy.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The entry's metadata couldn't be built.
    #[error(transparent)]
    Audit(#[from] fleet_core::audit::AuditError),
}

impl From<RecordError> for AdmitError {
    fn from(error: RecordError) -> Self {
        match error {
            RecordError::Store(StoreError::Busy) => Self::Busy,
            other => Self::Internal(Box::new(other)),
        }
    }
}

impl From<StoreError> for AdmitError {
    fn from(error: StoreError) -> Self {
        RecordError::Store(error).into()
    }
}

#[async_trait]
impl AgentConnections for AgentConnectionService {
    async fn admit(&self, presented: Presented) -> Result<Admitted, AdmitError> {
        let now = self.clock.now();
        let Presented {
            request,
            fingerprint,
            certificate,
            peer,
        } = presented;
        let Some(cert) = self.store.agents().cert(fingerprint).await? else {
            // Only the CA key makes a certificate the handshake accepts; one
            // the server never stored means a restored backup or a stolen
            // key. It names no agent the log could trust, so it isn't parsed.
            error!(
                request_id = %request,
                fingerprint = %fingerprint,
                "a certificate the CA signed but the server doesn't know was refused"
            );
            return Err(AdmitError::Refused);
        };
        let agent = self.store.agents().find(cert.agent).await?;
        let enabled = agent.as_ref().is_some_and(|agent| agent.enabled);
        let refusal = |reason| Refusal {
            request,
            agent: cert.agent,
            fingerprint,
            peer,
            reason,
        };
        if let Err(reason) = Self::judge(&cert, enabled, now) {
            self.refuse(refusal(reason)).await;
            return Err(AdmitError::Refused);
        }
        let Some(agent) = agent else {
            // The foreign key keeps every certificate's agent.
            return Err(AdmitError::Refused);
        };
        if self.names.agent_of(&certificate) != Some(cert.agent) {
            self.refuse(refusal(ConnectRefusal::SanMismatch)).await;
            return Err(AdmitError::Refused);
        }
        Ok(Admitted {
            request,
            agent: cert.agent,
            name: agent.name,
            fingerprint,
            not_after: cert.not_after,
            peer,
        })
    }

    async fn hello(
        &self,
        admitted: &Admitted,
        capacity: AgentCapacity,
        version: &AgentVersion,
    ) -> Result<(), AdmitError> {
        match self.record_hello(admitted, capacity, version).await? {
            Ok(()) => Ok(()),
            Err(reason) => {
                self.refuse(Refusal {
                    request: admitted.request,
                    agent: admitted.agent,
                    fingerprint: admitted.fingerprint,
                    peer: admitted.peer,
                    reason,
                })
                .await;
                Err(AdmitError::Refused)
            }
        }
    }

    async fn name_refused(&self, admitted: &Admitted) {
        self.refuse(Refusal {
            request: admitted.request,
            agent: admitted.agent,
            fingerprint: admitted.fingerprint,
            peer: admitted.peer,
            reason: ConnectRefusal::NameMismatch,
        })
        .await;
    }

    async fn ended(&self, admitted: &Admitted, ended: Ended) {
        if let Err(failure) = self.record_end(admitted, ended).await {
            warn!(
                request_id = %admitted.request,
                agent_id = %admitted.agent,
                reason = ended.reason.as_str(),
                error = %failure,
                "an agent's disconnect couldn't be recorded"
            );
        }
    }
}
