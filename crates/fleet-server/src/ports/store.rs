//! [`Store`] and [`WriteTx`]: the database as a unit of work, and
//! [`StoreError`], how it fails.

use core::fmt;

use async_trait::async_trait;

use super::accounts::{AccountReads, AccountWrites};
use super::agents::{AgentReads, AgentWrites};
use super::audit::{AuditEntryId, AuditReads, AuditWrites, NewAuditEntry};
use super::envelopes::{EnvelopeReads, EnvelopeWrites};
use super::grants::{GrantReads, GrantWrites};
use super::invites::{InviteReads, InviteWrites};
use super::login_attempts::{LoginAttemptReads, LoginAttemptWrites};
use super::mfa::{MfaReads, MfaWrites};
use super::sessions::{SessionReads, SessionWrites};
use super::users::{UserReads, UserWrites};

/// Why the database couldn't do what was asked.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// No connection was free within the acquire timeout, or SQLite stayed
    /// locked: the caller gets an explicit error instead of waiting.
    #[error("the database is busy")]
    Busy,
    /// A stored value isn't valid. The error names where it is and never
    /// carries the value, which could be a password hash.
    #[error("{table}.{column} of row {rowid} holds an invalid value")]
    Corrupt {
        /// The table.
        table: &'static str,
        /// The column.
        column: &'static str,
        /// SQLite's rowid of the row, which every table has.
        rowid: i64,
    },
    /// A value can't be stored, such as a time outside the years 0 to 9999.
    #[error("{table}.{column} can't store this value")]
    Unstorable {
        /// The table.
        table: &'static str,
        /// The column.
        column: &'static str,
    },
    /// The database failed. The source is the database's own error, for the
    /// log; clients only ever see a generic message.
    #[error("the database failed")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The database: read handles over the read connections, and one write
/// transaction at a time.
#[async_trait]
pub trait Store: Send + Sync + fmt::Debug {
    /// Starts a write transaction on the one write connection. It takes the
    /// write lock at once (`BEGIN IMMEDIATE`), so a transaction that reads
    /// first and writes later can't fail halfway because another process
    /// wrote in between.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when the write connection stays taken for the
    /// acquire timeout, or [`StoreError::Backend`].
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError>;

    /// The users, read through the read connections.
    fn users(&self) -> &dyn UserReads;

    /// The sessions, read through the read connections.
    fn sessions(&self) -> &dyn SessionReads;

    /// The failed logins per username, read through the read connections.
    fn login_attempts(&self) -> &dyn LoginAttemptReads;

    /// The users' second factors, read through the read connections.
    fn mfa(&self) -> &dyn MfaReads;

    /// The invites, read through the read connections.
    fn invites(&self) -> &dyn InviteReads;

    /// The linked Minecraft accounts, read through the read connections.
    fn accounts(&self) -> &dyn AccountReads;

    /// The grants that share accounts, read through the read connections.
    fn grants(&self) -> &dyn GrantReads;

    /// Every stored vault envelope, read through the read connections.
    fn envelopes(&self) -> &dyn EnvelopeReads;

    /// The agents and their certificates, read through the read
    /// connections.
    fn agents(&self) -> &dyn AgentReads;

    /// The audit log, read through the read connections.
    fn audit(&self) -> &dyn AuditReads;

    /// Checks that the database answers: one trivial query through a read
    /// connection, for the readiness check (P6.7). It never competes with the
    /// one write connection.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when no read connection is free within the
    /// acquire timeout, or [`StoreError::Backend`] when the database fails.
    async fn ping(&self) -> Result<(), StoreError>;
}

/// One write transaction. Dropping it without [`WriteTx::commit`] rolls
/// everything back.
///
/// **Keep it short.** There's only one write connection, so nothing slow
/// that isn't database work happens while a `WriteTx` is open: no password
/// hashing, no crypto, no network or agent calls. Do that work before
/// [`Store::write`].
///
/// **Every commit is audited.** [`WriteTx::commit`] takes the entry that
/// describes the change, so a change can't be committed without one. The
/// only exceptions are the named [`UnauditedWrite`]s, committed with
/// [`WriteTx::commit_unaudited`].
#[async_trait]
pub trait WriteTx: Send {
    /// The users, written in this transaction.
    fn users(&mut self) -> &mut dyn UserWrites;

    /// The sessions, read and written in this transaction.
    fn sessions(&mut self) -> &mut dyn SessionWrites;

    /// The failed logins per username, read and written in this
    /// transaction.
    fn login_attempts(&mut self) -> &mut dyn LoginAttemptWrites;

    /// The users' second factors, read and written in this transaction.
    fn mfa(&mut self) -> &mut dyn MfaWrites;

    /// The invites, read and written in this transaction.
    fn invites(&mut self) -> &mut dyn InviteWrites;

    /// The linked Minecraft accounts, read and written in this transaction.
    fn accounts(&mut self) -> &mut dyn AccountWrites;

    /// The grants that share accounts, read and written in this
    /// transaction.
    fn grants(&mut self) -> &mut dyn GrantWrites;

    /// The stored vault envelopes, changed in this transaction.
    fn envelopes(&mut self) -> &mut dyn EnvelopeWrites;

    /// The agents and their certificates, written in this transaction.
    fn agents(&mut self) -> &mut dyn AgentWrites;

    /// The audit log, for a second entry in this transaction.
    fn audit(&mut self) -> &mut dyn AuditWrites;

    /// Records `entry`, then commits everything.
    ///
    /// # Errors
    /// [`StoreError`] when the entry can't be recorded or the commit fails;
    /// nothing is committed then.
    async fn commit(self: Box<Self>, entry: NewAuditEntry) -> Result<AuditEntryId, StoreError>;

    /// Commits everything without an audit entry, for one of the named
    /// exemptions only.
    ///
    /// # Errors
    /// [`StoreError`] when the commit fails; nothing is committed then.
    async fn commit_unaudited(self: Box<Self>, reason: UnauditedWrite) -> Result<(), StoreError>;
}

/// A write that commits without an audit entry. Each variant is a named
/// exemption with its own ADR note (ADR-0015, ADR-0016), never free text, so
/// review sees every new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnauditedWrite {
    /// A session's `last_used_at`, written by an authenticated request at
    /// most once a minute. It's for display only (expiry counts from the
    /// last refresh), and an entry per request would bury the log.
    SessionTouch,
    /// A successful refresh: the session's new token pair. It happens about
    /// every 15 minutes per active client and says nothing new: the
    /// session's creation and its end (logout, reuse, disabling, a role
    /// change) are audited.
    SessionRotate,
    /// One batch of the purge of expired login failure streaks (at most
    /// 1000 rows), run by the server's periodic housekeeping. It deletes
    /// only rows the lockout rule already treats as absent (their last
    /// failure is 24 h old and no lock runs), so an entry per batch would say
    /// nothing; no client asks for it.
    LoginAttemptsPurge,
}
