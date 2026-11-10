//! [`CertificateRenewalService`]: an agent's next certificate (Plan.md
//! P13.6; ADR-0027).
//!
//! - **Who may ask** is the caller's check: the gRPC port admits the call's
//!   certificate as it admits a `Connect` call's, and hands over what it
//!   admitted and the certificate itself.
//! - **Signing** runs in `spawn_blocking`, outside any transaction, within
//!   the call's deadline: the CA signs the presented certificate's own key
//!   again, for the agent its row names, from now for the configured
//!   lifetime. It writes nothing, so a deadline that cuts it off leaves
//!   nothing behind.
//! - **One transaction** then reads the presented certificate and its agent
//!   again and applies fleet-core's rule (unrevoked, unexpired, enabled), so
//!   a disable or a revocation meanwhile wins; revokes every other
//!   certificate of the agent (a renewal whose answer it never stored);
//!   stores the new fingerprint; and commits `agent.cert_renew`. It runs to
//!   its end past the deadline, as an enrollment's does.
//! - **The presented certificate stays valid:** an answer that's lost on
//!   its way locks no agent out. The agent's first hello with the new
//!   certificate revokes it (`AgentConnectionService`).
//! - **A refusal** is an `agent.connect` Failure with its reason, under the
//!   same daily cap as a refused connection, and one answer.

use core::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use fleet_core::agent::ConnectRefusal;
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::system::Clock;
use tokio::time::timeout;
use tracing::info;

use super::connections::{AgentConnectionService, Refusal};
use crate::app::audit::AuditService;
use crate::metrics::ServerMetrics;
use crate::ports::agent_connections::Admitted;
use crate::ports::agents::NewAgentCert;
use crate::ports::pki::AgentCertificates;
use crate::ports::renewals::{CertificateRenewals, RenewError, Renewed};
use crate::ports::store::{Store, StoreError};

/// The renewals' use case; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct CertificateRenewalService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    certificates: Arc<dyn AgentCertificates>,
    connections: AgentConnectionService,
    audit: AuditService,
    metrics: ServerMetrics,
}

impl CertificateRenewalService {
    /// The service over `store`, stamping with `clock` and signing with
    /// `certificates`; a refusal is recorded through `connections`, as a
    /// refused connection is.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        store: Arc<dyn Store>,
        certificates: Arc<dyn AgentCertificates>,
        connections: AgentConnectionService,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            store,
            certificates,
            connections,
            audit,
            metrics: ServerMetrics::off(),
        }
    }

    /// The service, counting every renewal in `metrics`.
    #[must_use]
    pub fn with_metrics(self, metrics: ServerMetrics) -> Self {
        Self { metrics, ..self }
    }
}

#[async_trait]
impl CertificateRenewals for CertificateRenewalService {
    async fn renew(
        &self,
        admitted: &Admitted,
        presented: Vec<u8>,
        deadline: Duration,
    ) -> Result<Renewed, RenewError> {
        let now = self.clock.now();
        let agent = admitted.agent;
        // The deadline bounds only the signing, which writes nothing, so
        // dropping it leaves nothing behind; its thread ends by itself. The
        // transaction that follows runs to its end, bounded by the write
        // pool's acquire timeout and SQLite's busy timeout: sqlx finishes a
        // COMMIT already sent even when its future is dropped, so cutting it
        // off could store a certificate that's never sent.
        let certificates = Arc::clone(&self.certificates);
        let signing =
            tokio::task::spawn_blocking(move || certificates.renew(&presented, agent, now));
        let issued = timeout(deadline, signing)
            .await
            .map_err(|_| RenewError::TooSlow)?
            .map_err(internal)?
            .map_err(internal)?;

        let mut tx = self.store.write().await.map_err(store_error)?;
        // Read again where it's written: a disable or a revocation since the
        // call was admitted wins.
        let cert = tx
            .agents()
            .cert(admitted.fingerprint)
            .await
            .map_err(store_error)?;
        let row = tx.agents().find(agent).await.map_err(store_error)?;
        let judged = match (cert, row) {
            (Some(cert), Some(row)) if cert.agent == agent => {
                AgentConnectionService::judge(&cert, row.enabled, now)
            }
            // A row that's gone, or names another agent now, admits nobody.
            _ => Err(ConnectRefusal::Revoked),
        };
        if let Err(reason) = judged {
            drop(tx);
            self.connections
                .refuse(Refusal {
                    request: admitted.request,
                    agent,
                    fingerprint: admitted.fingerprint,
                    peer: admitted.peer,
                    reason,
                })
                .await;
            return Err(RenewError::Refused);
        }
        // An earlier renewal whose answer the agent never stored: it asks
        // with the certificate it still holds, so nothing else is in use.
        let revoked = tx
            .agents()
            .revoke_other_certs(agent, admitted.fingerprint, now)
            .await
            .map_err(store_error)?;
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint: issued.fingerprint,
                agent,
                not_after: issued.not_after,
                created_at: now,
            })
            .await
            .map_err(store_error)?;
        let metadata = AuditMetadata::new()
            .text("fingerprint", &issued.fingerprint.to_string())
            .and_then(|metadata| metadata.text("replaces", &admitted.fingerprint.to_string()))
            .and_then(|metadata| metadata.time("not_after", issued.not_after))
            .and_then(|metadata| {
                metadata.int(
                    "certificates_revoked",
                    i64::try_from(revoked).unwrap_or(i64::MAX),
                )
            })
            .map_err(internal)?;
        let mut entry = self
            .audit
            .entry(AuditAction::AGENT_CERT_RENEW, AuditOutcome::Success)
            .target(AuditTarget::Agent(agent))
            .metadata(metadata);
        if let Some(peer) = admitted.peer {
            entry = entry.ip(peer);
        }
        tx.commit(entry).await.map_err(store_error)?;
        self.metrics.certificate_renewed();
        info!(
            request_id = %admitted.request,
            agent_id = %agent,
            fingerprint = %issued.fingerprint,
            replaces = %admitted.fingerprint,
            not_after = %issued.not_after,
            "an agent's certificate was renewed"
        );
        Ok(Renewed {
            fingerprint: issued.fingerprint,
            certificate_pem: issued.pem,
            not_after: issued.not_after,
        })
    }
}

/// A store error as the renewal's: busy, or a failure.
fn store_error(error: StoreError) -> RenewError {
    match error {
        StoreError::Busy => RenewError::Busy,
        other => RenewError::Internal(Box::new(other)),
    }
}

/// A failure the caller only reports.
fn internal(error: impl std::error::Error + Send + Sync + 'static) -> RenewError {
    RenewError::Internal(Box::new(error))
}
