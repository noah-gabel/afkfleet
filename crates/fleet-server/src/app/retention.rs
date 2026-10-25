//! [`RetentionService`]: what the server's retention task deletes in one
//! run, and the hourly summary it records (Plan.md P11.6; ADR-0020,
//! U19–U22, S44–S46, R50–R54).
//!
//! **A run** ([`RetentionService::run_once`]) cuts at its start, from one
//! clock read (fleet-core's `retention`), then goes through its phases in
//! order:
//! 1. **sessions:** the replaced refresh tokens of sessions created at or
//!    before the cut, then those sessions, revoked or not (ADR-0016);
//! 2. **chat by age:** lines received at or before `[chat] retention_days`
//!    ago, users' lines among them;
//! 3. **chat by size:** for each bot, the lines past its newest
//!    `[chat] max_lines_per_bot`, the cut read before its deletes, so a line
//!    written meanwhile, newer, stays;
//! 4. **the audit log:** a prefix by ID, everything below the first entry
//!    dated after `[audit] retention_days` ago;
//! 5. **invites,** then **enrollment tokens:** unused ones that expired at
//!    or before the audit cut (S46).
//!
//! **A batch** deletes at most [`BATCH`] rows of one table in its own write
//! transaction, the store's retention batch, which can only delete and
//! commits without an audit entry (U20, R57), then pauses ([`RetentionTimings::pause`]), so the ingest's ticks and
//! logins get the one write connection between batches and the live events
//! sockets see a bounded rate of commits (S45). A batch that deleted nothing
//! rolls back, so a quiet run commits nothing and moves no commit count. The
//! retention publishes nothing on the bus (S42).
//!
//! **A failed phase** is one `warn` naming it and the rows it deleted so
//! far; the run goes on, and the next run tries again (S45). **The stop** is
//! checked before each phase, bot and batch, once the write connection is
//! taken, and during the pause; a batch already running finishes.
//!
//! **The summary** ([`Tally`], [`RetentionService::record`]): the task adds
//! each run to a tally and records one `retention.purge` entry after every
//! [`RetentionTimings::summary_every`]th run that deleted anything since the
//! last entry, and one at the stop, which waits at most [`STOP_WAIT`] for
//! the write connection (R53). The hourly one waits for the connection only
//! until the stop, so it never holds the stop up (R58). A record that fails
//! keeps the tally for the next try.
//!
//! **Logs carry counts only**, never a line's text or sender (R52).

use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome};
use fleet_core::auth::SessionPolicy;
use fleet_core::id::BotId;
use fleet_core::retention::{Cuts, RetentionPolicy};
use fleet_core::system::Clock;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::http::error::chain;
use crate::ports::audit::{AuditEntryId, NewAuditEntry};
use crate::ports::chat::ChatMessageId;
use crate::ports::retention::{AuditPurged, KeptEntry, RetentionBatch};
use crate::ports::store::{Store, StoreError};

/// The most rows one batch deletes, of one table, in its own transaction:
/// the lockout purge's size, short enough that the ingest's batch waits at
/// most one (ADR-0019, S74).
pub const BATCH: u32 = 1000;

/// How long the summary at the stop waits for the write connection (R53).
pub const STOP_WAIT: Duration = Duration::from_secs(1);

/// How far past the clock the first kept audit entry may be dated before
/// the retention warns that it holds the log's prefix up (R60): NTP's small
/// corrections stay below it.
pub const CLOCK_SLACK: TimeDelta = TimeDelta::hours(1);

/// The task's pace (R54): how often it runs, how long it pauses after each
/// committed batch, and after how many runs it sums up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionTimings {
    /// How often a run starts, the first one interval after the start.
    pub interval: Duration,
    /// The pause after each committed batch.
    pub pause: Duration,
    /// After how many runs the tally is recorded, when it holds deletions.
    pub summary_every: u32,
}

impl Default for RetentionTimings {
    /// S45's and U20's: a run every 60 s, a 100 ms pause, a summary an hour.
    fn default() -> Self {
        Self {
            interval: Duration::from_mins(1),
            pause: Duration::from_millis(100),
            summary_every: 60,
        }
    }
}

