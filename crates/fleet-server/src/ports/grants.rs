//! The grants that share a linked account (Plan.md P9.1, P9.6, §7.3;
//! ADR-0010, ADR-0018): [`AccountGrant`], and the read and write traits.
//!
//! A grant gives one user [`GrantLevel::View`], `Control` or `Manage` on one
//! account. The account's owner has Manage through ownership and never a
//! grant, and nobody grants to themselves. There's one grant per account
//! and user, so changing one rewrites it; the grants go with their account.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::authz::GrantLevel;
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::Username;

use super::page::Page;
use super::store::StoreError;

/// One user's grant on one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountGrant {
    /// The account it shares.
    pub account: AccountId,
    /// Who it's for.
    pub user: UserId,
    /// What it allows.
    pub level: GrantLevel,
    /// Who set it last.
    pub granted_by: UserId,
    /// When it was set last.
    pub granted_at: DateTime<Utc>,
}

/// A grant as the list shows it, with its people's names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedGrant {
    /// The grant.
    pub grant: AccountGrant,
    /// The grantee's username.
    pub user_name: Username,
    /// The username of whoever set it last.
    pub granted_by_name: Username,
}

/// Why a grant couldn't be inserted.
#[derive(Debug, thiserror::Error)]
pub enum InsertGrantError {
    /// The user already holds a grant on this account.
    #[error("the user already holds a grant on the account")]
    Exists,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reading grants through the read connections.
#[async_trait]
pub trait GrantReads: Send + Sync + fmt::Debug {
    /// One page of the grants on `account`, newest grantee first by user ID.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn list(
        &self,
        account: AccountId,
        page: Page<UserId>,
    ) -> Result<Vec<ListedGrant>, StoreError>;
}

/// Reading and writing grants inside a [`super::store::WriteTx`].
#[async_trait]
pub trait GrantWrites: Send {
    /// `user`'s grant on `account`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(
        &mut self,
        account: AccountId,
        user: UserId,
    ) -> Result<Option<AccountGrant>, StoreError>;

    /// Inserts `grant`.
    ///
    /// # Errors
    /// [`InsertGrantError::Exists`] when the user already holds one on the
    /// account, or [`InsertGrantError::Store`]; an account or a user that
    /// doesn't exist, or a grant to its own granter, is
    /// [`StoreError::Backend`].
    async fn insert(&mut self, grant: &AccountGrant) -> Result<(), InsertGrantError>;

    /// Rewrites `user`'s grant on `account` with `level`, set by `by` at
    /// `at`. Whether a grant changed: `false` when there's none.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn set_level(
        &mut self,
        account: AccountId,
        user: UserId,
        level: GrantLevel,
        by: UserId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError>;

    /// Deletes `user`'s grant on `account`. Whether one was deleted.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn delete(&mut self, account: AccountId, user: UserId) -> Result<bool, StoreError>;
}
