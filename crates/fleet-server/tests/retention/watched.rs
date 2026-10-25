//! A store that tells the retention's tests what its writes did, and lets
//! them step in: one log of what happened in order (each read of where it
//! cuts, each write or retention batch asked for, each delete with its
//! rows, each commit), the
//! time of each retention commit, a write that fails on cue, and a hold
//! before the next write, inside the next chat or audit cut's read or after
//! the next delete, until the test releases it.

use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::BotId;
use fleet_server::infra::sqlite::Database;
use fleet_server::ports::accounts::{AccountReads, AccountWrites};
use fleet_server::ports::agents::{AgentReads, AgentWrites};
use fleet_server::ports::audit::{AuditEntryId, AuditReads, AuditWrites, NewAuditEntry};
use fleet_server::ports::bots::{BotReads, BotWrites};
use fleet_server::ports::chat::{ChatMessageId, ChatReads, ChatWrites};
use fleet_server::ports::enrollment_tokens::{EnrollmentTokenReads, EnrollmentTokenWrites};
use fleet_server::ports::envelopes::{EnvelopeReads, EnvelopeWrites};
use fleet_server::ports::grants::{GrantReads, GrantWrites};
use fleet_server::ports::invites::{InviteReads, InviteWrites};
use fleet_server::ports::login_attempts::{LoginAttemptReads, LoginAttemptWrites};
use fleet_server::ports::mfa::{MfaReads, MfaWrites};
use fleet_server::ports::modes::{ModeReads, ModeWrites};
use fleet_server::ports::retention::{AuditCut, AuditPurged, RetentionBatch, RetentionReads};
use fleet_server::ports::sessions::{SessionReads, SessionWrites};
use fleet_server::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};
use fleet_server::ports::users::{UserReads, UserWrites};
use tokio::sync::{Notify, oneshot};
use tokio::time::{Instant, timeout};

/// How a write fails on cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The write connection stayed taken.
    Busy,
    /// The database failed.
    Backend,
}

impl Failure {
    fn error(self) -> StoreError {
        match self {
            Self::Busy => StoreError::Busy,
            Self::Backend => StoreError::Backend("the database failed on cue".into()),
        }
    }
}

/// What the store's tests set, shared with its transactions.
#[derive(Debug, Default)]
struct Hooks {
    log: Mutex<Vec<String>>,
    commits: Mutex<Vec<Instant>>,
    writes: AtomicU32,
    /// The writes that fail, counted from 1, and how.
    failing: Mutex<BTreeMap<u32, Failure>>,
    /// Whether the next audited commit fails, and how.
    failing_record: Mutex<Option<Failure>>,
    hold_chat_cut: Mutex<Option<Hold>>,
    hold_audit_cut: Mutex<Option<Hold>>,
    hold_write: Mutex<Option<Hold>>,
    hold_delete: Mutex<Option<(String, Hold)>>,
    requested: Notify,
}

/// A held step's side: tells the test it was reached, then waits.
#[derive(Debug)]
struct Hold {
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

/// The test's side of a hold.
#[derive(Debug)]
pub(crate) struct Held {
    reached: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
}

impl Held {
    /// Waits, within `bound`, until the held step was reached.
    pub(crate) async fn reached(&mut self, bound: Duration) {
        timeout(bound, &mut self.reached)
            .await
            .expect("the held step is reached in time")
            .unwrap();
    }

    /// Lets the held step go on.
    pub(crate) fn release(self) {
        let _ = self.release.send(());
    }
}

/// A new hold and its test side.
fn hold() -> (Hold, Held) {
    let (reached, reached_rx) = oneshot::channel();
    let (release_tx, release) = oneshot::channel();
    (
        Hold { reached, release },
        Held {
            reached: reached_rx,
            release: release_tx,
        },
    )
}

/// Waits on `slot`'s hold, if one is set.
async fn wait_if_held(slot: &Mutex<Option<Hold>>) {
    let held = slot.lock().unwrap().take();
    if let Some(Hold { reached, release }) = held {
        let _ = reached.send(());
        let _ = release.await;
    }
}

/// The database, its writes watched.
#[derive(Debug, Clone)]
pub(crate) struct WatchedStore {
    inner: Database,
    hooks: Arc<Hooks>,
}

impl WatchedStore {
    /// Watches `inner`.
    pub(crate) fn new(inner: Database) -> Self {
        Self {
            inner,
            hooks: Arc::default(),
        }
    }