/// A run's phase, named in its `warn` when it fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Expired sessions, their replaced tokens first.
    Sessions,
    /// Chat older than its retention.
    ChatByAge,
    /// Chat past a bot's bound.
    ChatBySize,
    /// The audit log's old prefix.
    Audit,
    /// Unused invites past the audit retention.
    Invites,
    /// Unused enrollment tokens past the audit retention.
    EnrollmentTokens,
}

impl Phase {
    /// The phase's name in a log line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::ChatByAge => "chat_by_age",
            Self::ChatBySize => "chat_by_size",
            Self::Audit => "audit",
            Self::Invites => "invites",
            Self::EnrollmentTokens => "enrollment_tokens",
        }
    }
}

/// How many rows of each kind were deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Deleted {
    /// Chat lines older than their retention.
    pub chat_by_age: u64,
    /// Chat lines past their bot's bound.
    pub chat_by_size: u64,
    /// Sessions past their absolute lifetime.
    pub sessions: u64,
    /// Those sessions' replaced refresh tokens.
    pub rotations: u64,
    /// Audit entries.
    pub audit_entries: u64,
    /// Unused invites.
    pub invites: u64,
    /// Unused enrollment tokens.
    pub enrollments: u64,
}

impl Deleted {
    /// Whether anything was deleted.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.chat_by_age > 0
            || self.chat_by_size > 0
            || self.sessions > 0
            || self.rotations > 0
            || self.audit_entries > 0
            || self.invites > 0
            || self.enrollments > 0
    }

    /// Adds `other`'s counts.
    fn add(&mut self, other: &Self) {
        self.chat_by_age = self.chat_by_age.saturating_add(other.chat_by_age);
        self.chat_by_size = self.chat_by_size.saturating_add(other.chat_by_size);
        self.sessions = self.sessions.saturating_add(other.sessions);
        self.rotations = self.rotations.saturating_add(other.rotations);
        self.audit_entries = self.audit_entries.saturating_add(other.audit_entries);
        self.invites = self.invites.saturating_add(other.invites);
        self.enrollments = self.enrollments.saturating_add(other.enrollments);
    }
}

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Where it cut.
    pub cuts: Cuts,
    /// What it deleted.
    pub deleted: Deleted,
    /// The highest audit ID its committed batches deleted, which the log no
    /// longer holds; `None` when none did (R59).
    pub audit_through: Option<AuditEntryId>,
    /// The phases that failed, in order.
    pub failed: Vec<Phase>,
    /// Whether the stop ended it before its last phase.
    pub stopped: bool,
}

/// The runs since the last summary entry (U20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    since: DateTime<Utc>,
    runs: u32,
    deleted: Deleted,
    phases_failed: u64,
    cuts: Option<Cuts>,
    audit_through: Option<AuditEntryId>,
}

impl Tally {
    /// An empty tally, counting from `since`: the task's start, or the last
    /// entry.
    #[must_use]
    pub const fn new(since: DateTime<Utc>) -> Self {
        Self {
            since,
            runs: 0,
            deleted: Deleted {
                chat_by_age: 0,
                chat_by_size: 0,
                sessions: 0,
                rotations: 0,
                audit_entries: 0,
                invites: 0,
                enrollments: 0,
            },
            phases_failed: 0,
            cuts: None,
            audit_through: None,
        }
    }

    /// Adds `run`: its counts, its failures, its cuts as the latest.
    pub fn add(&mut self, run: &Run) {
        self.runs = self.runs.saturating_add(1);
        self.deleted.add(&run.deleted);
        let failed = u64::try_from(run.failed.len()).unwrap_or(u64::MAX);
        self.phases_failed = self.phases_failed.saturating_add(failed);
        self.cuts = Some(run.cuts);
        self.audit_through = run.audit_through.or(self.audit_through);
    }

    /// Whether `every` runs were added since the tally last restarted.
    #[must_use]
    pub const fn due(&self, every: u32) -> bool {
        self.runs >= every
    }

    /// What the runs deleted.
    #[must_use]
    pub const fn deleted(&self) -> Deleted {
        self.deleted
    }

    /// When the counted period began.
    #[must_use]
    pub const fn since(&self) -> DateTime<Utc> {
        self.since
    }

