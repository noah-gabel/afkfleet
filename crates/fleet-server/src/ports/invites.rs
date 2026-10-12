//! The invites (Plan.md P7.1, P7.9, §7.2, ADR-0016): [`Invite`], one
//! single-use, expiring invitation to register with a fixed role, and the
//! read and write traits.
//!
//! The code is stored only as its SHA-256 hash ([`TokenHash`]); a lookup by
//! hash is confirmed with [`TokenHash::ct_eq`], like a token's. What a
//! stored invite is at a given time is `fleet_core::auth::invite_state`'s
//! decision. Three writes are conditional, so an invite is used once and
//! never both used and revoked, even by two transactions that both checked
//! it first: [`InviteWrites::use_invite`], [`InviteWrites::revoke`] and
//! [`InviteWrites::revoke_pending_by`].

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::auth::InviteFacts;
use fleet_core::authz::Role;
use fleet_core::id::{InviteId, UserId};
use fleet_core::value::Username;

use super::page::Page;
use super::store::StoreError;
use super::tokens::TokenHash;

/// A stored invite.
#[derive(Debug, Clone)]
pub struct Invite {
    /// The ID.
    pub id: InviteId,
    /// SHA-256 of the code's text.
    pub code_hash: TokenHash,
    /// The role the new user gets: a Member or an Admin, never the Owner.
    pub role: Role,
    /// Who created it.
    pub created_by: UserId,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
    /// The user it registered, once used.
    pub used_by: Option<UserId>,
    /// When it registered that user.
    pub used_at: Option<DateTime<Utc>>,
    /// When it was revoked, if it was.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Invite {
    /// The facts `fleet_core::auth::invite_state` decides on.
    #[must_use]
    pub const fn facts(&self) -> InviteFacts {
        InviteFacts {
            expires_at: self.expires_at,
            used_at: self.used_at,
            revoked_at: self.revoked_at,
        }
    }
}

/// An invite to create: unused and unrevoked.
#[derive(Debug, Clone)]
pub struct NewInvite {
    /// The ID, minted from `created_at`.
    pub id: InviteId,
    /// SHA-256 of the code's text.
    pub code_hash: TokenHash,
    /// The role the new user gets: a Member or an Admin.
    pub role: Role,
    /// Who creates it.
    pub created_by: UserId,
    /// When it's created.
    pub created_at: DateTime<Utc>,
    /// When it expires: after `created_at`.
    pub expires_at: DateTime<Utc>,
}

/// An invite as the list shows it, with its people's names.
#[derive(Debug, Clone)]
pub struct ListedInvite {
    /// The invite.
    pub invite: Invite,
    /// Its creator's username.
    pub created_by_name: Username,
    /// The username of the user it registered, once used.
    pub used_by_name: Option<Username>,
}

/// An invite [`InviteWrites::revoke_pending_by`] revoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevokedInvite {
    /// Its ID.
    pub id: InviteId,
    /// Its role.
    pub role: Role,
}

/// Reading invites through the read connections.
#[async_trait]
pub trait InviteReads: Send + Sync + fmt::Debug {
    /// The invite with this ID.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&self, id: InviteId) -> Result<Option<Invite>, StoreError>;

    /// The invite whose code hashes to `hash`, for the caller to confirm
    /// with [`TokenHash::ct_eq`].
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find_by_code_hash(&self, hash: &TokenHash) -> Result<Option<Invite>, StoreError>;

    /// The invites of `page`, newest first, every state, with their
    /// people's names: up to `page.limit.fetch()` of them, one more than the
    /// page holds.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn list(&self, page: Page<InviteId>) -> Result<Vec<ListedInvite>, StoreError>;
}

/// Reading and writing invites inside a [`super::store::WriteTx`].
#[async_trait]
pub trait InviteWrites: Send {
    /// Inserts `invite`.
    ///
    /// # Errors
    /// [`StoreError`]: a creator that doesn't exist or a hash that already
    /// exists is [`StoreError::Backend`].
    async fn insert(&mut self, invite: &NewInvite) -> Result<(), StoreError>;

    /// The invite with this ID, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, id: InviteId) -> Result<Option<Invite>, StoreError>;

    /// The invite whose code hashes to `hash`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find_by_code_hash(&mut self, hash: &TokenHash) -> Result<Option<Invite>, StoreError>;

    /// How many pending invites `creator` holds at `now`: unused,
    /// unrevoked and unexpired.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count_pending_by(
        &mut self,
        creator: UserId,
        now: DateTime<Utc>,
    ) -> Result<u32, StoreError>;

    /// Marks the invite used by `user` at `at`, but only while it's unused,
    /// unrevoked and unexpired at `at`. Whether it changed a row: `false`
    /// means it was used, revoked or expired meanwhile.
    ///
    /// # Errors
    /// [`StoreError`]: a user that doesn't exist, or one who already used
    /// an invite, is [`StoreError::Backend`].
    async fn use_invite(
        &mut self,
        id: InviteId,
        user: UserId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError>;

    /// Revokes the invite at `at`, but only while it's unused, unrevoked
    /// and unexpired at `at`. Whether it changed a row.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn revoke(&mut self, id: InviteId, at: DateTime<Utc>) -> Result<bool, StoreError>;

    /// Revokes every invite `creator` holds pending at `at`, and returns
    /// them.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn revoke_pending_by(
        &mut self,
        creator: UserId,
        at: DateTime<Utc>,
    ) -> Result<Vec<RevokedInvite>, StoreError>;
}
