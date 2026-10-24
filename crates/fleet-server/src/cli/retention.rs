//! The server's retention task (Plan.md P11.6; ADR-0020, S45, U20, R52,
//! R53): its own long-lived background task, which `serve` owns, with or
//! without the control plane, and stops at the first signal.
//!
//! Every [`RetentionTimings::interval`], the first time one interval after
//! the start, one run of [`RetentionService::run_once`]; a run that deleted
//! anything logs its counts at `debug`. After every
//! [`RetentionTimings::summary_every`]th run, the runs since the last entry
//! are recorded as one `retention.purge` entry when they deleted anything,
//! with one `info` line; a record that fails is a `warn`, and the next run
//! tries again with everything since. At the stop, what the runs since the
//! last entry deleted is recorded once more. It's not watched: a run that
//! fails is tried again a minute later, and an unasked end costs disk, not
//! service (S45). [`every`] holds the timing, tested on paused time with
//! the housekeeping.

use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::housekeeping::every;
use crate::app::retention::{Deleted, RetentionService, RetentionTimings, Tally};
use crate::http::error::chain;

/// The retention task: its service and its pace.
#[derive(Debug, Clone)]
pub struct Retention {
    service: RetentionService,
    timings: RetentionTimings,
}

impl Retention {
    /// The task over `service`, at `timings`' pace.
    #[must_use]
    pub const fn new(service: RetentionService, timings: RetentionTimings) -> Self {
        Self { service, timings }
    }

    /// The same task at `timings`' pace instead, for tests that can't wait
    /// a minute.
    #[must_use]
    pub fn with_timings(self, timings: RetentionTimings) -> Self {
        Self {
            service: self.service.with_pause(timings.pause),
            timings,
        }
    }

    /// Runs until `stop` is cancelled, then records what the runs since the
    /// last entry deleted; see the [module docs](self).
    pub async fn run(self, stop: CancellationToken) {
        let tally = Mutex::new(Tally::new(self.service.now()));
        every(self.timings.interval, &stop, || self.tick(&stop, &tally)).await;
        let tally = lock(&tally).clone();
        let deleted = tally.deleted();
        if !deleted.any() {
            return;
        }
        match self.service.record_at_stop(&tally).await {
            Ok(()) => info!(
                chat_by_age = deleted.chat_by_age,
                chat_by_size = deleted.chat_by_size,
                sessions = deleted.sessions,
                rotations = deleted.rotations,
                audit_entries = deleted.audit_entries,
                invites = deleted.invites,
                enrollments = deleted.enrollments,
                "the retention's summary was recorded at the stop"
            ),
            Err(error) => warn!(
                chat_by_age = deleted.chat_by_age,
                chat_by_size = deleted.chat_by_size,
                sessions = deleted.sessions,
                rotations = deleted.rotations,
                audit_entries = deleted.audit_entries,
                invites = deleted.invites,
                enrollments = deleted.enrollments,
                error = %chain(&error),
                "the retention's summary couldn't be recorded at the stop"
            ),
        }
    }

    /// One run, added to `tally`, which is recorded when it's due.
    async fn tick(&self, stop: &CancellationToken, tally: &Mutex<Tally>) {
        let run = self.service.run_once(stop).await;
        if run.deleted.any() {
            log_run(&run.deleted);
        }
        let due = {
            let mut tally = lock(tally);
            tally.add(&run);
            tally.due(self.timings.summary_every).then(|| tally.clone())
        };
        let Some(due) = due else {
            return;
        };
        let deleted = due.deleted();
        if !deleted.any() {
            lock(tally).passed();
            return;
        }
        match self.service.record(&due).await {
            Ok(()) => {
                info!(
                    chat_by_age = deleted.chat_by_age,
                    chat_by_size = deleted.chat_by_size,
                    sessions = deleted.sessions,
                    rotations = deleted.rotations,
                    audit_entries = deleted.audit_entries,
                    invites = deleted.invites,
                    enrollments = deleted.enrollments,
                    "the retention's summary was recorded"
                );
                lock(tally).recorded(self.service.now());
            }
            Err(error) => warn!(
                error = %chain(&error),
                "the retention's summary couldn't be recorded; the next run tries again"
            ),
        }
    }
}

/// A run's counts, at `debug`.
fn log_run(deleted: &Deleted) {
    debug!(
        chat_by_age = deleted.chat_by_age,
        chat_by_size = deleted.chat_by_size,
        sessions = deleted.sessions,
        rotations = deleted.rotations,
        audit_entries = deleted.audit_entries,
        invites = deleted.invites,
        enrollments = deleted.enrollments,
        "the retention deleted rows"
    );
}

/// The tally, held only between awaits; a panic while it was held can't
/// leave it half-written.
fn lock(tally: &Mutex<Tally>) -> MutexGuard<'_, Tally> {
    tally.lock().unwrap_or_else(PoisonError::into_inner)
}
