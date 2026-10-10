//! [`Store`] and [`WriteTx`]: the database as a unit of work, and
//! [`StoreError`], how it fails.

use core::fmt;

use async_trait::async_trait;

use super::audit::{AuditEntryId, AuditReads, AuditWrites, NewAuditEntry};
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
/// describes the change, so a change can't be committed without one.
#[async_trait]
pub trait WriteTx: Send {
    /// The users, written in this transaction.
    fn users(&mut self) -> &mut dyn UserWrites;

    /// The audit log, for a second entry in this transaction.
    fn audit(&mut self) -> &mut dyn AuditWrites;

    /// Records `entry`, then commits everything.
    ///
    /// # Errors
    /// [`StoreError`] when the entry can't be recorded or the commit fails;
    /// nothing is committed then.
    async fn commit(self: Box<Self>, entry: NewAuditEntry) -> Result<AuditEntryId, StoreError>;
}
