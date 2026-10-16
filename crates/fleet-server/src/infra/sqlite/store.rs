//! [`Database`](super::Database) as the [`Store`]: read handles over the read
//! pool, and [`SqliteWriteTx`] on the one write connection.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditTarget};
use fleet_core::authz::{Actor, GrantLevel, Role};
use fleet_core::id::{AccountId, AgentId, EnrollmentTokenId, InviteId, SessionId, UserId};
use fleet_core::value::{AgentName, McUsername, Username};
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::error::store_error;
use super::{
    Database, accounts, agents, audit, enrollment_tokens, envelopes, grants, invites,
    login_attempts, mfa, sessions, users,
};
use crate::ports::accounts::{
    AccountReads, AccountWrites, InsertAccountError, ListedAccount, McAccount,
};
use crate::ports::agents::{
    Agent, AgentCert, AgentReads, AgentWrites, InsertAgentError, NewAgent, NewAgentCert,
};
use crate::ports::audit::{
    AuditEntryId, AuditPage, AuditReads, AuditRecord, AuditWrites, FailedCodeAttempts,
    NewAuditEntry,
};
use crate::ports::enrollment_tokens::{
    EnrollmentToken, EnrollmentTokenReads, EnrollmentTokenWrites, NewEnrollmentToken,
};
use crate::ports::envelopes::{EnvelopeReads, EnvelopeWrites, SealedEnvelope, SealedRecord};
use crate::ports::grants::{AccountGrant, GrantReads, GrantWrites, InsertGrantError, ListedGrant};
use crate::ports::invites::{
    Invite, InviteReads, InviteWrites, ListedInvite, NewInvite, RevokedInvite,
};
use crate::ports::login_attempts::{
    AttemptKey, LoginAttempt, LoginAttemptReads, LoginAttemptWrites,
};
use crate::ports::mfa::{InsertMfaError, MfaReads, MfaWrites, UserMfa};
use crate::ports::page::Page;
use crate::ports::recovery_codes::RecoveryCodeHash;
use crate::ports::sessions::{
    LiveSince, NewSession, RefreshLookup, Rotation, Session, SessionReads, SessionWithUser,
    SessionWrites,
};
use crate::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};
use crate::ports::tokens::TokenHash;
use crate::ports::users::{
    InsertUserError, ListedUser, NewUser, PasswordHash, User, UserReads, UserWrites,
};

/// The statement that starts a write transaction: it takes the write lock at
/// once. A fixed literal with no input, the one non-macro SQL in production
/// (ADR-0015).
const BEGIN_IMMEDIATE: &str = "BEGIN IMMEDIATE";

/// The read handles: users, sessions, login failures and the audit log over
/// the read pool.
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

    async fn list(&self, page: Page<UserId>) -> Result<Vec<ListedUser>, StoreError> {
        users::list(&self.pool, page).await
    }

    async fn listed(&self, id: UserId) -> Result<Option<ListedUser>, StoreError> {
        users::listed(&self.pool, id).await
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

    async fn refresh_hash_exists(&self, hash: &TokenHash) -> Result<bool, StoreError> {
        sessions::refresh_hash_exists(&self.pool, hash).await
    }

    async fn list_live(
        &self,
        user: UserId,
        live: LiveSince,
        page: Page<SessionId>,
    ) -> Result<Vec<Session>, StoreError> {
        sessions::list_live(&self.pool, user, live, page).await
    }
}

#[async_trait]
impl MfaReads for Reads {
    async fn find(&self, user: UserId) -> Result<Option<UserMfa>, StoreError> {
        mfa::find(&self.pool, user).await
    }

    async fn find_unused_code(
        &self,
        user: UserId,
        hash: &RecoveryCodeHash,
    ) -> Result<Option<RecoveryCodeHash>, StoreError> {
        mfa::find_unused_code(&self.pool, user, hash).await
    }

