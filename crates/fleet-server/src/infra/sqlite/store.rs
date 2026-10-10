//! [`Database`](super::Database) as the [`Store`]: read handles over the read
//! pool, and [`SqliteWriteTx`] on the one write connection.

use core::fmt;

use async_trait::async_trait;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::error::store_error;
use super::{Database, audit, users};
use crate::ports::audit::{
    AuditEntryId, AuditPage, AuditReads, AuditRecord, AuditWrites, NewAuditEntry,
};
use crate::ports::store::{Store, StoreError, WriteTx};
use crate::ports::users::{InsertUserError, NewUser, User, UserReads, UserWrites};

/// The statement that starts a write transaction: it takes the write lock at
/// once. A fixed literal with no input, the one non-macro SQL in production
/// (ADR-0015).
const BEGIN_IMMEDIATE: &str = "BEGIN IMMEDIATE";

/// The read handles: users and the audit log over the read pool.
#[derive(Debug, Clone)]
pub(super) struct Reads {
    pub(super) pool: SqlitePool,
}

#[async_trait]
impl UserReads for Reads {
    async fn get(&self, id: UserId) -> Result<Option<User>, StoreError> {
        users::get(&self.pool, id).await
    }

    async fn find_by_username(&self, username: &Username) -> Result<Option<User>, StoreError> {
        users::find_by_username(&self.pool, username).await
    }
}

#[async_trait]
impl AuditReads for Reads {
    async fn list(&self, page: AuditPage) -> Result<Vec<AuditRecord>, StoreError> {
        audit::list(&self.pool, page).await
    }
}

/// A write transaction on the one write connection. Dropping it rolls back.
pub struct SqliteWriteTx {
    tx: Transaction<'static, Sqlite>,
}

impl fmt::Debug for SqliteWriteTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteWriteTx").finish_non_exhaustive()
    }
}

#[async_trait]
impl WriteTx for SqliteWriteTx {
    fn users(&mut self) -> &mut dyn UserWrites {
        self
    }

    fn audit(&mut self) -> &mut dyn AuditWrites {
        self
    }

    async fn commit(mut self: Box<Self>, entry: NewAuditEntry) -> Result<AuditEntryId, StoreError> {
        let id = audit::record(&mut self.tx, &entry).await?;
        self.tx.commit().await.map_err(store_error)?;
        Ok(id)
    }
}

#[async_trait]
impl UserWrites for SqliteWriteTx {
    async fn insert(&mut self, user: &NewUser) -> Result<(), InsertUserError> {
        users::insert(&mut self.tx, user).await
    }
}

#[async_trait]
impl AuditWrites for SqliteWriteTx {
    async fn record(&mut self, entry: &NewAuditEntry) -> Result<AuditEntryId, StoreError> {
        audit::record(&mut self.tx, entry).await
    }
}

#[async_trait]
impl Store for Database {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        let tx = self
            .write
            .begin_with(BEGIN_IMMEDIATE)
            .await
            .map_err(store_error)?;
        Ok(Box::new(SqliteWriteTx { tx }))
    }

    fn users(&self) -> &dyn UserReads {
        &self.reads
    }

    fn audit(&self) -> &dyn AuditReads {
        &self.reads
    }
}
