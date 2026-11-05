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
//!
//! **Backups** (P12.3; ADR-0022, S3, S6): with `[database] backup_dir`, each
//! tick ends with [`Retention::back_up_when_due`], after the run and its
//! summary, so a backup's read snapshot never overlaps a retention batch and
//! each backup follows a finished run. One is due when the newest backup's
//! name is an interval old; a stop ends a running one and keeps nothing of
//! it; a failure is an `error` line, and the next try comes after
//! [`RETRY_AFTER`].

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::housekeeping::every;
use crate::app::backup::{BackupService, RETRY_AFTER};
use crate::app::retention::{Deleted, RetentionService, RetentionTimings, SummaryError, Tally};
use crate::http::error::chain;
use crate::infra::sqlite::backup::BackupError;

/// What one tick's backup step did (ADR-0022, S3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStep {
    /// Backups are off: no `[database] backup_dir`.
    Off,
    /// The newest backup is younger than the interval.
    NotDue,
    /// A backup failed less than [`RETRY_AFTER`] ago.
    Waiting,
    /// A backup was taken.
    Taken,
    /// The backup, or the look at its folder, failed.
    Failed,
    /// The stop ended the backup; nothing of it was kept.
    Stopped,
}

/// The retention task: its service, its pace, and the backups it takes.
#[derive(Debug, Clone)]
pub struct Retention {
    service: RetentionService,
    timings: RetentionTimings,
    backups: Option<Arc<BackupService>>,
    /// When the next try may come after a failed backup.
    retry_at: Arc<Mutex<Option<DateTime<Utc>>>>,
}

impl Retention {
    /// The task over `service`, at `timings`' pace, its pause too: the
    /// timings are the one source (R63).
    #[must_use]
    pub fn new(service: RetentionService, timings: RetentionTimings) -> Self {
        Self {
            service: service.with_pause(timings.pause),
            timings,
            backups: None,
            retry_at: Arc::new(Mutex::new(None)),
        }
    }

    /// The same task at `timings`' pace instead, for tests that can't wait
    /// a minute.
    #[must_use]
    pub fn with_timings(self, timings: RetentionTimings) -> Self {
        Self {
            backups: self.backups,
            ..Self::new(self.service, timings)
        }
    }

    /// The same task, taking `backups` after its runs (none for `None`).
    #[must_use]
    pub fn with_backups(self, backups: Option<BackupService>) -> Self {
        Self {
            backups: backups.map(Arc::new),
            ..self
        }
    }

    /// Its service.
    #[must_use]
    pub const fn service(&self) -> &RetentionService {
        &self.service
    }

    /// Its pace.
    #[must_use]
    pub const fn timings(&self) -> RetentionTimings {
        self.timings
    }

    /// The backups it takes, if any.
    #[must_use]
    pub fn backups(&self) -> Option<&BackupService> {
        self.backups.as_deref()
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

    /// One run and its summary, then the backup if it's due.
    async fn tick(&self, stop: &CancellationToken, tally: &Mutex<Tally>) {
        self.run_and_sum_up(stop, tally).await;
        if !stop.is_cancelled() {
            self.back_up_when_due(stop).await;
        }
    }

    /// Takes a backup when one is due and the last failure is
    /// [`RETRY_AFTER`] old; see the [module docs](self). It runs inside the
    /// task's tick, after the retention's run.
    pub async fn back_up_when_due(&self, stop: &CancellationToken) -> BackupStep {
        let Some(backups) = &self.backups else {
            return BackupStep::Off;
        };
        let now = self.service.now();
        if lock(&self.retry_at).is_some_and(|after| now < after) {
            return BackupStep::Waiting;
        }
        let retry_later = || {
            *lock(&self.retry_at) = now.checked_add_signed(RETRY_AFTER);
            BackupStep::Failed
        };
        match backups.is_due().await {
            Ok(true) => {}
            Ok(false) => return BackupStep::NotDue,
            Err(failure) => {
                error!(
                    error = %chain(&failure),
                    "the backups' folder couldn't be read; the next try is in an hour"
                );
                return retry_later();
            }
        }
        match backups.take(stop).await {
            Ok(_) => {
                *lock(&self.retry_at) = None;
                BackupStep::Taken
            }
            Err(BackupError::Stopped { file }) if stop.is_cancelled() => {
                info!(
                    file = %file.display(),
                    "the stop ended a backup; nothing of it was kept"
                );
                BackupStep::Stopped
            }
            Err(failure) => {
                error!(
                    error = %chain(&failure),
                    "a backup failed; the next try is in an hour"
                );
                retry_later()
            }
        }
    }

    /// One run, added to `tally`, which is recorded when it's due.
    async fn run_and_sum_up(&self, stop: &CancellationToken, tally: &Mutex<Tally>) {
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
        match self.service.record(&due, stop).await {
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
            // The stop came first: its own summary records the tally.
            Err(SummaryError::Stopped) => {}
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

/// A value held only between awaits; a panic while it was held can't leave
/// it half-written.
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}