    /// What happened so far, in order.
    pub(crate) fn log(&self) -> Vec<String> {
        self.hooks.log.lock().unwrap().clone()
    }

    /// When each retention batch committed.
    pub(crate) fn commit_times(&self) -> Vec<Instant> {
        self.hooks.commits.lock().unwrap().clone()
    }

    /// Makes the `nth` write asked for from now on (1 for the next) fail
    /// with `failure`.
    pub(crate) fn fail_write(&self, nth: u32, failure: Failure) {
        let at = self.hooks.writes.load(Ordering::SeqCst) + nth;
        self.hooks.failing.lock().unwrap().insert(at, failure);
    }

    /// Makes the next audited commit fail with `failure`, as a summary's
    /// would.
    pub(crate) fn fail_next_record(&self, failure: Failure) {
        *self.hooks.failing_record.lock().unwrap() = Some(failure);
    }

    /// Holds the next chat cut's read once it read the database, until the
    /// returned hold is released.
    pub(crate) fn hold_next_chat_cut(&self) -> Held {
        let (step, test) = hold();
        *self.hooks.hold_chat_cut.lock().unwrap() = Some(step);
        test
    }

    /// Holds the next write transaction asked for (never a retention
    /// batch) before it takes the connection, until the returned hold is
    /// released or dropped: a summary's write.
    pub(crate) fn hold_next_write(&self) -> Held {
        let (step, test) = hold();
        *self.hooks.hold_write.lock().unwrap() = Some(step);
        test
    }

    /// Holds the next audit cut's read once it read the database, until the
    /// returned hold is released.
    pub(crate) fn hold_next_audit_cut(&self) -> Held {
        let (step, test) = hold();
        *self.hooks.hold_audit_cut.lock().unwrap() = Some(step);
        test
    }

    /// Holds the next retention delete of `kind` (as the log names it) once
    /// it deleted, before its commit, until the returned hold is released.
    pub(crate) fn hold_next_delete(&self, kind: &str) -> Held {
        let (step, test) = hold();
        *self.hooks.hold_delete.lock().unwrap() = Some((kind.to_owned(), step));
        test
    }