    /// After a recorded entry: empty again, counting from `now`.
    pub const fn recorded(&mut self, now: DateTime<Utc>) {
        *self = Self::new(now);
    }

    /// After a due tally with nothing to record: its runs count again from
    /// zero, its period still from the last entry.
    pub const fn passed(&mut self) {
        self.runs = 0;
    }
}

/// Why a summary wasn't recorded.
#[derive(Debug, thiserror::Error)]
pub enum SummaryError {
    /// Its metadata didn't build: a key or a value the audit rules refuse.
    #[error("the summary's metadata didn't build")]
    Metadata(#[from] AuditError),
    /// The write connection stayed taken past [`STOP_WAIT`] at the stop.
    #[error("the database stayed busy")]
    Busy,
    /// The stop came while the hourly summary waited for the write
    /// connection; the stop's summary records the tally instead.
    #[error("the server is stopping")]
    Stopped,
    /// The store failed.
    #[error("the summary couldn't be stored")]
    Store(#[from] StoreError),
}

/// One batch's delete.
#[derive(Debug, Clone, Copy)]
enum Delete {
    Rotations(DateTime<Utc>),
    Sessions(DateTime<Utc>),
    ChatByAge(DateTime<Utc>),
    ChatThrough(BotId, ChatMessageId),
    AuditThrough(AuditEntryId),
    Invites(DateTime<Utc>),
    EnrollmentTokens(DateTime<Utc>),
}

impl Delete {
    /// Deletes one batch of at most `limit` rows through `writes`: how many,
    /// and for the audit log the highest ID among them.
    async fn run(
        self,
        writes: &mut dyn RetentionBatch,
        limit: u32,
    ) -> Result<AuditPurged, StoreError> {
        let count = match self {
            Self::Rotations(cut) => writes.delete_expired_rotations(cut, limit).await,
            Self::Sessions(cut) => writes.delete_expired_sessions(cut, limit).await,
            Self::ChatByAge(cut) => writes.delete_chat_received_by(cut, limit).await,
            Self::ChatThrough(bot, cut) => writes.delete_chat_through(bot, cut, limit).await,
            Self::AuditThrough(cut) => return writes.delete_audit_through(cut, limit).await,
            Self::Invites(cut) => writes.delete_unused_invites(cut, limit).await,
            Self::EnrollmentTokens(cut) => writes.delete_unused_enrollment_tokens(cut, limit).await,
        };
        count.map(|count| AuditPurged {
            count,
            through: None,
        })
    }

    /// Where `deleted` rows of this kind are counted.
    const fn count(self, deleted: &mut Deleted) -> &mut u64 {
        match self {
            Self::Rotations(_) => &mut deleted.rotations,
            Self::Sessions(_) => &mut deleted.sessions,
            Self::ChatByAge(_) => &mut deleted.chat_by_age,
            Self::ChatThrough(..) => &mut deleted.chat_by_size,
            Self::AuditThrough(_) => &mut deleted.audit_entries,
            Self::Invites(_) => &mut deleted.invites,
            Self::EnrollmentTokens(_) => &mut deleted.enrollments,
        }
    }
}

/// How a delete's batches ended, and what the committed ones deleted.
struct Drained {
    /// The rows they deleted.
    deleted: u64,
    /// The highest audit ID they deleted.
    through: Option<AuditEntryId>,
    /// Why they ended: a batch came back short, the stop came, or a batch
    /// failed, the earlier ones staying.
    ended: Ended,
}

/// What the retention deletes, and where it reads what it cuts; see the
/// [module docs](self).
#[derive(Debug, Clone)]
pub struct RetentionService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    policy: RetentionPolicy,
    sessions: SessionPolicy,
    pause: Duration,
    /// The future-dated audit entry last warned about, so each is warned
    /// about once (R60).
    warned: Arc<Mutex<Option<AuditEntryId>>>,
}

impl RetentionService {
    /// The retention of `store` by `policy`, sessions by `sessions`'
    /// absolute lifetime, cut from `clock`'s time, pausing as
    /// [`RetentionTimings::default`] after each committed batch. The task
    /// sets its own timings' pause (R63).
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        store: Arc<dyn Store>,
        policy: RetentionPolicy,
        sessions: SessionPolicy,
    ) -> Self {
        Self {
            clock,
            store,
            policy,
            sessions,
            pause: RetentionTimings::default().pause,
            warned: Arc::default(),
        }
    }

