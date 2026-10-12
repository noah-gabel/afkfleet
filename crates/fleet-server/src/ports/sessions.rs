//! The sessions (Plan.md P7.1, P7.3, §7.2, ADR-0016): [`NewSession`],
//! [`Session`] as stored, the lookups' results, and the read and write
//! traits.
//!
//! A session is one login's token family: its ID stays the same across
//! every refresh, and its row holds the hashes of the current access and
//! refresh tokens. Every refresh token it replaced stays in its history
//! until the session is deleted, so presenting one again is recognized as
//! reuse. No expiry is stored: `fleet_core::auth` computes it from
//! `created_at` and `refreshed_at` with the running config's lifetimes.
//!
//! The repository returns revoked and expired sessions too; the service's
//! rule decides what they mean.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};

use super::page::Page;
use super::store::StoreError;
use super::tokens::TokenHash;

/// A session to create, made before the write transaction opens.
#[derive(Debug, Clone)]
pub struct NewSession {
    /// The ID, minted from `created_at`.
    pub id: SessionId,
    /// Whose session it is.
    pub user_id: UserId,
    /// The first access token's hash.
    pub access_hash: TokenHash,
    /// The first refresh token's hash.
    pub refresh_hash: TokenHash,
    /// When it's created; also its first `refreshed_at` and `last_used_at`.
    pub created_at: DateTime<Utc>,
}

/// A stored session.
#[derive(Debug, Clone)]
pub struct Session {
    /// The ID: the token family's, the same across every refresh.
    pub id: SessionId,
    /// Whose session it is.
    pub user_id: UserId,
    /// The current access token's hash.
    pub access_hash: TokenHash,
    /// The current refresh token's hash.
    pub refresh_hash: TokenHash,
    /// When it was created: the anchor of its absolute expiry.
    pub created_at: DateTime<Utc>,
    /// When the current token pair was issued: the anchor of the access
    /// token's expiry and of the idle expiry.
    pub refreshed_at: DateTime<Utc>,
    /// The last authenticated request, written at most once a minute.
    pub last_used_at: DateTime<Utc>,
    /// When the password was last entered again (step-up), if ever.
    pub stepped_up_at: Option<DateTime<Utc>>,
    /// When it was revoked, if it was.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// A session with its user's current role and status, read together so a
/// role change or a disabled user counts on the very next request. A
/// session whose user row is gone isn't found at all.
#[derive(Debug, Clone)]
pub struct SessionWithUser {
    /// The session.
    pub session: Session,
    /// The user's current role.
    pub role: Role,
    /// Whether the user is disabled.
    pub disabled: bool,
}

/// What a presented refresh token's hash found.
#[derive(Debug, Clone)]
pub enum RefreshLookup {
    /// The session's current refresh token.
    Current(SessionWithUser),
    /// A refresh token the session has already replaced.
    Rotated {
        /// The session it belonged to.
        session: SessionWithUser,
        /// The replaced token's hash, as stored in the history.
        hash: TokenHash,
        /// When it was replaced.
        rotated_at: DateTime<Utc>,
    },
}

/// A refresh: the session's new token hashes. The replaced refresh token's
/// hash moves into the history.
#[derive(Debug, Clone)]
pub struct Rotation {
    /// The session.
    pub session_id: SessionId,
    /// The refresh token being replaced: the session's current one.
    pub replaced: TokenHash,
    /// The new access token's hash.
    pub access_hash: TokenHash,
    /// The new refresh token's hash.
    pub refresh_hash: TokenHash,
    /// When the new pair is issued: the new `refreshed_at`, and the replaced
    /// token's `rotated_at`.
    pub at: DateTime<Utc>,
}

/// What makes a session live at a time: not revoked, and both its idle and
/// its absolute expiry still ahead (`fleet_core::auth::SessionPolicy`'s
/// refresh expiry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSince {
    /// Only sessions refreshed after this: now minus the idle lifetime.
    pub refreshed_after: DateTime<Utc>,
    /// Only sessions created after this: now minus the absolute lifetime.
    pub created_after: DateTime<Utc>,
}

/// Reading sessions through the read connections.
#[async_trait]
pub trait SessionReads: Send + Sync + fmt::Debug {
    /// The session whose current access token hashes to `hash`, with its
    /// user, if there is one.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find_by_access_hash(
        &self,
        hash: &TokenHash,
    ) -> Result<Option<SessionWithUser>, StoreError>;

    /// Whether `hash` is any session's current refresh token or one it
    /// already replaced. A refresh asks this first, so an unknown token is
    /// refused without taking the one write connection; a token reaches a
    /// client only after its commit, so this never misses a real one.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn refresh_hash_exists(&self, hash: &TokenHash) -> Result<bool, StoreError>;

    /// The user's live sessions of `page`, newest first: up to
    /// `page.limit.fetch()` of them, one more than the page holds.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn list_live(
        &self,
        user: UserId,
        live: LiveSince,
        page: Page<SessionId>,
    ) -> Result<Vec<Session>, StoreError>;
}

/// Reading and writing sessions inside a [`super::store::WriteTx`]. A
/// refresh reads here, not through the read connections, so two refreshes
/// with the same token take turns (`BEGIN IMMEDIATE`) and the second finds
/// the token already replaced.
#[async_trait]
pub trait SessionWrites: Send {
    /// Ends session `id`'s step-up: its next step-up action needs the
    /// password again. Whether the session exists.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn clear_step_up(&mut self, id: SessionId) -> Result<bool, StoreError>;

    /// Inserts `session`.
    ///
    /// # Errors
    /// [`StoreError`]: a hash or ID that already exists, or a user that
    /// doesn't, is [`StoreError::Backend`].
    async fn insert(&mut self, session: &NewSession) -> Result<(), StoreError>;

    /// The session a refresh token's `hash` belongs to, as its current
    /// token or one it already replaced, with its user.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find_by_refresh_hash(
        &mut self,
        hash: &TokenHash,
    ) -> Result<Option<RefreshLookup>, StoreError>;

    /// Replaces the session's tokens and keeps the replaced refresh token's
    /// hash in its history. `false`, with nothing changed, when the session
    /// doesn't exist or its current refresh token isn't `replaced`.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn rotate(&mut self, rotation: &Rotation) -> Result<bool, StoreError>;

    /// Revokes the session at `at`. `false`, with nothing changed, when it
    /// doesn't exist or was already revoked.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn revoke(&mut self, id: SessionId, at: DateTime<Utc>) -> Result<bool, StoreError>;

    /// Revokes every session of `user` that isn't revoked yet at `at`, all
    /// but `except` when it's given (the session that asked). How many it
    /// revoked.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn revoke_all_for_user(
        &mut self,
        user: UserId,
        except: Option<SessionId>,
        at: DateTime<Utc>,
    ) -> Result<u64, StoreError>;

    /// The session with this ID and its user, read in this transaction, so a
    /// commit that acts for a session can check it's still live.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, id: SessionId) -> Result<Option<SessionWithUser>, StoreError>;

    /// Stamps `stepped_up_at` = `at` on the session, if it isn't revoked.
    /// Whether exactly that one row changed.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn stamp_step_up(&mut self, id: SessionId, at: DateTime<Utc>)
    -> Result<bool, StoreError>;

    /// Sets `last_used_at` to `at`, but only when the stored value is at or
    /// before `stale_since`, so two requests that both found it stale write
    /// it once, and a clock that went back never moves it backwards. Whether
    /// it wrote.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn touch(
        &mut self,
        id: SessionId,
        at: DateTime<Utc>,
        stale_since: DateTime<Utc>,
    ) -> Result<bool, StoreError>;
}