    async fn count_unused_codes(&self, user: UserId) -> Result<u32, StoreError> {
        mfa::count_unused_codes(&self.pool, user).await
    }
}

#[async_trait]
impl AccountReads for Reads {
    async fn find(&self, id: AccountId) -> Result<Option<McAccount>, StoreError> {
        accounts::find(&self.pool, id).await
    }

    async fn count_owned_by(&self, owner: UserId) -> Result<u32, StoreError> {
        accounts::count_owned_by(&self.pool, owner).await
    }

    async fn count_offline(&self) -> Result<u64, StoreError> {
        accounts::count_offline(&self.pool).await
    }

    async fn listed(
        &self,
        id: AccountId,
        viewer: UserId,
    ) -> Result<Option<ListedAccount>, StoreError> {
        accounts::listed(&self.pool, id, viewer).await
    }

    async fn list_visible(
        &self,
        viewer: Actor,
        page: Page<AccountId>,
    ) -> Result<Vec<ListedAccount>, StoreError> {
        accounts::list_visible(&self.pool, viewer, page).await
    }

    async fn awaiting_review(
        &self,
        viewer: Actor,
        page: Page<AccountId>,
    ) -> Result<Vec<ListedAccount>, StoreError> {
        accounts::awaiting_review(&self.pool, viewer, page).await
    }
}

#[async_trait]
impl GrantReads for Reads {
    async fn list(
        &self,
        account: AccountId,
        page: Page<UserId>,
    ) -> Result<Vec<ListedGrant>, StoreError> {
        grants::list(&self.pool, account, page).await
    }
}

#[async_trait]
impl EnrollmentTokenReads for Reads {
    async fn find_by_hash(&self, hash: &TokenHash) -> Result<Option<EnrollmentToken>, StoreError> {
        enrollment_tokens::find_by_hash(&self.pool, hash).await
    }
}

#[async_trait]
impl AgentReads for Reads {
    async fn find(&self, id: AgentId) -> Result<Option<Agent>, StoreError> {
        agents::find(&self.pool, id).await
    }

    async fn name_taken(&self, name: &AgentName) -> Result<bool, StoreError> {
        agents::name_taken(&self.pool, name).await
    }

    async fn certs(&self, id: AgentId) -> Result<Vec<AgentCert>, StoreError> {
        agents::certs(&self.pool, id).await
    }
}

#[async_trait]
impl EnvelopeReads for Reads {
    async fn all(&self) -> Result<Vec<SealedEnvelope>, StoreError> {
        envelopes::all(&self.pool).await
    }
}

#[async_trait]
impl InviteReads for Reads {
    async fn find(&self, id: InviteId) -> Result<Option<Invite>, StoreError> {
        invites::find(&self.pool, id).await
    }

    async fn find_by_code_hash(&self, hash: &TokenHash) -> Result<Option<Invite>, StoreError> {
        invites::find_by_code_hash(&self.pool, hash).await
    }

    async fn list(&self, page: Page<InviteId>) -> Result<Vec<ListedInvite>, StoreError> {
        invites::list(&self.pool, page).await
    }
}

#[async_trait]
impl LoginAttemptReads for Reads {
    async fn find(&self, key: &AttemptKey) -> Result<Option<LoginAttempt>, StoreError> {
        login_attempts::find(&self.pool, key).await
    }
}

#[async_trait]
impl AuditReads for Reads {
    async fn list(&self, page: AuditPage) -> Result<Vec<AuditRecord>, StoreError> {
        audit::list(&self.pool, page).await
    }

    async fn failed_code_attempts(&self, user: UserId) -> Result<FailedCodeAttempts, StoreError> {
        audit::failed_code_attempts(&self.pool, user).await
    }

    async fn user_successes_since(
        &self,
        user: UserId,
        action: &AuditAction,
        since: DateTime<Utc>,
    ) -> Result<Vec<DateTime<Utc>>, StoreError> {
        audit::user_successes_since(&self.pool, user, action, since).await
    }

