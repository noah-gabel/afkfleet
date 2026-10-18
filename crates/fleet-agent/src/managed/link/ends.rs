//! How an attempt on the control stream ended, and what the agent does
//! next (ADR-0019, S67): stop for good, try again slowly, or back off.
//!
//! - **Stop** (exit 5): `UNAUTHENTICATED`, and the server's refusals of
//!   this agent: its name, a disabled agent, a revoked or expired
//!   certificate, before or after `Welcome`; also its own certificate
//!   expired by the run's clock (R52).
//! - **Slow:** the server can't read this agent: an incompatible protocol
//!   or an invalid hello. It waits the plateau every time.
//! - **Backoff:** everything else: other statuses, transport and TLS
//!   failures, the welcome timeout, a stream that ends without a `Reject`,
//!   a first message that isn't `Welcome` or `Reject`, and the agent's own
//!   ends (a full queue, an overflowing outbox, a failed snapshot).
//!   `AlreadyConnected` and a server shutting down are routine.

use core::time::Duration;

use fleet_proto::control::RejectReason;
use tonic::{Code, Status};
use tracing::Level;

/// Why the server refused this agent for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// `UNAUTHENTICATED`: the server doesn't admit the certificate: it's
    /// unknown, revoked or expired, or the agent is disabled.
    Unauthenticated,
    /// The hello's name isn't the enrolled one.
    NameMismatch,
    /// The agent is disabled.
    AgentDisabled,
    /// The certificate was revoked.
    CertificateRevoked,
    /// The server found the certificate expired.
    CertificateExpired,
    /// The agent's own certificate expired by its clock (R52).
    OwnCertificateExpired,
}

impl Refusal {
    /// Its name, for a log line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::NameMismatch => "name_mismatch",
            Self::AgentDisabled => "agent_disabled",
            Self::CertificateRevoked => "certificate_revoked",
            Self::CertificateExpired => "certificate_expired",
            Self::OwnCertificateExpired => "own_certificate_expired",
        }
    }
}

/// What the agent does after an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// It stops: every bot is stopped, then the process exits 5.
    Stop(Refusal),
    /// It tries again after the plateau's wait.
    Slow,
    /// It tries again after the backoff's wait.
    Backoff,
}

/// Why an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cause {
    /// A gRPC status, the server's or the transport's.
    Status(Code),
    /// The server's `Reject`.
    Reject(RejectReason),
    /// Dialing or the TLS handshake failed.
    Unreachable,
    /// No `Welcome` or `Reject` within the welcome timeout.
    WelcomeTimeout,
    /// The stream ended without a `Reject`.
    Ended,
    /// The first message wasn't `Welcome` or `Reject`.
    UnexpectedFirst,
    /// A second `Welcome` came.
    SecondWelcome,
    /// A message from the server couldn't be read.
    Unreadable,
    /// The conductor's queue was full.
    QueueFull,
    /// The outbox overflowed.
    Overflow,
    /// The hello's snapshot failed.
    Snapshot,
    /// The agent's certificate isn't valid yet by its clock (R52).
    NotYetValid,
    /// The agent's certificate expired by its clock (R52).
    Expired,
}

impl Cause {
    /// Its name, for a log line; a status by its code.
    #[must_use]
    pub fn as_str(self) -> String {
        let fixed = match self {
            Self::Status(code) => return format!("status_{}", i32::from(code)),
            Self::Reject(reason) => return format!("reject_{}", reject_name(reason)),
            Self::Unreachable => "unreachable",
            Self::WelcomeTimeout => "welcome_timeout",
            Self::Ended => "ended",
            Self::UnexpectedFirst => "unexpected_first",
            Self::SecondWelcome => "second_welcome",
            Self::Unreadable => "unreadable",
            Self::QueueFull => "queue_full",
            Self::Overflow => "overflow",
            Self::Snapshot => "snapshot",
            Self::NotYetValid => "not_yet_valid",
            Self::Expired => "expired",
        };
        fixed.to_owned()
    }
}

/// An attempt's end: what comes next, why, and the wait the server asked
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct End {
    /// What the agent does.
    pub next: Next,
    /// Why.
    pub cause: Cause,
    /// `RESOURCE_EXHAUSTED`'s `retry-after`, in whole seconds.
    pub retry_after: Option<Duration>,
}

impl End {
    /// An end the agent decided itself: always a backoff.
    #[must_use]
    pub const fn own(cause: Cause) -> Self {
        Self {
            next: Next::Backoff,
            cause,
            retry_after: None,
        }
    }

    /// The end of `status`.
    #[must_use]
    pub fn of_status(status: &Status) -> Self {
        let code = status.code();
        let next = match code {
            Code::Unauthenticated => Next::Stop(Refusal::Unauthenticated),
            _ => Next::Backoff,
        };
        let retry_after = (code == Code::ResourceExhausted)
            .then(|| status.metadata().get("retry-after"))
            .flatten()
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.parse::<u64>().ok())
            .map(Duration::from_secs);
        Self {
            next,
            cause: Cause::Status(code),
            retry_after,
        }
    }

    /// The end of a `Reject` for `reason`.
    #[must_use]
    pub const fn of_reject(reason: RejectReason) -> Self {
        let next = match reason {
            RejectReason::NameMismatch => Next::Stop(Refusal::NameMismatch),
            RejectReason::AgentDisabled => Next::Stop(Refusal::AgentDisabled),
            RejectReason::CertificateRevoked => Next::Stop(Refusal::CertificateRevoked),
            RejectReason::CertificateExpired => Next::Stop(Refusal::CertificateExpired),
            RejectReason::IncompatibleProtocol | RejectReason::InvalidHello => Next::Slow,
            RejectReason::AlreadyConnected => Next::Backoff,
        };
        Self {
            next,
            cause: Cause::Reject(reason),
            retry_after: None,
        }
    }

    /// The level its first log line takes: a stop and a slow retry need a
    /// human (`error`), as does a certificate not valid yet; the rest is
    /// routine (`info`).
    #[must_use]
    pub const fn level(&self) -> Level {
        match (self.next, self.cause) {
            (Next::Stop(_) | Next::Slow, _) | (_, Cause::NotYetValid) => Level::ERROR,
            (Next::Backoff, _) => Level::INFO,
        }
    }
}

