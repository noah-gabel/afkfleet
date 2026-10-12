//! The users' second factor (Plan.md P7.1, P7.7, §7.2, ADR-0016):
//! [`UserMfa`], a confirmed TOTP authenticator, its recovery codes, and the
//! read and write traits.
//!
//! A user has 2FA exactly when their [`UserMfa`] exists: only a confirmed
//! enrollment writes it, and replacing the authenticator replaces it and
//! every recovery code in one transaction. The TOTP secret is stored only as
//! a vault envelope bound to the user's ID; the recovery codes only as
//! hashes ([`RecoveryCodeHash`]).
//!
//! Two writes are conditional, so a code can't be accepted twice even by
//! two transactions that both checked it first:
//! [`MfaWrites::advance_step`] (a TOTP code's step) and
//! [`MfaWrites::use_code`] (a recovery code).

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::UserId;

use super::recovery_codes::RecoveryCodeHash;
use super::store::StoreError;

/// A user's confirmed TOTP authenticator.
#[derive(Clone)]
pub struct UserMfa {
    /// Whose it is.
    pub user_id: UserId,
    /// The TOTP secret, sealed by the vault (a version 1 envelope bound to
    /// `user_id` and the purpose `totp_secret`).
    pub secret_envelope: Vec<u8>,
    /// The last time step a code was accepted for; only a later one counts.
    pub last_used_step: u64,
    /// When it was confirmed. A login accepts a code only while it's
    /// unchanged, so a code of a just-replaced secret can't log in.
    pub confirmed_at: DateTime<Utc>,
}

/// Shows the envelope's length, not its bytes.
impl fmt::Debug for UserMfa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserMfa")
            .field("user_id", &self.user_id)
            .field(
                "secret_envelope",
                &format_args!("{} bytes", self.secret_envelope.len()),
            )
            .field("last_used_step", &self.last_used_step)
            .field("confirmed_at", &self.confirmed_at)
            .finish()
    }
}

/// Why a first enrollment couldn't be inserted.
#[derive(Debug, thiserror::Error)]
pub enum InsertMfaError {
    /// The user already has 2FA: only a replacement may change it.
    #[error("the user already has 2FA")]
    Exists,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reading second factors through the read connections.
#[async_trait]
pub trait MfaReads: Send + Sync + fmt::Debug {
    /// The user's authenticator, if they have 2FA.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&self, user: UserId) -> Result<Option<UserMfa>, StoreError>;

    /// The stored hash of the user's unused recovery code `hash`, if there is
    /// one, for the caller to confirm with [`RecoveryCodeHash::ct_eq`].
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find_unused_code(
        &self,
        user: UserId,
        hash: &RecoveryCodeHash,
    ) -> Result<Option<RecoveryCodeHash>, StoreError>;

    /// How many unused recovery codes the user has.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count_unused_codes(&self, user: UserId) -> Result<u32, StoreError>;
}

/// Reading and writing second factors inside a [`super::store::WriteTx`].
#[async_trait]
pub trait MfaWrites: Send {
    /// The user's authenticator, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, user: UserId) -> Result<Option<UserMfa>, StoreError>;

    /// Inserts a first enrollment.
    ///
    /// # Errors
    /// [`InsertMfaError::Exists`] when the user already has 2FA, or
    /// [`InsertMfaError::Store`]; a user that doesn't exist is
    /// [`StoreError::Backend`].
    async fn insert(&mut self, mfa: &UserMfa) -> Result<(), InsertMfaError>;

    /// Replaces the user's authenticator with `mfa`. `false`, with nothing
    /// changed, when the user has no 2FA.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn replace(&mut self, mfa: &UserMfa) -> Result<bool, StoreError>;

    /// Accepts `step` for the user's authenticator: sets `last_used_step` to
    /// `step`, but only while it's before `step` and the authenticator is
    /// still the one confirmed at `confirmed_at`. Whether it changed a row:
    /// `false` means the step was used meanwhile or the authenticator was
    /// replaced, and the code must be refused.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn advance_step(
        &mut self,
        user: UserId,
        step: u64,
        confirmed_at: DateTime<Utc>,
    ) -> Result<bool, StoreError>;

    /// Replaces every recovery code of the user with `hashes`, all unused.
    ///
    /// # Errors
    /// [`StoreError`]: a hash that already exists is [`StoreError::Backend`].
    async fn replace_codes(
        &mut self,
        user: UserId,
        hashes: &[RecoveryCodeHash],
    ) -> Result<(), StoreError>;

    /// Marks the user's recovery code `hash` used at `at`, but only while
    /// it's unused. Whether it changed a row: `false` means it was used
    /// meanwhile, or replaced, and the code must be refused.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn use_code(
        &mut self,
        user: UserId,
        hash: &RecoveryCodeHash,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_the_envelopes_length_not_its_bytes() {
        let mfa = UserMfa {
            user_id: UserId::new_v7(
                DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
                [1; 10],
            )
            .unwrap(),
            secret_envelope: vec![0xAB; 65],
            last_used_step: 7,
            confirmed_at: DateTime::UNIX_EPOCH,
        };

        let debug = format!("{mfa:?}");

        assert!(debug.contains("65 bytes"), "{debug}");
        assert!(!debug.contains("171"), "{debug}");
        assert!(!debug.to_lowercase().contains("ab, "), "{debug}");
    }
}
