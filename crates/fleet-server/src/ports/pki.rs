//! Issuing agents' certificates (Plan.md P10.2, P10.3; ADR-0019):
//! [`AgentCertificates`], the CA as the enrollment service sees it.
//!
//! The service hands over a CSR exactly as the agent sent it and the agent ID
//! it minted. The CSR proves only that the agent holds its key: its
//! signature is checked and its public key taken, and nothing else in it is
//! used. Subject, SAN (the agent ID), key usage and lifetime are the
//! server's. Only Ed25519 keys are accepted (S10).
//!
//! The calls are synchronous CPU work, so the service runs them in
//! `spawn_blocking`, and never inside a write transaction (ADR-0015).

use core::fmt;

use chrono::{DateTime, Utc};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::system::RandomError;

/// Why a CSR was refused: a closed set, for the refusal's log line. None
/// holds any part of the CSR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CsrRefusal {
    /// It's longer than [`CsrRefusal::MAX_BYTES`].
    #[error("the certificate request is too large")]
    TooLarge,
    /// It isn't a PKCS#10 certificate request.
    #[error("the certificate request can't be read")]
    Unparsable,
    /// Its signature doesn't verify with its own public key.
    #[error("the certificate request's signature is invalid")]
    Signature,
    /// Its key isn't an Ed25519 key.
    #[error("the certificate request's key isn't Ed25519")]
    KeyType,
    /// It asks for an extension, such as a name or a key usage.
    #[error("the certificate request asks for extensions")]
    Extensions,
}

impl CsrRefusal {
    /// The longest CSR accepted, in bytes: an Ed25519 CSR without extensions
    /// takes about 150.
    pub const MAX_BYTES: usize = 4096;

    /// The refusal's name in a log line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::Unparsable => "unparsable",
            Self::Signature => "signature",
            Self::KeyType => "key_type",
            Self::Extensions => "extensions",
        }
    }
}

/// Why no certificate was issued.
#[derive(Debug, thiserror::Error)]
pub enum IssueError {
    /// The CSR was refused.
    #[error(transparent)]
    Csr(#[from] CsrRefusal),
    /// The serial number couldn't be drawn.
    #[error("the certificate's serial number couldn't be drawn")]
    Random(#[from] RandomError),
    /// The certificate would be valid outside the years 1970 to 9999.
    #[error("the certificate's validity is out of range")]
    OutOfRange,
    /// The certificate couldn't be built or signed. The source is the
    /// adapter's own error, for the log.
    #[error("the certificate couldn't be signed")]
    Signing(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// A certificate the CA signed.
#[derive(Clone, PartialEq, Eq)]
pub struct IssuedCertificate {
    /// The certificate, DER-encoded.
    pub der: Vec<u8>,
    /// SHA-256 of `der`.
    pub fingerprint: CertFingerprint,
    /// When it starts being valid.
    pub not_before: DateTime<Utc>,
    /// When it stops being valid: its notAfter, in whole seconds.
    pub not_after: DateTime<Utc>,
}

impl fmt::Debug for IssuedCertificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The certificate isn't secret, but its bytes say nothing in a log.
        f.debug_struct("IssuedCertificate")
            .field("fingerprint", &self.fingerprint)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// The CA, as far as enrollment needs it.
pub trait AgentCertificates: Send + Sync + fmt::Debug {
    /// Checks `csr_der` and signs a certificate for its public key that
    /// names `agent` and is valid from about `now` for the configured
    /// lifetime.
    ///
    /// # Errors
    /// [`IssueError::Csr`] for a refused CSR, or another [`IssueError`].
    fn issue(
        &self,
        csr_der: &[u8],
        agent: AgentId,
        now: DateTime<Utc>,
    ) -> Result<IssuedCertificate, IssueError>;

    /// The CA certificate's fingerprint, which enrollment bundles pin.
    fn ca_fingerprint(&self) -> CertFingerprint;
}