/// A reject's name, for a cause's.
const fn reject_name(reason: RejectReason) -> &'static str {
    match reason {
        RejectReason::IncompatibleProtocol => "incompatible_protocol",
        RejectReason::AgentDisabled => "agent_disabled",
        RejectReason::NameMismatch => "name_mismatch",
        RejectReason::InvalidHello => "invalid_hello",
        RejectReason::AlreadyConnected => "already_connected",
        RejectReason::CertificateRevoked => "certificate_revoked",
        RejectReason::CertificateExpired => "certificate_expired",
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tonic::metadata::MetadataValue;

    use super::*;

    fn stop(refusal: Refusal) -> Next {
        Next::Stop(refusal)
    }

    #[rstest]
    #[case(RejectReason::NameMismatch, stop(Refusal::NameMismatch))]
    #[case(RejectReason::AgentDisabled, stop(Refusal::AgentDisabled))]
    #[case(RejectReason::CertificateRevoked, stop(Refusal::CertificateRevoked))]
    #[case(RejectReason::CertificateExpired, stop(Refusal::CertificateExpired))]
    #[case(RejectReason::IncompatibleProtocol, Next::Slow)]
    #[case(RejectReason::InvalidHello, Next::Slow)]
    #[case(RejectReason::AlreadyConnected, Next::Backoff)]
    fn every_reject_has_its_next(#[case] reason: RejectReason, #[case] next: Next) {
        let end = End::of_reject(reason);

        assert_eq!(end.next, next);
        assert_eq!(end.cause, Cause::Reject(reason));
        assert_eq!(end.retry_after, None);
    }

    #[rstest]
    #[case(Code::Unauthenticated, stop(Refusal::Unauthenticated))]
    #[case(Code::PermissionDenied, Next::Backoff)]
    #[case(Code::Unavailable, Next::Backoff)]
    #[case(Code::ResourceExhausted, Next::Backoff)]
    #[case(Code::Internal, Next::Backoff)]
    #[case(Code::InvalidArgument, Next::Backoff)]
    #[case(Code::Cancelled, Next::Backoff)]
    #[case(Code::Unknown, Next::Backoff)]
    fn every_status_has_its_next(#[case] code: Code, #[case] next: Next) {
        let end = End::of_status(&Status::new(code, "the server's own words"));

        assert_eq!(end.next, next);
        assert_eq!(end.cause, Cause::Status(code));
    }

    #[rstest]
    #[case::seconds("7", Some(Duration::from_secs(7)))]
    #[case::not_a_number("soon", None)]
    #[case::negative("-1", None)]
    fn a_rate_limit_carries_its_retry_after(
        #[case] value: &'static str,
        #[case] wait: Option<Duration>,
    ) {
        let mut status = Status::resource_exhausted("slow down");
        status
            .metadata_mut()
            .insert("retry-after", MetadataValue::from_static(value));

        assert_eq!(End::of_status(&status).retry_after, wait);
        assert_eq!(End::of_status(&Status::unavailable("x")).retry_after, None);
    }

    #[test]
    fn stops_slow_retries_and_a_clock_ahead_log_at_error_the_rest_at_info() {
        assert_eq!(
            End::of_reject(RejectReason::AgentDisabled).level(),
            Level::ERROR
        );
        assert_eq!(
            End::of_reject(RejectReason::InvalidHello).level(),
            Level::ERROR
        );
        assert_eq!(End::own(Cause::NotYetValid).level(), Level::ERROR);
        assert_eq!(
            End::of_reject(RejectReason::AlreadyConnected).level(),
            Level::INFO
        );
        assert_eq!(End::own(Cause::WelcomeTimeout).level(), Level::INFO);
        assert_eq!(
            End::of_status(&Status::unavailable("x")).level(),
            Level::INFO
        );
    }

    #[test]
    fn a_cause_is_named_without_any_message() {
        let names = [
            Cause::Status(Code::Unavailable),
            Cause::Reject(RejectReason::AlreadyConnected),
            Cause::Unreachable,
            Cause::WelcomeTimeout,
            Cause::Ended,
            Cause::UnexpectedFirst,
            Cause::SecondWelcome,
            Cause::Unreadable,
            Cause::QueueFull,
            Cause::Overflow,
            Cause::Snapshot,
            Cause::NotYetValid,
            Cause::Expired,
        ]
        .map(Cause::as_str);

        let distinct: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(distinct.len(), names.len(), "{names:?}");
        assert!(
            names
                .iter()
                .all(|name| !name.is_empty() && !name.contains(' ')),
            "{names:?}"
        );
        assert_eq!(Refusal::Unauthenticated.as_str(), "unauthenticated");
    }
}