    async fn failures_on(
        &self,
        target: AuditTarget,
        action: &AuditAction,
    ) -> Result<u32, StoreError> {
        audit::failures_on(&self.pool, target, action).await
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

    fn login_attempts(&mut self) -> &mut dyn LoginAttemptWrites {
        self
    }

    fn mfa(&mut self) -> &mut dyn MfaWrites {
        self
    }

    fn invites(&mut self) -> &mut dyn InviteWrites {
        self
    }

    fn accounts(&mut self) -> &mut dyn AccountWrites {
        self
    }

    fn grants(&mut self) -> &mut dyn GrantWrites {
        self
    }

    fn enrollment_tokens(&mut self) -> &mut dyn EnrollmentTokenWrites {
        self
    }

    fn agents(&mut self) -> &mut dyn AgentWrites {
        self
    }

    fn envelopes(&mut self) -> &mut dyn EnvelopeWrites {
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

    async fn revoke_all_for_user(
        &mut self,
        user: UserId,
        except: Option<SessionId>,
        at: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        sessions::revoke_all_for_user(&mut self.tx, user, except, at).await
    }

    async fn find(&mut self, id: SessionId) -> Result<Option<SessionWithUser>, StoreError> {
        sessions::find_by_id(&mut self.tx, id).await
    }

    async fn stamp_step_up(
        &mut self,
        id: SessionId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        sessions::stamp_step_up(&mut self.tx, id, at).await
    }

    async fn clear_step_up(&mut self, id: SessionId) -> Result<bool, StoreError> {
        sessions::clear_step_up(&mut self.tx, id).await
    }
}

#[async_trait]
impl MfaWrites for SqliteWriteTx {
    async fn find(&mut self, user: UserId) -> Result<Option<UserMfa>, StoreError> {
        mfa::find(&mut *self.tx, user).await
    }

    async fn insert(&mut self, mfa: &UserMfa) -> Result<(), InsertMfaError> {
        mfa::insert(&mut self.tx, mfa).await
    }

    async fn replace(&mut self, mfa: &UserMfa) -> Result<bool, StoreError> {
        mfa::replace(&mut self.tx, mfa).await
    }

    async fn advance_step(
        &mut self,
        user: UserId,
        step: u64,
        confirmed_at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        mfa::advance_step(&mut self.tx, user, step, confirmed_at).await
    }

    async fn replace_codes(
        &mut self,
        user: UserId,
        hashes: &[RecoveryCodeHash],
    ) -> Result<(), StoreError> {
        mfa::replace_codes(&mut self.tx, user, hashes).await
    }

    async fn use_code(
        &mut self,
        user: UserId,
        hash: &RecoveryCodeHash,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        mfa::use_code(&mut self.tx, user, hash, at).await
    }
}

#[async_trait]
impl AccountWrites for SqliteWriteTx {
    async fn find(&mut self, id: AccountId) -> Result<Option<McAccount>, StoreError> {
        accounts::find(&mut *self.tx, id).await
    }

    async fn count_owned_by(&mut self, owner: UserId) -> Result<u32, StoreError> {
        accounts::count_owned_by(&mut *self.tx, owner).await
    }

    async fn insert(&mut self, account: &McAccount) -> Result<(), InsertAccountError> {
        accounts::insert(&mut self.tx, account).await
    }

    async fn set_status(
        &mut self,
        id: AccountId,
        status: AccountStatus,
    ) -> Result<bool, StoreError> {
        accounts::set_status(&mut self.tx, id, status).await
    }

    async fn rename(
        &mut self,
        id: AccountId,
        from: &McUsername,
        to: &McUsername,
    ) -> Result<bool, StoreError> {
        accounts::rename(&mut self.tx, id, from, to).await
    }

    async fn listed(
        &mut self,
        id: AccountId,
        viewer: UserId,
    ) -> Result<Option<ListedAccount>, StoreError> {
        accounts::listed(&mut *self.tx, id, viewer).await
    }

    async fn delete(&mut self, id: AccountId) -> Result<bool, StoreError> {
        accounts::delete(&mut self.tx, id).await
    }

    async fn relink(
        &mut self,
        id: AccountId,
        from: &[u8],
        to: &[u8],
        name: &McUsername,
    ) -> Result<bool, StoreError> {
        accounts::relink(&mut self.tx, id, from, to, name).await
    }
}

#[async_trait]
impl GrantWrites for SqliteWriteTx {
    async fn find(
        &mut self,
        account: AccountId,
        user: UserId,
    ) -> Result<Option<AccountGrant>, StoreError> {
        grants::find(&mut self.tx, account, user).await
    }

    async fn insert(&mut self, grant: &AccountGrant) -> Result<(), InsertGrantError> {
        grants::insert(&mut self.tx, grant).await
    }

    async fn set_level(
        &mut self,
        account: AccountId,
        user: UserId,
        level: GrantLevel,
        by: UserId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        grants::set_level(&mut self.tx, account, user, level, by, at).await
    }

    async fn delete(&mut self, account: AccountId, user: UserId) -> Result<bool, StoreError> {
        grants::delete(&mut self.tx, account, user).await
    }
}

#[async_trait]
impl EnrollmentTokenWrites for SqliteWriteTx {
    async fn insert(&mut self, token: &NewEnrollmentToken) -> Result<(), StoreError> {
        enrollment_tokens::insert(&mut self.tx, token).await
    }

    async fn find_by_hash(
        &mut self,
        hash: &TokenHash,
    ) -> Result<Option<EnrollmentToken>, StoreError> {
        enrollment_tokens::find_by_hash(&mut *self.tx, hash).await
    }

    async fn count_pending(&mut self, now: DateTime<Utc>) -> Result<u32, StoreError> {
        enrollment_tokens::count_pending(&mut self.tx, now).await
    }

    async fn use_token(
        &mut self,
        id: EnrollmentTokenId,
        agent: AgentId,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        enrollment_tokens::use_token(&mut self.tx, id, agent, now).await
    }
}

#[async_trait]
impl AgentWrites for SqliteWriteTx {
    async fn insert(&mut self, agent: &NewAgent) -> Result<(), InsertAgentError> {
        agents::insert(&mut self.tx, agent).await
    }

    async fn insert_cert(&mut self, cert: &NewAgentCert) -> Result<(), StoreError> {
        agents::insert_cert(&mut self.tx, cert).await
    }
}

#[async_trait]
impl EnvelopeWrites for SqliteWriteTx {
    async fn swap(
        &mut self,
        record: SealedRecord,
        old: &[u8],
        new: &[u8],
    ) -> Result<bool, StoreError> {
        envelopes::swap(&mut self.tx, record, old, new).await
    }

    async fn count_not_under(&mut self, key_id: u32) -> Result<u64, StoreError> {
        envelopes::count_not_under(&mut self.tx, key_id).await
    }
}

#[async_trait]
impl InviteWrites for SqliteWriteTx {
    async fn insert(&mut self, invite: &NewInvite) -> Result<(), StoreError> {
        invites::insert(&mut self.tx, invite).await
    }

    async fn find(&mut self, id: InviteId) -> Result<Option<Invite>, StoreError> {
        invites::find(&mut *self.tx, id).await
    }

    async fn find_by_code_hash(&mut self, hash: &TokenHash) -> Result<Option<Invite>, StoreError> {
        invites::find_by_code_hash(&mut *self.tx, hash).await
    }

    async fn count_pending_by(
        &mut self,
        creator: UserId,
        now: DateTime<Utc>,
    ) -> Result<u32, StoreError> {
        invites::count_pending_by(&mut self.tx, creator, now).await
    }

    async fn use_invite(
        &mut self,
        id: InviteId,
        user: UserId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        invites::use_invite(&mut self.tx, id, user, at).await
    }

    async fn revoke(&mut self, id: InviteId, at: DateTime<Utc>) -> Result<bool, StoreError> {
        invites::revoke(&mut self.tx, id, at).await
    }

    async fn revoke_pending_by(
        &mut self,
        creator: UserId,
        at: DateTime<Utc>,
    ) -> Result<Vec<RevokedInvite>, StoreError> {
        invites::revoke_pending_by(&mut self.tx, creator, at).await
    }
}

#[async_trait]
impl LoginAttemptWrites for SqliteWriteTx {
    async fn find(&mut self, key: &AttemptKey) -> Result<Option<LoginAttempt>, StoreError> {
        login_attempts::find(&mut *self.tx, key).await
    }

    async fn upsert(&mut self, key: &AttemptKey, attempt: &LoginAttempt) -> Result<(), StoreError> {
        login_attempts::upsert(&mut self.tx, key, attempt).await
    }

    async fn delete(&mut self, key: &AttemptKey) -> Result<bool, StoreError> {
        login_attempts::delete(&mut self.tx, key).await
    }

    async fn purge_stale(
        &mut self,
        failed_by: DateTime<Utc>,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError> {
        login_attempts::purge_stale(&mut self.tx, failed_by, now, limit).await
    }
}

#[async_trait]
impl UserWrites for SqliteWriteTx {
    async fn insert(&mut self, user: &NewUser) -> Result<(), InsertUserError> {
        users::insert(&mut self.tx, user).await
    }

    async fn get(&mut self, id: UserId) -> Result<Option<User>, StoreError> {
        users::get_in(&mut self.tx, id).await
    }

    async fn find_by_username(&mut self, username: &Username) -> Result<Option<User>, StoreError> {
        users::find_by_username(&mut *self.tx, username).await
    }

    async fn set_password_hash(
        &mut self,
        id: UserId,
        hash: &PasswordHash,
    ) -> Result<bool, StoreError> {
        users::set_password_hash(&mut self.tx, id, hash).await
    }

    async fn change_password(
        &mut self,
        id: UserId,
        hash: &PasswordHash,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        users::change_password(&mut self.tx, id, hash, at).await
    }

    async fn set_role(&mut self, id: UserId, role: Role) -> Result<bool, StoreError> {
        users::set_role(&mut self.tx, id, role).await
    }

    async fn set_disabled(&mut self, id: UserId, disabled: bool) -> Result<bool, StoreError> {
        users::set_disabled(&mut self.tx, id, disabled).await
    }
}

#[async_trait]
impl AuditWrites for SqliteWriteTx {
    async fn record(&mut self, entry: &NewAuditEntry) -> Result<AuditEntryId, StoreError> {
        audit::record(&mut self.tx, entry).await
    }

    async fn user_successes_since(
        &mut self,
        user: UserId,
        action: &AuditAction,
        since: DateTime<Utc>,
    ) -> Result<Vec<DateTime<Utc>>, StoreError> {
        audit::user_successes_since(&mut *self.tx, user, action, since).await
    }

    async fn failures_on(
        &mut self,
        target: AuditTarget,
        action: &AuditAction,
    ) -> Result<u32, StoreError> {
        audit::failures_on(&mut *self.tx, target, action).await
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

    fn login_attempts(&self) -> &dyn LoginAttemptReads {
        &self.reads
    }

    fn mfa(&self) -> &dyn MfaReads {
        &self.reads
    }

    fn invites(&self) -> &dyn InviteReads {
        &self.reads
    }

    fn accounts(&self) -> &dyn AccountReads {
        &self.reads
    }

    fn grants(&self) -> &dyn GrantReads {
        &self.reads
    }

    fn enrollment_tokens(&self) -> &dyn EnrollmentTokenReads {
        &self.reads
    }

    fn agents(&self) -> &dyn AgentReads {
        &self.reads
    }

    fn envelopes(&self) -> &dyn EnvelopeReads {
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