    /// Waits, within `bound`, until a write is asked for.
    pub(crate) async fn write_requested(&self, bound: Duration) {
        timeout(bound, self.hooks.requested.notified())
            .await
            .expect("a write asked for in time");
    }
}

/// A write transaction of [`WatchedStore`].
struct WatchedTx {
    inner: Box<dyn WriteTx>,
    hooks: Arc<Hooks>,
}

/// A retention batch of [`WatchedStore`].
struct WatchedBatch {
    inner: Box<dyn RetentionBatch>,
    hooks: Arc<Hooks>,
}

/// Logs `kind`'s delete of `deleted` rows in `hooks`, then waits if it's
/// held. A free function: the transaction itself isn't `Sync`, so it can't
/// be borrowed across the wait.
async fn log_delete(
    hooks: &Hooks,
    kind: &str,
    deleted: Result<u64, StoreError>,
) -> Result<u64, StoreError> {
    if let Ok(rows) = &deleted {
        hooks
            .log
            .lock()
            .unwrap()
            .push(format!("delete {kind} {rows}"));
    }
    let held = {
        let mut slot = hooks.hold_delete.lock().unwrap();
        if slot.as_ref().is_some_and(|(held, _)| held == kind) {
            slot.take()
        } else {
            None
        }
    };
    if let Some((_, Hold { reached, release })) = held {
        let _ = reached.send(());
        let _ = release.await;
    }
    deleted
}

#[async_trait]
impl RetentionBatch for WatchedBatch {
    async fn delete_expired_rotations(
        &mut self,
        created_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError> {
        let deleted = self.inner.delete_expired_rotations(created_by, limit).await;
        log_delete(&self.hooks, "rotations", deleted).await
    }

    async fn delete_expired_sessions(
        &mut self,
        created_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError> {
        let deleted = self.inner.delete_expired_sessions(created_by, limit).await;
        log_delete(&self.hooks, "sessions", deleted).await
    }

    async fn delete_chat_received_by(
        &mut self,
        received_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError> {
        let deleted = self.inner.delete_chat_received_by(received_by, limit).await;
        log_delete(&self.hooks, "chat_by_age", deleted).await
    }

    async fn delete_chat_through(
        &mut self,
        bot: BotId,
        cut: ChatMessageId,
        limit: u32,
    ) -> Result<u64, StoreError> {
        let deleted = self.inner.delete_chat_through(bot, cut, limit).await;
        log_delete(&self.hooks, "chat_by_size", deleted).await
    }

    async fn delete_audit_through(
        &mut self,
        cut: AuditEntryId,
        limit: u32,
    ) -> Result<AuditPurged, StoreError> {
        match self.inner.delete_audit_through(cut, limit).await {
            Ok(purged) => log_delete(&self.hooks, "audit", Ok(purged.count))
                .await
                .map(|_| purged),
            Err(error) => log_delete(&self.hooks, "audit", Err(error))
                .await
                .map(|_| AuditPurged::default()),
        }
    }

    async fn delete_unused_invites(
        &mut self,
        expired_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError> {
        let deleted = self.inner.delete_unused_invites(expired_by, limit).await;
        log_delete(&self.hooks, "invites", deleted).await
    }

    async fn delete_unused_enrollment_tokens(
        &mut self,
        expired_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError> {
        let deleted = self
            .inner
            .delete_unused_enrollment_tokens(expired_by, limit)
            .await;
        log_delete(&self.hooks, "enrollment_tokens", deleted).await
    }

    async fn commit(self: Box<Self>) -> Result<(), StoreError> {
        self.inner.commit().await?;
        self.hooks.commits.lock().unwrap().push(Instant::now());
        self.hooks
            .log
            .lock()
            .unwrap()
            .push("commit retention batch".to_owned());
        Ok(())
    }
}

#[async_trait]
impl WriteTx for WatchedTx {
    fn users(&mut self) -> &mut dyn UserWrites {
        self.inner.users()
    }

    fn sessions(&mut self) -> &mut dyn SessionWrites {
        self.inner.sessions()
    }

    fn login_attempts(&mut self) -> &mut dyn LoginAttemptWrites {
        self.inner.login_attempts()
    }

    fn mfa(&mut self) -> &mut dyn MfaWrites {
        self.inner.mfa()
    }

    fn invites(&mut self) -> &mut dyn InviteWrites {
        self.inner.invites()
    }

    fn accounts(&mut self) -> &mut dyn AccountWrites {
        self.inner.accounts()
    }

    fn grants(&mut self) -> &mut dyn GrantWrites {
        self.inner.grants()
    }

    fn envelopes(&mut self) -> &mut dyn EnvelopeWrites {
        self.inner.envelopes()
    }

    fn enrollment_tokens(&mut self) -> &mut dyn EnrollmentTokenWrites {
        self.inner.enrollment_tokens()
    }

    fn agents(&mut self) -> &mut dyn AgentWrites {
        self.inner.agents()
    }

    fn bots(&mut self) -> &mut dyn BotWrites {
        self.inner.bots()
    }

    fn modes(&mut self) -> &mut dyn ModeWrites {
        self.inner.modes()
    }

    fn chat(&mut self) -> &mut dyn ChatWrites {
        self.inner.chat()
    }

    fn audit(&mut self) -> &mut dyn AuditWrites {
        self.inner.audit()
    }

    async fn commit(self: Box<Self>, entry: NewAuditEntry) -> Result<AuditEntryId, StoreError> {
        let failing = self.hooks.failing_record.lock().unwrap().take();
        if let Some(failure) = failing {
            return Err(failure.error());
        }
        let id = self.inner.commit(entry).await?;
        self.hooks
            .log
            .lock()
            .unwrap()
            .push("commit audited".to_owned());
        Ok(id)
    }

    async fn commit_unaudited(self: Box<Self>, reason: UnauditedWrite) -> Result<(), StoreError> {
        self.inner.commit_unaudited(reason).await?;
        self.hooks
            .log
            .lock()
            .unwrap()
            .push(format!("commit {reason:?}"));
        Ok(())
    }
}

#[async_trait]
impl RetentionReads for WatchedStore {
    async fn bots(&self) -> Result<Vec<BotId>, StoreError> {
        self.read("bots");
        self.inner.retention().bots().await
    }

    async fn chat_cut(&self, bot: BotId, keep: u32) -> Result<Option<ChatMessageId>, StoreError> {
        self.read("chat cut");
        let cut = self.inner.retention().chat_cut(bot, keep).await;
        wait_if_held(&self.hooks.hold_chat_cut).await;
        cut
    }

    async fn audit_cut(&self, before: DateTime<Utc>) -> Result<Option<AuditCut>, StoreError> {
        self.read("audit cut");
        let cut = self.inner.retention().audit_cut(before).await;
        wait_if_held(&self.hooks.hold_audit_cut).await;
        cut
    }
}

impl WatchedStore {
    /// Logs a read of where the retention cuts.
    fn read(&self, what: &str) {
        self.hooks.log.lock().unwrap().push(format!("read {what}"));
    }

    /// Counts and logs a write asked for, a retention batch's too, and
    /// fails it when it was set to.
    fn requested(&self) -> Result<(), StoreError> {
        let nth = self.hooks.writes.fetch_add(1, Ordering::SeqCst) + 1;
        self.hooks
            .log
            .lock()
            .unwrap()
            .push("write requested".to_owned());
        self.hooks.requested.notify_waiters();
        let failing = self.hooks.failing.lock().unwrap().remove(&nth);
        failing.map_or(Ok(()), |failure| Err(failure.error()))
    }
}

#[async_trait]
impl Store for WatchedStore {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        self.requested()?;
        wait_if_held(&self.hooks.hold_write).await;
        let inner = self.inner.write().await?;
        Ok(Box::new(WatchedTx {
            inner,
            hooks: Arc::clone(&self.hooks),
        }))
    }

    async fn retention_batch(&self) -> Result<Box<dyn RetentionBatch>, StoreError> {
        self.requested()?;
        let inner = self.inner.retention_batch().await?;
        Ok(Box::new(WatchedBatch {
            inner,
            hooks: Arc::clone(&self.hooks),
        }))
    }

    fn users(&self) -> &dyn UserReads {
        self.inner.users()
    }

    fn sessions(&self) -> &dyn SessionReads {
        self.inner.sessions()
    }

    fn login_attempts(&self) -> &dyn LoginAttemptReads {
        self.inner.login_attempts()
    }

    fn mfa(&self) -> &dyn MfaReads {
        self.inner.mfa()
    }

    fn invites(&self) -> &dyn InviteReads {
        self.inner.invites()
    }

    fn accounts(&self) -> &dyn AccountReads {
        self.inner.accounts()
    }

    fn grants(&self) -> &dyn GrantReads {
        self.inner.grants()
    }

    fn envelopes(&self) -> &dyn EnvelopeReads {
        self.inner.envelopes()
    }

    fn enrollment_tokens(&self) -> &dyn EnrollmentTokenReads {
        self.inner.enrollment_tokens()
    }

    fn agents(&self) -> &dyn AgentReads {
        self.inner.agents()
    }

    fn bots(&self) -> &dyn BotReads {
        self.inner.bots()
    }

    fn modes(&self) -> &dyn ModeReads {
        self.inner.modes()
    }

    fn chat(&self) -> &dyn ChatReads {
        self.inner.chat()
    }

    fn audit(&self) -> &dyn AuditReads {
        self.inner.audit()
    }

    fn retention(&self) -> &dyn RetentionReads {
        self
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }

    fn commits(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.commits()
    }
}
