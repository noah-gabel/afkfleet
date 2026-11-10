//! [`CertificateWatch`]: the warnings before a certificate of the control
//! plane ends (Plan.md P13.6; ADR-0027). The housekeeping runs it once a
//! minute.
//!
//! - **Agents' certificates.** Each run reads every enabled agent's latest
//!   unrevoked certificate and sets `afkfleet_agent_certificates` by state:
//!   `expiring` (fleet-core's [`Expiry::Soon`]: the last 14 days, or the
//!   last quarter of a shorter lifetime) and `expired`. An agent renews at
//!   two thirds of its certificate's lifetime, so one that's `expiring`
//!   didn't: it's offline, or older than the renewal. Once every
//!   [`REPORT_EVERY`], the first run included, each such agent gets a line
//!   with its ID, its name and the certificate's end: `warn` while it can
//!   still renew, `error` once it has expired and needs enrolling again.
//! - **The server's certificate,** which lasts a year and is renewed by
//!   hand: its end is `afkfleet_server_certificate_not_after_timestamp_seconds`
//!   from the start, and within [`SERVER_EXPIRY_WARNING`] of it a `warn`
//!   line says so once every [`REPORT_EVERY`]; an expired one is an `error`
//!   line, as at the start.
//! - **A store that can't be read** changes nothing: the counts stay, and
//!   the agents' lines come with the next run that reads it.

use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::agent::{Expiry, certificate_expired, certificate_time_left, expiry};
use fleet_core::system::Clock;
use tracing::{debug, error, warn};

use crate::http::error::chain;
use crate::metrics::ServerMetrics;
use crate::ports::agents::LatestCert;
use crate::ports::store::Store;

/// How often the certificates that end soon are named in the log.
pub const REPORT_EVERY: Duration = Duration::from_hours(24);

/// How long before the server certificate's end the warnings start
/// (ADR-0019, the user's note at Phase 10's plan review).
pub const SERVER_EXPIRY_WARNING: Duration = Duration::from_hours(30 * 24);

/// How near its end the server certificate, valid until `not_after`, is at
/// `now`: [`Expiry::Soon`] within [`SERVER_EXPIRY_WARNING`] of it.
#[must_use]
pub fn server_expiry(not_after: DateTime<Utc>, now: DateTime<Utc>) -> Expiry {
    if certificate_expired(not_after, now) {
        Expiry::Expired
    } else if certificate_time_left(not_after, now) < SERVER_EXPIRY_WARNING {
        Expiry::Soon
    } else {
        Expiry::Fine
    }
}

/// The whole days from `now` to `not_after`, for a log line.
fn days_left(not_after: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    certificate_time_left(not_after, now).as_secs() / 86_400
}

/// Logs the server certificate's state at `now`, when it ends soon or has
/// ended: the start's line and the watch's daily one.
pub fn log_server_certificate(not_after: DateTime<Utc>, now: DateTime<Utc>) {
    match server_expiry(not_after, now) {
        Expiry::Fine => {}
        Expiry::Soon => warn!(
            %not_after,
            days_left = days_left(not_after, now),
            "the server certificate expires soon: no agent can connect after that; renew it with `afkfleet-server pki server-cert` and restart the server"
        ),
        Expiry::Expired => error!(
            %not_after,
            "the server certificate has expired: no agent can connect until `afkfleet-server pki server-cert` replaces it"
        ),
    }
}

/// When each kind of line was last logged.
#[derive(Debug, Default, Clone, Copy)]
struct Reported {
    server: Option<DateTime<Utc>>,
    agents: Option<DateTime<Utc>>,
}

/// The certificates' watch; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct CertificateWatch {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    metrics: ServerMetrics,
    server_not_after: Option<DateTime<Utc>>,
    /// Held only to read or set a time, never across an `.await`.
    reported: Arc<Mutex<Reported>>,
}

impl CertificateWatch {
    /// The watch over `store`'s agents, by `clock`, counting in `metrics`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>, metrics: ServerMetrics) -> Self {
        Self {
            clock,
            store,
            metrics,
            server_not_after: None,
            reported: Arc::default(),
        }
    }

    /// The watch, with the server's own certificate, valid until
    /// `not_after`, which the metrics show from now on.
    #[must_use]
    pub fn with_server_certificate(self, not_after: DateTime<Utc>) -> Self {
        self.metrics.server_certificate(not_after);
        Self {
            server_not_after: Some(not_after),
            ..self
        }
    }

    /// One run; see the [module docs](self). The read is bounded by the
    /// read pool's acquire timeout.
    pub async fn run_once(&self) {
        let now = self.clock.now();
        if let Some(not_after) = self.server_not_after
            && self.due(now, |reported| &mut reported.server)
        {
            log_server_certificate(not_after, now);
        }
        let latest = match self.store.agents().latest_certs().await {
            Ok(latest) => latest,
            Err(failure) => {
                debug!(
                    error = %chain(&failure),
                    "the agents' certificates couldn't be read; the next run tries again"
                );
                return;
            }
        };
        let state = |cert: &LatestCert| expiry(cert.issued_at, cert.not_after, now);
        let count = |wanted| latest.iter().filter(|cert| state(cert) == wanted).count();
        self.metrics
            .agent_certificates(count(Expiry::Soon), count(Expiry::Expired));
        if !self.due(now, |reported| &mut reported.agents) {
            return;
        }
        for cert in &latest {
            match state(cert) {
                Expiry::Fine => {}
                Expiry::Soon => warn!(
                    agent_id = %cert.agent,
                    agent = %cert.name,
                    not_after = %cert.not_after,
                    days_left = days_left(cert.not_after, now),
                    "an agent's certificate expires soon and wasn't renewed: it renews once the agent connects with a version that renews (0.1.2 or later); otherwise enroll its host again before then"
                ),
                Expiry::Expired => error!(
                    agent_id = %cert.agent,
                    agent = %cert.name,
                    not_after = %cert.not_after,
                    "an agent's certificate has expired: it can't connect any more; disable it, enroll its host again and release its bots"
                ),
            }
        }
    }

    /// Whether the line `which` names is due at `now`: never logged, or
    /// [`REPORT_EVERY`] ago at least. It's marked as logged now.
    fn due(
        &self,
        now: DateTime<Utc>,
        which: impl FnOnce(&mut Reported) -> &mut Option<DateTime<Utc>>,
    ) -> bool {
        let mut reported = self.reported.lock().unwrap_or_else(PoisonError::into_inner);
        let last = which(&mut reported);
        let due = last.is_none_or(|last| {
            now.signed_duration_since(last)
                .to_std()
                .is_ok_and(|since| since >= REPORT_EVERY)
        });
        if due {
            *last = Some(now);
        }
        due
    }
}
