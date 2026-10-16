//! What an agent's `Connect` stream needs from the app (Plan.md P10.4;
//! ADR-0019, S24, S29, S30, R22): admitting the certificate it connected
//! with, recording its hello, and recording how its stream ended.
//!
//! The gRPC port calls it through this trait, not the service itself, so a
//! test can run the stream on paused time over an in-memory transport with a
//! fake behind it; sqlx's worker threads can't run on paused time.

use core::fmt;
use std::net::IpAddr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::{AgentId, RequestId};
use fleet_core::pki::CertFingerprint;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};

/// The client certificate a `Connect` call presented, and where it came
/// from.
pub struct Presented {
    /// The call's request ID, for the log.
    pub request: RequestId,
    /// SHA-256 of the certificate's DER.
    pub fingerprint: CertFingerprint,
    /// The certificate, DER-encoded: its SAN is read only once its
    /// fingerprint is known.
    pub certificate: Vec<u8>,
    /// The peer's address, for the audit log only, never a log line.
    pub peer: Option<IpAddr>,
}

impl fmt::Debug for Presented {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The certificate isn't secret, but its bytes say nothing in a log,
        // and the peer's address stays out of logs (ADR-0019, S14).
        f.debug_struct("Presented")
            .field("request", &self.request)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// An agent its certificate admitted: who it is, by the server's records.
#[derive(Clone, PartialEq, Eq)]
pub struct Admitted {
    /// The call's request ID.
    pub request: RequestId,
    /// The agent, from its certificate's row.
    pub agent: AgentId,
    /// The agent's name as the server stored it at its enrollment.
    pub name: AgentName,
    /// The certificate it connected with.
    pub fingerprint: CertFingerprint,
    /// When that certificate expires.
    pub not_after: DateTime<Utc>,
    /// The peer's address, for the audit log only.
    pub peer: Option<IpAddr>,
}

impl fmt::Debug for Admitted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Admitted")
            .field("request", &self.request)
            .field("agent", &self.agent)
            .field("name", &self.name)
            .field("fingerprint", &self.fingerprint)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// Why a certificate or a hello wasn't admitted.
#[derive(Debug, thiserror::Error)]
pub enum AdmitError {
    /// The certificate doesn't admit an agent: one answer for every reason,
    /// which only the log and the audit entry name.
    #[error("the certificate doesn't admit an agent")]
    Refused,
    /// The database is busy; asking again soon may help.
    #[error("the database is busy")]
    Busy,
    /// The server failed.
    #[error("the admission failed")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// How an admitted agent's stream ended (ADR-0019, R19). Its
/// [`as_str`](Self::as_str) is `agent.disconnect`'s `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamEnd {
    /// The agent closed it.
    Closed,
    /// The agent sent nothing within the heartbeat timeout.
    HeartbeatTimeout,
    /// The transport failed, or the agent reset it.
    StreamError,
    /// The agent broke the protocol: a second hello, or a message over the
    /// size limit.
    ProtocolError,
    /// The agent read too slowly: its stream's queue was full.
    TooSlow,
    /// The agent was disabled.
    Disabled,
    /// The agent's certificate was revoked.
    CertificateRevoked,
    /// The agent's certificate expired.
    CertificateExpired,
    /// The server shut down.
    ServerShutdown,
}

impl StreamEnd {
    /// The end's stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::HeartbeatTimeout => "heartbeat_timeout",
            Self::StreamError => "stream_error",
            Self::ProtocolError => "protocol_error",
            Self::TooSlow => "too_slow",
            Self::Disabled => "disabled",
            Self::CertificateRevoked => "certificate_revoked",
            Self::CertificateExpired => "certificate_expired",
            Self::ServerShutdown => "server_shutdown",
        }
    }
}

impl fmt::Display for StreamEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An admitted agent's stream as it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    /// Why.
    pub reason: StreamEnd,
    /// When its hello was accepted, by the server's clock.
    pub connected_at: DateTime<Utc>,
    /// When the server last heard from it, by its clock.
    pub last_seen: DateTime<Utc>,
}

/// What a `Connect` stream asks the app; see the [module docs](self).
#[async_trait]
pub trait AgentConnections: Send + Sync + fmt::Debug {
    /// Admits `presented`'s certificate: its fingerprint's row, unrevoked
    /// and unexpired, of an enabled agent its SAN names. A refusal of a
    /// certificate the server knows is audited, at most 10 times a day per
    /// agent.
    ///
    /// # Errors
    /// [`AdmitError`]: one [`AdmitError::Refused`] for every reason.
    async fn admit(&self, presented: Presented) -> Result<Admitted, AdmitError>;

    /// Records `admitted`'s accepted hello in one transaction that checks the
    /// certificate and the agent again: its capacity, its version and when it
    /// was seen, with `agent.connect`.
    ///
    /// # Errors
    /// [`AdmitError::Refused`] when the certificate or the agent stopped
    /// admitting it meanwhile (audited like [`admit`](Self::admit)'s
    /// refusals), or another [`AdmitError`].
    async fn hello(
        &self,
        admitted: &Admitted,
        capacity: AgentCapacity,
        version: &AgentVersion,
    ) -> Result<(), AdmitError>;

    /// Records that `admitted`'s hello named another agent, like
    /// [`admit`](Self::admit)'s refusals. A failure is logged here.
    async fn name_refused(&self, admitted: &Admitted);

    /// Records how `admitted`'s stream ended: `agent.disconnect`, and its last
    /// message's time as `last_seen_at`. A failure is logged here.
    async fn ended(&self, admitted: &Admitted, ended: Ended);
}
