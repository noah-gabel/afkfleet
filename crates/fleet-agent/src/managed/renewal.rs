//! [`Renewer`]: a managed agent renews its own certificate (Plan.md P13.6;
//! ADR-0027).
//!
//! - **When.** Once two thirds of the certificate's lifetime have passed,
//!   by the run's clock (fleet-core's `renewal_due`): at once for an agent
//!   that starts past that point. After a failure it tries again every
//!   [`RENEW_RETRY`], and after a renewal no sooner than that, whatever the
//!   new certificate's dates say. An expired certificate is never renewed: the link
//!   stops the agent before (R52), and it enrolls again.
//! - **How.** `RenewCertificate` on the connection that carries the
//!   welcomed stream, within `RENEW_CLIENT_DEADLINE`: no CSR, since the
//!   server signs the presented certificate's key again. The answer is
//!   checked as `enroll` checks its own (one PEM certificate the pinned CA
//!   signed for client authentication, for this agent's key, valid now);
//!   then the new endpoint is built, the certificate's file replaced in one
//!   step, and the identity swapped. The link ends its stream and connects
//!   again with the new certificate, whose hello revokes the old one on the
//!   server. Nothing is stored unless every check passed.
//! - **An attempt runs to its end:** the link awaits it without racing it
//!   against the stream, so the file and the identity in memory never
//!   disagree while the process lives. An agent that dies in between starts
//!   with the old certificate, still valid, or the new one.
//! - **A server that doesn't renew** (older than the agent) answers
//!   `UNIMPLEMENTED`: the agent keeps running on its certificate and tries
//!   again, as it does after every other failure.
//! - **Warnings.** A failure's first line of its kind is a `warn` naming the
//!   cause, repeats are `debug`. While the certificate is in its last 14
//!   days (fleet-core's `expiry`), one `warn` a day says how many days are
//!   left and what to do.
//! - **Logs** never hold the URL, a status's message, a transport error's
//!   text or the certificate's bytes.

use core::fmt;
use core::time::Duration;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use fleet_core::agent::{Expiry, certificate_time_left, expiry, renewal_due};
use fleet_core::pki::CertFingerprint;
use fleet_proto::control::{RENEW_CLIENT_DEADLINE, RenewResponse};
use fleet_proto::enrollment::MAX_RESPONSE_BYTES;
use fleet_proto::generated as wire;
use fleet_proto::generated::agent_control_client::AgentControlClient;
use fleet_runtime::RuntimeClock;
use rustls::pki_types::UnixTime;
use tokio::time::timeout;
use tonic::Code;
use tonic::transport::{Channel, Endpoint};
use tracing::{debug, error, info, warn};

use crate::diagnostics;
use crate::enroll::certificate::{self, CertificateProblem, Lifetime};
use crate::enroll::files::replace_certificate;
use crate::enroll::key::AgentKey;
use crate::enroll::pin::fingerprint;
use crate::managed::identity::{KeptCertificate, RenewalParts, tls_config};
use crate::managed::link::{self, LinkTimings};

/// How long the agent waits after a failed renewal before it tries again.
pub const RENEW_RETRY: Duration = Duration::from_hours(1);

/// How often the agent says that its certificate ends soon.
pub const REMIND_EVERY: Duration = Duration::from_hours(24);

/// A renewed identity, for the link: the endpoint with the new certificate,
/// and the certificate for the check before each attempt.
#[derive(Debug)]
pub struct Renewed {
    /// The server's endpoint, presenting the new certificate.
    pub endpoint: Endpoint,
    /// The new certificate and the CA.
    pub kept: KeptCertificate,
}

/// The renewal of a managed agent's certificate; see the
/// [module docs](self). It holds the agent's key, wiped when dropped, and
/// its `Debug` shows none of it.
pub struct Renewer {
    key: AgentKey,
    ca_pem: Vec<u8>,
    ca_der: Vec<u8>,
    cert_file: PathBuf,
    url: String,
    domain: String,
    timings: LinkTimings,
    clock: RuntimeClock,
    /// The certificate in use.
    lifetime: Lifetime,
    /// When the next attempt is due.
    next: DateTime<Utc>,
    /// When the certificate's end was last warned of.
    reminded: Option<DateTime<Utc>>,
    /// The last attempt's failure, so a repeat logs at `debug`.
    failed: Option<Failure>,
}

