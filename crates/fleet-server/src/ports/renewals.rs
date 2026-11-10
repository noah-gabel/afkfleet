//! What `RenewCertificate` needs from the app (Plan.md P13.6; ADR-0027): a
//! new certificate for an agent the call's own certificate admitted.
//!
//! The gRPC port calls it through this trait, as it calls
//! [`AgentConnections`](super::agent_connections::AgentConnections), so a
//! test can put a fake behind it.

use core::fmt;
use core::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::pki::CertFingerprint;

use super::agent_connections::Admitted;

/// A renewed certificate, for its answer.
#[derive(Clone, PartialEq, Eq)]
pub struct Renewed {
    /// The new certificate's fingerprint.
    pub fingerprint: CertFingerprint,
    /// The new certificate as one PEM `CERTIFICATE` section, for the agent.
    pub certificate_pem: String,
    /// When it stops being valid.
    pub not_after: DateTime<Utc>,
}

impl fmt::Debug for Renewed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The certificate isn't secret, but its text says nothing in a log.
        f.debug_struct("Renewed")
            .field("fingerprint", &self.fingerprint)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// Why a certificate wasn't renewed.
#[derive(Debug, thiserror::Error)]
pub enum RenewError {
    /// The certificate stopped admitting its agent since the call was
    /// admitted: one answer for every reason, as a connection's.
    #[error("the certificate doesn't admit an agent")]
    Refused,
    /// The database is busy; asking again soon may help.
    #[error("the database is busy")]
    Busy,
    /// The signing took longer than the call's deadline. Nothing was stored.
    #[error("the renewal took longer than its deadline")]
    TooSlow,
    /// The server failed.
    #[error("the renewal failed")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Renewing agents' certificates; see the [module docs](self).
#[async_trait]
pub trait CertificateRenewals: Send + Sync + fmt::Debug {
    /// Signs a new certificate for `admitted`'s agent and the key of
    /// `presented`, the certificate its call connected with, within
    /// `deadline`, and stores its fingerprint beside the presented one's,
    /// which stays valid; the agent's other certificates are revoked.
    /// Audited as `agent.cert_renew`.
    ///
    /// # Errors
    /// [`RenewError::Refused`] when the presented certificate or its agent
    /// stopped admitting it meanwhile (audited like a refused connection),
    /// or another [`RenewError`]; nothing is stored then.
    async fn renew(
        &self,
        admitted: &Admitted,
        presented: Vec<u8>,
        deadline: Duration,
    ) -> Result<Renewed, RenewError>;
}
