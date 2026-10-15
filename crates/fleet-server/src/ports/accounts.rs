//! The linked Minecraft accounts (Plan.md P9.1, ADR-0018): [`McAccount`],
//! and the read and write traits.
//!
//! An account is a Microsoft account, whose refresh token is stored only as
//! a vault envelope bound to the account's ID (security rule 6), or an
//! offline account, a name for offline-mode servers in development (P9.7).
//! A Minecraft account is linked at most once, whoever linked it: a
//! Microsoft account is unique by its profile's UUID, pending ones included,
//! and an offline account by its name, whatever its case.
//!
//! Group A added what it uses: inserting an account and finding one by its
//! ID. Group C (P9.5) adds what the session-token service writes: an
//! account's status, with its reason ([`AccountWrites::set_status`]), and
//! its Minecraft name after a rename ([`AccountWrites::rename`]); it stores
//! a rotated refresh token through
//! [`EnvelopeWrites::swap`](super::envelopes::EnvelopeWrites::swap). Later
//! groups add what the link flow and the endpoints need.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::account::{AccountKind, AccountStatus};
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::{McUsername, McUuid};

use super::store::StoreError;

/// A linked Minecraft account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McAccount {
    /// Its ID, which the vault binds its refresh token to.
    pub id: AccountId,
    /// Who linked it, and so owns it with implicit Manage (ADR-0010).
    pub owner: UserId,
    /// The Minecraft name, in the case Minecraft shows it. Names compare
    /// without case; a Microsoft account is identified by its UUID.
    pub name: McUsername,
    /// When it was linked.
    pub created_at: DateTime<Utc>,
    /// How it signs in, with what that needs.
    pub credential: AccountCredential,
}

impl McAccount {
    /// Where it stands: a Microsoft account's status, and always
    /// [`AccountStatus::Active`] for an offline one.
    #[must_use]
    pub const fn status(&self) -> AccountStatus {
        match &self.credential {
            AccountCredential::Microsoft { status, .. } => *status,
            AccountCredential::Offline => AccountStatus::Active,
        }
    }
}

/// How an account signs in. Only a Microsoft account has a UUID, a refresh
/// token and a status other than active, so an offline account that waits
/// for approval can't be built.
#[derive(Clone, PartialEq, Eq)]
pub enum AccountCredential {
    /// Through Microsoft.
    Microsoft {
        /// The Minecraft profile's UUID, unique among the accounts.
        uuid: McUuid,
        /// The Microsoft refresh token, sealed by the vault: a version 1
        /// envelope bound to the account's ID and the purpose `ms_refresh`.
        refresh_envelope: Vec<u8>,
        /// Where it stands, with the reason of a refusal.
        status: AccountStatus,
    },
    /// Without any sign-in, for offline-mode servers in development.
    Offline,
}

impl AccountCredential {
    /// Its kind, as `mc_accounts.kind` stores it.
    #[must_use]
    pub const fn kind(&self) -> AccountKind {
        match self {
            Self::Microsoft { .. } => AccountKind::Microsoft,
            Self::Offline => AccountKind::Offline,
        }
    }
}

/// Shows the envelope's length, not its bytes.
impl fmt::Debug for AccountCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Microsoft {
                uuid,
                refresh_envelope,
                status,
            } => f
                .debug_struct("Microsoft")
                .field("uuid", uuid)
                .field(
                    "refresh_envelope",
                    &format_args!("{} bytes", refresh_envelope.len()),
                )
                .field("status", status)
                .finish(),
            Self::Offline => f.write_str("Offline"),
        }
    }
}

/// Why an account couldn't be inserted.
#[derive(Debug, thiserror::Error)]
pub enum InsertAccountError {
    /// The Minecraft account is linked already: a Microsoft account with the
    /// same UUID, or an offline account with the same name in any case.
    #[error("the Minecraft account is already linked")]
    Taken,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reading accounts through the read connections.
#[async_trait]
pub trait AccountReads: Send + Sync + fmt::Debug {
    /// The account `id`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&self, id: AccountId) -> Result<Option<McAccount>, StoreError>;

    /// How many accounts `owner` owns, of any kind and status: what the
    /// Member quota counts (P9.4, ADR-0018).
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count_owned_by(&self, owner: UserId) -> Result<u32, StoreError>;
}

/// Reading and writing accounts inside a [`super::store::WriteTx`].
#[async_trait]
pub trait AccountWrites: Send {
    /// The account `id`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, id: AccountId) -> Result<Option<McAccount>, StoreError>;

    /// How many accounts `owner` owns, of any kind and status, read in this
    /// transaction.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count_owned_by(&mut self, owner: UserId) -> Result<u32, StoreError>;

    /// Inserts `account`.
    ///
    /// # Errors
    /// [`InsertAccountError::Taken`] when the Minecraft account is linked
    /// already, or [`InsertAccountError::Store`]; an owner that doesn't exist
    /// or an ID that does is [`StoreError::Backend`].
    async fn insert(&mut self, account: &McAccount) -> Result<(), InsertAccountError>;

    /// Sets the Microsoft account `id`'s status to `status`, and its reason
    /// with it (none unless refused). Whether a row changed: `false` for an
    /// account that doesn't exist or is an offline one, which is always
    /// active. The caller compares the row first, in the same transaction.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn set_status(
        &mut self,
        id: AccountId,
        status: AccountStatus,
    ) -> Result<bool, StoreError>;

    /// Renames the account `id` from `from` to `to`, but only while its name
    /// is exactly `from` (case included). Whether a row changed.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn rename(
        &mut self,
        id: AccountId,
        from: &McUsername,
        to: &McUsername,
    ) -> Result<bool, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(credential: AccountCredential) -> McAccount {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        McAccount {
            id: AccountId::new_v7(at, [1; 10]).unwrap(),
            owner: UserId::new_v7(at, [2; 10]).unwrap(),
            name: "AfkBot1".parse().unwrap(),
            created_at: at,
            credential,
        }
    }

    fn microsoft(status: AccountStatus) -> AccountCredential {
        AccountCredential::Microsoft {
            uuid: "069a79f4-44e9-4726-a5be-fca90e38aaf5".parse().unwrap(),
            refresh_envelope: vec![0xAB; 61],
            status,
        }
    }

    #[test]
    fn debug_shows_the_envelopes_length_not_its_bytes() {
        let debug = format!("{:?}", account(microsoft(AccountStatus::Active)));

        assert!(debug.contains("61 bytes"), "{debug}");
        assert!(!debug.contains("171"), "{debug}");
        assert!(!debug.to_lowercase().contains("ab, "), "{debug}");
    }

    #[test]
    fn a_microsoft_account_has_its_status_and_an_offline_one_is_active() {
        let pending = account(microsoft(AccountStatus::Pending));
        let offline = account(AccountCredential::Offline);

        assert_eq!(pending.status(), AccountStatus::Pending);
        assert_eq!(pending.credential.kind(), AccountKind::Microsoft);
        assert_eq!(offline.status(), AccountStatus::Active);
        assert_eq!(offline.credential.kind(), AccountKind::Offline);
    }
}