impl fmt::Debug for Renewer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renewer")
            .field("cert_file", &self.cert_file)
            .field("lifetime", &self.lifetime)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl Renewer {
    /// The renewal of `parts`' certificate for the server at `url`, named
    /// `domain` in its certificate, with `timings`' keepalive and retry, on
    /// the run's `clock`.
    #[must_use]
    pub fn new(
        parts: RenewalParts,
        url: String,
        domain: String,
        timings: LinkTimings,
        clock: RuntimeClock,
    ) -> Self {
        let RenewalParts {
            key,
            ca_pem,
            ca_der,
            cert_file,
            lifetime,
        } = parts;
        diagnostics::identity_valid_until(lifetime.not_after);
        Self {
            key,
            ca_pem,
            ca_der,
            cert_file,
            url,
            domain,
            timings,
            clock,
            lifetime,
            next: renewal_due(lifetime.not_before, lifetime.not_after),
            reminded: None,
            failed: None,
        }
    }

    /// Waits until the next attempt is due: at once when it's past. It's
    /// only a sleep, so dropping it loses nothing.
    pub async fn due(&self) {
        match self.clock.deadline(self.next) {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            // Further ahead than tokio's clock reaches: never in this run.
            None => core::future::pending().await,
        }
    }

    /// One attempt over `channel`, the welcomed stream's connection: the
    /// renewed identity, or none after a failure, which is logged here and
    /// tried again [`LinkTimings::renew_retry`] later.
    pub async fn attempt(&mut self, channel: Channel) -> Option<Renewed> {
        match self.renew(channel).await {
            Ok((renewed, lifetime, fingerprint)) => {
                self.lifetime = lifetime;
                // Never at once again: a certificate that's due already (a
                // server whose clock is far behind) would otherwise end
                // every stream it opens with another renewal.
                let again = fleet_core::time::add(self.clock.now(), self.timings.renew_retry);
                self.next = renewal_due(lifetime.not_before, lifetime.not_after).max(again);
                self.failed = None;
                diagnostics::identity_renewed(lifetime.not_after);
                info!(
                    certificate_sha256 = %fingerprint,
                    not_after = %lifetime.not_after,
                    "the agent's certificate was renewed"
                );
                Some(renewed)
            }
            Err(failure) => {
                self.next = fleet_core::time::add(self.clock.now(), self.timings.renew_retry);
                diagnostics::identity_renewal_failed();
                self.log(failure);
                None
            }
        }
    }

    /// The call, the answer's check, the new endpoint and the file, in that
    /// order: nothing is stored unless everything before it passed.
    async fn renew(
        &self,
        channel: Channel,
    ) -> Result<(Renewed, Lifetime, CertFingerprint), Failure> {
        let mut client =
            AgentControlClient::new(channel).max_decoding_message_size(MAX_RESPONSE_BYTES);
        let mut request = tonic::Request::new(wire::RenewRequest {});
        // The server's own deadline is shorter; this one also travels as
        // `grpc-timeout`.
        request.set_timeout(RENEW_CLIENT_DEADLINE);
        let answer = timeout(RENEW_CLIENT_DEADLINE, client.renew_certificate(request))
            .await
            .map_err(|_| Failure::TimedOut)?
            .map_err(|status| match status.code() {
                Code::Unimplemented => Failure::Unimplemented,
                code => Failure::Status(code),
            })?;
        let answer = RenewResponse::try_from(answer.into_inner()).map_err(|_| Failure::Answer)?;
        let pem = answer.certificate_pem;
        let secs = u64::try_from(self.clock.now().timestamp()).unwrap_or(0);
        let now = UnixTime::since_unix_epoch(Duration::from_secs(secs));
        let der =
            certificate::check(&pem, &self.ca_der, &self.key, now).map_err(Failure::Certificate)?;
        let lifetime = certificate::lifetime(&der).ok_or(Failure::Unreadable)?;
        let tls = tls_config(pem.as_bytes(), &self.key, &self.ca_pem, self.domain.clone());
        let endpoint =
            link::endpoint(&self.url, tls, self.timings).map_err(|_| Failure::Endpoint)?;
        let cert_file = self.cert_file.clone();
        // A local write with no timeout: cutting it off would leave the
        // file and this process disagreeing about the certificate, and the
        // thread would write on anyway.
        tokio::task::spawn_blocking(move || replace_certificate(&cert_file, pem.as_bytes()))
            .await
            .map_err(|_| Failure::Store)?
            .map_err(|error| {
                error!(%error, "the renewed certificate couldn't be stored");
                Failure::Store
            })?;
        let renewed = Renewed {
            endpoint,
            kept: KeptCertificate::new(der.clone(), self.ca_der.clone()),
        };
        Ok((renewed, lifetime, fingerprint(&der)))
    }

    /// Logs `failure`: the first of its kind at `warn`, a repeat at `debug`.
    fn log(&mut self, failure: Failure) {
        let repeated = self.failed == Some(failure);
        self.failed = Some(failure);
        let cause = failure.cause();
        let retry_secs = self.timings.renew_retry.as_secs();
        match (failure, repeated) {
            (Failure::Unimplemented, false) => warn!(cause, retry_secs, "{OLD_SERVER}"),
            (Failure::Unimplemented, true) => debug!(cause, retry_secs, "{OLD_SERVER}"),
            (_, false) => warn!(cause, retry_secs, "{FAILED}"),
            (_, true) => debug!(cause, retry_secs, "{FAILED}"),
        }
    }

    /// Says, at most once every [`REMIND_EVERY`], that the certificate ends
    /// soon and isn't renewed yet. The link calls it before each attempt to
    /// connect and after a failed renewal.
    pub fn remind(&mut self) {
        let now = self.clock.now();
        let Lifetime {
            not_before,
            not_after,
        } = self.lifetime;
        if expiry(not_before, not_after, now) != Expiry::Soon {
            return;
        }
        let due = self.reminded.is_none_or(|last| {
            now.signed_duration_since(last)
                .to_std()
                .is_ok_and(|since| since >= REMIND_EVERY)
        });
        if due {
            self.reminded = Some(now);
            warn!(
                %not_after,
                days_left = certificate_time_left(not_after, now).as_secs() / 86_400,
                "this agent's certificate ends soon and isn't renewed yet: it renews once it's connected to a server that renews (0.1.2 or later); otherwise enroll this agent again before then"
            );
        }
    }
}

