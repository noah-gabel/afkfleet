//! [`Database`](super::Database) as the [`Store`]: read handles over the read
//! pool, and [`SqliteWriteTx`] on the one write connection.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::{SessionId, UserId};
use fleet_core::value::Username;
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::error::store_error;
use super::{Database, audit, sessions, users};
use crate::ports::audit::{
    AuditEntryId, AuditPage, AuditReads, AuditRecord, AuditWrites, NewAuditEntry,
};
use crate::ports::sessions::{
    NewSession, RefreshLookup, Rotation, SessionReads, SessionWithUser, SessionWrites,
};
use crate::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};
use crate::ports::tokens::TokenHash;
use crate::ports::users::{InsertUserError, NewUser, User, UserReads, UserWrites};

/// The statement that starts a write transaction: it takes the write lock at
/// once. A fixed literal with no input, the one non-macro SQL in production
/// (ADR-0015).
const BEGIN_IMMEDIATE: &str = "BEGIN IMMEDIATE";

/// The read handles: users, sessions and the audit log over the read pool.
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

    async fn owner_exists(&self) -> Result<bool, StoreError> {
        users::owner_exists(&self.pool).await
    }
}

#[async_trait]
impl SessionReads for Reads {
    async fn find_by_access_hash(
        &self,
        hash: &TokenHash,
    ) -> Result<Option<SessionWithUser>, StoreError> {
        sessions::find_by_access_hash(&self.pool, hash).await
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

    fn sessions(&mut self) -> &mut dyn SessionWrites {
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

    async fn commit_unaudited(self: Box<Self>, _reason: UnauditedWrite) -> Result<(), StoreError> {
        // The reason names the exemption at the call site; nothing is stored.
        self.tx.commit().await.map_err(store_error)
    }
}

#[async_trait]
impl SessionWrites for SqliteWriteTx {
    async fn insert(&mut self, session: &NewSession) -> Result<(), StoreError> {
        sessions::insert(&mut self.tx, session).await
    }

    async fn find_by_refresh_hash(
        &mut self,
        hash: &TokenHash,
    ) -> Result<Option<RefreshLookup>, StoreError> {
        sessions::find_by_refresh_hash(&mut self.tx, hash).await
    }

    async fn rotate(&mut self, rotation: &Rotation) -> Result<bool, StoreError> {
        sessions::rotate(&mut self.tx, rotation).await
    }

    async fn revoke(&mut self, id: SessionId, at: DateTime<Utc>) -> Result<bool, StoreError> {
        sessions::revoke(&mut self.tx, id, at).await
    }

    async fn touch(
        &mut self,
        id: SessionId,
        at: DateTime<Utc>,
        stale_since: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        sessions::touch(&mut self.tx, id, at, stale_since).await
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

    fn sessions(&self) -> &dyn SessionReads {
        &self.reads
    }

    fn audit(&self) -> &dyn AuditReads {
        &self.reads
    }

    async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query_scalar!("SELECT 1")
            .fetch_one(&self.read)
            .await
            .map(drop)
            .map_err(store_error)
    }
}