    /// The same service, pausing `pause` after each committed batch instead.
    #[must_use]
    pub fn with_pause(self, pause: Duration) -> Self {
        Self { pause, ..self }
    }

    /// What it keeps.
    #[must_use]
    pub const fn policy(&self) -> RetentionPolicy {
        self.policy
    }

    /// The sessions' lifetimes it cuts sessions by.
    #[must_use]
    pub const fn sessions(&self) -> SessionPolicy {
        self.sessions
    }

    /// The pause after each committed batch.
    #[must_use]
    pub const fn pause(&self) -> Duration {
        self.pause
    }

    /// The clock's time now.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// One run; see the [module docs](self).
    pub async fn run_once(&self, stop: &CancellationToken) -> Run {
        let now = self.clock.now();
        let cuts = self.policy.cuts(now, &self.sessions);
        let mut run = Run {
            cuts,
            deleted: Deleted::default(),
            audit_through: None,
            failed: Vec::new(),
            stopped: false,
        };
        let phases = [
            Phase::Sessions,
            Phase::ChatByAge,
            Phase::ChatBySize,
            Phase::Audit,
            Phase::Invites,
            Phase::EnrollmentTokens,
        ];
        for phase in phases {
            if stop.is_cancelled() {
                run.stopped = true;
                break;
            }
            let ended = match phase {
                Phase::Sessions => self.sessions_phase(stop, &mut run).await,
                Phase::ChatByAge => {
                    self.drain(stop, &mut run, Delete::ChatByAge(cuts.chat))
                        .await
                }
                Phase::ChatBySize => self.size_phase(stop, &mut run).await,
                Phase::Audit => self.audit_phase(stop, &mut run, now).await,
                Phase::Invites => {
                    self.drain(stop, &mut run, Delete::Invites(cuts.audit))
                        .await
                }
                Phase::EnrollmentTokens => {
                    self.drain(stop, &mut run, Delete::EnrollmentTokens(cuts.audit))
                        .await
                }
            };
            match ended {
                Ended::Done => {}
                Ended::Stopped => {
                    run.stopped = true;
                    break;
                }
                Ended::Failed(error) => {
                    warn!(
                        phase = phase.name(),
                        deleted = deleted_in(phase, &run.deleted),
                        error = %chain(&error),
                        "a retention phase failed; the next run tries again"
                    );
                    run.failed.push(phase);
                }
            }
        }
        run
    }

    /// Expired sessions: their replaced tokens first, so no session's
    /// cascade makes a batch long; the sessions only once those are gone.
    async fn sessions_phase(&self, stop: &CancellationToken, run: &mut Run) -> Ended {
        let cut = run.cuts.sessions;
        match self.drain(stop, run, Delete::Rotations(cut)).await {
            Ended::Done => self.drain(stop, run, Delete::Sessions(cut)).await,
            ended => ended,
        }
    }

    /// Each bot's lines past its bound, its cut read before its deletes.
    async fn size_phase(&self, stop: &CancellationToken, run: &mut Run) -> Ended {
        let bots = match self.store.retention().bots().await {
            Ok(bots) => bots,
            Err(error) => return Ended::Failed(error),
        };
        let keep = self.policy.bound.lines();
        for bot in bots {
            if stop.is_cancelled() {
                return Ended::Stopped;
            }
            let cut = match self.store.retention().chat_cut(bot, keep).await {
                Ok(Some(cut)) => cut,
                Ok(None) => continue,
                Err(error) => return Ended::Failed(error),
            };
            match self.drain(stop, run, Delete::ChatThrough(bot, cut)).await {
                Ended::Done => {}
                ended => return ended,
            }
        }
        Ended::Done
    }

    /// The audit log's prefix below the first entry dated after the cut,
    /// with a `warn` when that entry is dated past `now` (R60).
    async fn audit_phase(
        &self,
        stop: &CancellationToken,
        run: &mut Run,
        now: DateTime<Utc>,
    ) -> Ended {
        match self.store.retention().audit_cut(run.cuts.audit).await {
            Ok(Some(cut)) => {
                if let Some(kept) = cut.first_kept {
                    self.warn_if_ahead(kept, now);
                }
                self.drain(stop, run, Delete::AuditThrough(cut.through))
                    .await
            }
            Ok(None) => Ended::Done,
            Err(error) => Ended::Failed(error),
        }
    }

