//! A store that tells the ingest's tests what its writes did, in one log
//! with what the marks did: when the write transaction was asked for, when
//! it opened, and when it committed (U16, U17).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleet_server::infra::sqlite::Database;
use fleet_server::ports::accounts::{AccountReads, AccountWrites};
use fleet_server::ports::agents::{AgentReads, AgentWrites};
use fleet_server::ports::audit::{AuditEntryId, AuditReads, AuditWrites, NewAuditEntry};
use fleet_server::ports::bots::{BotReads, BotWrites};
use fleet_server::ports::chat::{ChatReads, ChatWrites};
use fleet_server::ports::enrollment_tokens::{EnrollmentTokenReads, EnrollmentTokenWrites};
use fleet_server::ports::envelopes::{EnvelopeReads, EnvelopeWrites};
use fleet_server::ports::grants::{GrantReads, GrantWrites};
use fleet_server::ports::invites::{InviteReads, InviteWrites};
use fleet_server::ports::login_attempts::{LoginAttemptReads, LoginAttemptWrites};
use fleet_server::ports::mfa::{MfaReads, MfaWrites};
use fleet_server::ports::modes::{ModeReads, ModeWrites};
use fleet_server::ports::retention::{RetentionBatch, RetentionReads};
use fleet_server::ports::sessions::{SessionReads, SessionWrites};
use fleet_server::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};
use fleet_server::ports::users::{UserReads, UserWrites};
use tokio::sync::Notify;

/// What happened, in order.
pub(crate) type Log = Arc<Mutex<Vec<&'static str>>>;

/// The log's lines so far.
pub(crate) fn lines(log: &Log) -> Vec<&'static str> {
    log.lock().unwrap().clone()
}

/// The database, its writes logged.
#[derive(Debug)]
pub(crate) struct ObservedStore {
    pub(crate) inner: Database,
    pub(crate) log: Log,
    /// Told when a write transaction is asked for.
    pub(crate) requested: Arc<Notify>,
}

/// A write transaction whose commits are logged.
struct ObservedTx {
    inner: Box<dyn WriteTx>,
    log: Log,
}

#[async_trait]
impl WriteTx for ObservedTx {
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
        let id = self.inner.commit(entry).await?;
        self.log.lock().unwrap().push("committed");
        Ok(id)
    }

    async fn commit_unaudited(self: Box<Self>, reason: UnauditedWrite) -> Result<(), StoreError> {
        self.inner.commit_unaudited(reason).await?;
        self.log.lock().unwrap().push("committed");
        Ok(())
    }
}

#[async_trait]
impl Store for ObservedStore {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        self.log.lock().unwrap().push("write requested");
        self.requested.notify_one();
        let inner = self.inner.write().await?;
        self.log.lock().unwrap().push("write opened");
        Ok(Box::new(ObservedTx {
            inner,
            log: Arc::clone(&self.log),
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
        self.inner.retention()
    }

    async fn retention_batch(&self) -> Result<Box<dyn RetentionBatch>, StoreError> {
        self.inner.retention_batch().await
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }

    fn commits(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.commits()
    }
}