/// What a server that doesn't know the call makes the agent log.
const OLD_SERVER: &str = "the server doesn't renew certificates yet: update the server; the agent keeps its certificate and asks again";

/// What any other failed renewal logs.
const FAILED: &str =
    "the certificate's renewal failed; the agent keeps its certificate and tries again";

/// Why an attempt renewed nothing. None holds the server's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// No answer within the deadline.
    TimedOut,
    /// The server doesn't know the call: it's older than the renewal.
    Unimplemented,
    /// Any other status.
    Status(Code),
    /// The answer isn't a valid message.
    Answer,
    /// The certificate failed enroll's check.
    Certificate(CertificateProblem),
    /// The checked certificate's dates can't be read.
    Unreadable,
    /// The endpoint with the new certificate can't be built.
    Endpoint,
    /// The certificate's file couldn't be replaced.
    Store,
}

impl Failure {
    /// Its name in a log line; a status by its code.
    fn cause(self) -> String {
        let fixed = match self {
            Self::Status(code) => return format!("status_{}", i32::from(code)),
            Self::TimedOut => "timed_out",
            Self::Unimplemented => "unimplemented",
            Self::Answer => "answer",
            Self::Certificate(CertificateProblem::NotOnePem) => "certificate_not_one_pem",
            Self::Certificate(CertificateProblem::NotFromTheCa) => "certificate_not_from_the_ca",
            Self::Certificate(CertificateProblem::NotOurKey) => "certificate_not_our_key",
            Self::Certificate(CertificateProblem::Expired { .. }) => "certificate_expired",
            Self::Certificate(CertificateProblem::NotYetValid) => "certificate_not_yet_valid",
            Self::Unreadable => "certificate_unreadable",
            Self::Endpoint => "endpoint",
            Self::Store => "store",
        };
        fixed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_has_its_own_name_without_any_message() {
        let now = DateTime::UNIX_EPOCH;
        let names = [
            Failure::TimedOut,
            Failure::Unimplemented,
            Failure::Status(Code::Unavailable),
            Failure::Status(Code::Unauthenticated),
            Failure::Answer,
            Failure::Certificate(CertificateProblem::NotOnePem),
            Failure::Certificate(CertificateProblem::NotFromTheCa),
            Failure::Certificate(CertificateProblem::NotOurKey),
            Failure::Certificate(CertificateProblem::Expired {
                not_after: now,
                now,
            }),
            Failure::Certificate(CertificateProblem::NotYetValid),
            Failure::Unreadable,
            Failure::Endpoint,
            Failure::Store,
        ]
        .map(Failure::cause);

        let distinct: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(distinct.len(), names.len(), "{names:?}");
        assert!(
            names
                .iter()
                .all(|name| !name.is_empty() && !name.contains(' ')),
            "{names:?}"
        );
        assert_eq!(Failure::Status(Code::Unavailable).cause(), "status_14");
    }

    #[test]
    fn a_failed_renewal_is_tried_again_after_an_hour_and_the_end_said_once_a_day() {
        assert_eq!(RENEW_RETRY, Duration::from_hours(1));
        assert_eq!(REMIND_EVERY, Duration::from_hours(24));
    }
}