    /// One `warn` when `kept`, where the audit log's prefix stops, is dated
    /// more than [`CLOCK_SLACK`] after `now`: a clock set back after it was
    /// written. The log can't shrink past it until then, so `[audit]
    /// retention_days`' bound doesn't hold meanwhile. Once per entry.
    fn warn_if_ahead(&self, kept: KeptEntry, now: DateTime<Utc>) {
        let ahead = now
            .checked_add_signed(CLOCK_SLACK)
            .is_some_and(|limit| kept.at > limit);
        if !ahead {
            return;
        }
        {
            let mut warned = self.warned.lock().unwrap_or_else(PoisonError::into_inner);
            if *warned == Some(kept.id) {
                return;
            }
            *warned = Some(kept.id);
        }
        warn!(
            audit_entry = kept.id.get(),
            dated = %kept.at,
            "an audit entry is dated in the future: the audit log keeps it, and every entry after it, until then; check the server's clock"
        );
    }

    /// `delete`'s batches until one comes back short, each committed and
    /// followed by the pause, its rows counted in `run`.
    async fn drain(&self, stop: &CancellationToken, run: &mut Run, delete: Delete) -> Ended {
        let drained = self.batches(stop, delete).await;
        let count = delete.count(&mut run.deleted);
        *count = count.saturating_add(drained.deleted);
        if drained.through.is_some() {
            run.audit_through = drained.through;
        }
        drained.ended
    }

    /// `delete`'s batches; see [`Self::drain`].
    async fn batches(&self, stop: &CancellationToken, delete: Delete) -> Drained {
        let mut drained = Drained {
            deleted: 0,
            through: None,
            ended: Ended::Done,
        };
        let ended = loop {
            if stop.is_cancelled() {
                break Ended::Stopped;
            }
            let mut batch = match self.store.retention_batch().await {
                Ok(batch) => batch,
                Err(error) => break Ended::Failed(error),
            };
            if stop.is_cancelled() {
                // Dropping the batch rolls it back: the shutdown's flush
                // never waits behind a batch that started after it.
                break Ended::Stopped;
            }
            let purged = match delete.run(batch.as_mut(), BATCH).await {
                Ok(purged) => purged,
                Err(error) => break Ended::Failed(error),
            };
            if purged.count == 0 {
                // Nothing to commit: dropped, it moves no commit count.
                break Ended::Done;
            }
            if let Err(error) = batch.commit().await {
                break Ended::Failed(error);
            }
            // Counted once committed: a batch that failed deleted nothing.
            drained.deleted = drained.deleted.saturating_add(purged.count);
            drained.through = purged.through.or(drained.through);
            tokio::select! {
                biased;
                // Both are cancel-safe: waiting loses nothing.
                () = stop.cancelled() => break Ended::Stopped,
                () = sleep(self.pause) => {}
            }
            if purged.count < u64::from(BATCH) {
                break Ended::Done;
            }
        };
        drained.ended = ended;
        drained
    }

    /// Records `tally` as one `retention.purge` entry, in its own short
    /// transaction (U20), waiting for the write connection until `stop`
    /// (R58): the stop's own summary records the tally then.
    ///
    /// # Errors
    /// [`SummaryError::Stopped`] when the stop came first,
    /// [`SummaryError::Metadata`] or [`SummaryError::Store`].
    pub async fn record(
        &self,
        tally: &Tally,
        stop: &CancellationToken,
    ) -> Result<(), SummaryError> {
        self.record_within(tally, Wait::UntilStop(stop)).await
    }

    /// Records `tally` at the stop, waiting at most [`STOP_WAIT`] for the
    /// write connection (R53).
    ///
    /// # Errors
    /// [`SummaryError::Busy`] when the wait ran out,
    /// [`SummaryError::Metadata`] or [`SummaryError::Store`].
    pub async fn record_at_stop(&self, tally: &Tally) -> Result<(), SummaryError> {
        self.record_within(tally, Wait::AtMost(STOP_WAIT)).await
    }

    /// Records `tally`, waiting for the write connection as `wait` says. A
    /// commit under way is never cut, so no summary is recorded twice.
    async fn record_within(&self, tally: &Tally, wait: Wait<'_>) -> Result<(), SummaryError> {
        let entry = self.summary(tally)?;
        let tx = match wait {
            Wait::UntilStop(stop) => tokio::select! {
                biased;
                // Cancel-safe: dropped while it waits for the connection, it
                // has sent nothing, and the tally is kept.
                () = stop.cancelled() => return Err(SummaryError::Stopped),
                tx = self.store.write() => tx?,
            },
            Wait::AtMost(bound) => timeout(bound, self.store.write())
                .await
                .map_err(|_elapsed| SummaryError::Busy)??,
        };
        tx.commit(entry).await?;
        Ok(())
    }

    /// `tally`'s entry, at the clock's time now.
    fn summary(&self, tally: &Tally) -> Result<NewAuditEntry, AuditError> {
        summary(tally, self.clock.now(), &self.policy, &self.sessions)
    }
}

/// `tally`'s entry at `now`: every key always there, every value the
/// server's. Without a run in it, the cuts are `policy`'s at `now`.
fn summary(
    tally: &Tally,
    now: DateTime<Utc>,
    policy: &RetentionPolicy,
    sessions: &SessionPolicy,
) -> Result<NewAuditEntry, AuditError> {
    // At most the largest integer the audit keeps: one past it would be
    // refused, and the kept tally refused again at every try.
    let count =
        |n: u64| i64::try_from(n).map_or(AuditMetadata::MAX_INT, |n| n.min(AuditMetadata::MAX_INT));
    let cuts = tally.cuts.unwrap_or_else(|| policy.cuts(now, sessions));
    let deleted = tally.deleted;
    let metadata = AuditMetadata::new()
        .time("since", tally.since)?
        .time("chat_before", cuts.chat)?
        .int("chat_by_age", count(deleted.chat_by_age))?
        .int("chat_per_bot", i64::from(policy.bound.lines()))?
        .int("chat_by_size", count(deleted.chat_by_size))?
        .time("sessions_before", cuts.sessions)?
        .int("sessions", count(deleted.sessions))?
        .int("rotations", count(deleted.rotations))?
        .time("audit_before", cuts.audit)?
        .int("audit_entries", count(deleted.audit_entries))?
        .int(
            "audit_through",
            tally.audit_through.map_or(0, AuditEntryId::get),
        )?
        .int("invites", count(deleted.invites))?
        .int("enrollments", count(deleted.enrollments))?
        .int("phases_failed", count(tally.phases_failed))?;
    Ok(
        NewAuditEntry::new(now, AuditAction::RETENTION_PURGE, AuditOutcome::Success)
            .metadata(metadata),
    )
}

/// How long a summary waits for the write connection.
#[derive(Debug, Clone, Copy)]
enum Wait<'a> {
    /// Until the stop: the hourly summary.
    UntilStop(&'a CancellationToken),
    /// At most this long: the stop's summary.
    AtMost(Duration),
}

/// How a phase ended.
enum Ended {
    Done,
    Stopped,
    Failed(StoreError),
}

/// The rows `phase` deleted so far, for its failure's line.
const fn deleted_in(phase: Phase, deleted: &Deleted) -> u64 {
    match phase {
        Phase::Sessions => deleted.sessions.saturating_add(deleted.rotations),
        Phase::ChatByAge => deleted.chat_by_age,
        Phase::ChatBySize => deleted.chat_by_size,
        Phase::Audit => deleted.audit_entries,
        Phase::Invites => deleted.invites,
        Phase::EnrollmentTokens => deleted.enrollments,
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::audit::MetadataValue;

    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    /// A run at `when` that deleted `chat` lines by age, with `failed`
    /// phases.
    fn run(when: DateTime<Utc>, chat: u64, failed: Vec<Phase>) -> Run {
        Run {
            cuts: Cuts {
                chat: when,
                sessions: when,
                audit: when,
            },
            deleted: Deleted {
                chat_by_age: chat,
                ..Deleted::default()
            },
            audit_through: (chat > 0).then(|| AuditEntryId::new(i64::try_from(chat).unwrap())),
            failed,
            stopped: false,
        }
    }

    #[test]
    fn a_tally_sums_its_runs_and_is_due_after_its_count() {
        let start = at("2027-05-01T00:00:00Z");
        let mut tally = Tally::new(start);

        tally.add(&run(at("2027-05-01T00:01:00Z"), 3, vec![Phase::Audit]));
        let after_one = tally.due(2);
        tally.add(&run(at("2027-05-01T00:02:00Z"), 0, vec![Phase::Invites]));

        assert!(!after_one);
        assert!(tally.due(2));
        assert_eq!(tally.deleted().chat_by_age, 3);
        assert_eq!(tally.phases_failed, 2);
        assert_eq!(
            tally.cuts.map(|cuts| cuts.chat),
            Some(at("2027-05-01T00:02:00Z")),
            "the latest run's cuts"
        );
        assert_eq!(
            tally.audit_through,
            Some(AuditEntryId::new(3)),
            "kept past a run that read none"
        );
        assert_eq!(tally.since(), start);
    }

    #[test]
    fn a_recorded_tally_restarts_and_a_passed_one_keeps_its_period() {
        let start = at("2027-05-01T00:00:00Z");
        let mut passed = Tally::new(start);
        passed.add(&run(start, 0, Vec::new()));
        let mut recorded = passed.clone();
        recorded.add(&run(start, 4, Vec::new()));

        passed.passed();
        recorded.recorded(at("2027-05-01T01:00:00Z"));

        assert!(!passed.due(1));
        assert_eq!(passed.since(), start);
        assert_eq!(recorded, Tally::new(at("2027-05-01T01:00:00Z")));
    }

    /// Counts past 2^53 − 1, the largest integer the audit keeps, are
    /// recorded as that largest one: a refused summary would be kept and
    /// refused again at every try.
    #[test]
    fn a_summary_records_counts_past_the_safe_range_as_the_largest_it_keeps() {
        let when = at("2027-05-01T00:00:00Z");
        let mut tally = Tally::new(when);
        let mut huge = run(when, 0, Vec::new());
        huge.deleted = Deleted {
            chat_by_age: u64::MAX,
            chat_by_size: u64::MAX,
            sessions: u64::MAX,
            rotations: u64::MAX,
            audit_entries: u64::MAX,
            invites: u64::MAX,
            enrollments: u64::MAX,
        };
        tally.add(&huge);

        let entry = summary(
            &tally,
            when,
            &RetentionPolicy::default(),
            &SessionPolicy::default(),
        )
        .unwrap();

        let counts: Vec<(&str, &MetadataValue)> = entry
            .metadata
            .iter()
            .filter(|(key, _)| {
                [
                    "chat_by_age",
                    "chat_by_size",
                    "sessions",
                    "rotations",
                    "audit_entries",
                    "invites",
                    "enrollments",
                ]
                .contains(key)
            })
            .collect();
        assert_eq!(counts.len(), 7);
        assert!(
            counts
                .iter()
                .all(|(_, value)| **value == MetadataValue::Int(AuditMetadata::MAX_INT)),
            "{counts:?}"
        );
    }

    /// S45's and U20's pace: a run a minute, a 100 ms pause after each
    /// committed batch, and a summary after 60 runs, an hour.
    #[test]
    fn the_default_pace_is_a_run_a_minute_a_short_pause_and_a_summary_an_hour() {
        assert_eq!(
            RetentionTimings::default(),
            RetentionTimings {
                interval: Duration::from_secs(60),
                pause: Duration::from_millis(100),
                summary_every: 60,
            }
        );
    }

    #[test]
    fn each_phase_has_its_name() {
        let names = [
            Phase::Sessions,
            Phase::ChatByAge,
            Phase::ChatBySize,
            Phase::Audit,
            Phase::Invites,
            Phase::EnrollmentTokens,
        ]
        .map(Phase::name);

        assert_eq!(
            names,
            [
                "sessions",
                "chat_by_age",
                "chat_by_size",
                "audit",
                "invites",
                "enrollment_tokens"
            ]
        );
    }
}
