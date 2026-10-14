//! The vault's sealed secrets as the database holds them (Plan.md P9.2,
//! §7.5, ADR-0018): every row that stores an envelope, for `vault status`
//! and `vault rotate-key`.
//!
//! Two tables hold envelopes: `user_mfa` a TOTP secret per user, bound to
//! the user's ID, and `mc_accounts` a Microsoft refresh token per Microsoft
//! account, bound to the account's ID. A [`SealedRecord`] names one such row,
//! and so the table, the purpose and the record the envelope's AAD binds.
//!
//! The writes are what a rotation needs, inside its one transaction: a swap
//! that replaces an envelope only while the row still holds the one the
//! rotation read, and a count of the envelopes under other keys than the
//! active one, which must be 0 before the rotation commits.

use core::fmt;

use async_trait::async_trait;
use fleet_core::audit::AuditTarget;
use fleet_core::id::{AccountId, UserId};
use uuid::Uuid;

use super::store::StoreError;
use super::vault::VaultPurpose;

/// A row that stores a sealed secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SealedRecord {
    /// A user's TOTP secret, in `user_mfa`.
    TotpSecret(UserId),
    /// A Microsoft account's refresh token, in `mc_accounts`.
    RefreshToken(AccountId),
}

impl SealedRecord {
    /// What the secret is for, as the vault binds it.
    #[must_use]
    pub const fn purpose(self) -> VaultPurpose {
        match self {
            Self::TotpSecret(_) => VaultPurpose::TotpSecret,
            Self::RefreshToken(_) => VaultPurpose::MsRefreshToken,
        }
    }

    /// The record the vault binds the secret to: the user's or the
    /// account's ID.
    #[must_use]
    pub fn record(self) -> Uuid {
        match self {
            Self::TotpSecret(user) => user.into(),
            Self::RefreshToken(account) => account.into(),
        }
    }

    /// The table that holds the row.
    #[must_use]
    pub const fn table(self) -> &'static str {
        match self {
            Self::TotpSecret(_) => "user_mfa",
            Self::RefreshToken(_) => "mc_accounts",
        }
    }

    /// The audit log's target for the row: the user or the account.
    #[must_use]
    pub const fn audit_target(self) -> AuditTarget {
        match self {
            Self::TotpSecret(user) => AuditTarget::User(user),
            Self::RefreshToken(account) => AuditTarget::Account(account),
        }
    }
}

/// The table and the row's ID, as an operator looks it up: `user_mfa
/// 0192…`.
impl fmt::Display for SealedRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.table(), self.record())
    }
}

/// A row's sealed secret.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedEnvelope {
    /// The row.
    pub record: SealedRecord,
    /// The envelope it holds.
    pub envelope: Vec<u8>,
}

/// Shows the envelope's length, not its bytes.
impl fmt::Debug for SealedEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedEnvelope")
            .field("record", &self.record)
            .field("envelope", &format_args!("{} bytes", self.envelope.len()))
            .finish()
    }
}

/// Reading the sealed secrets through the read connections.
#[async_trait]
pub trait EnvelopeReads: Send + Sync + fmt::Debug {
    /// Every stored envelope: each user's TOTP secret and each Microsoft
    /// account's refresh token. Offline accounts hold none.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row whose ID
    /// doesn't read back.
    async fn all(&self) -> Result<Vec<SealedEnvelope>, StoreError>;
}

/// Changing the sealed secrets inside a [`super::store::WriteTx`].
#[async_trait]
pub trait EnvelopeWrites: Send {
    /// Replaces `record`'s envelope `old` with `new`, but only while the row
    /// still holds `old`. Whether it changed a row: `false` means the row
    /// changed or went away since `old` was read.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn swap(
        &mut self,
        record: SealedRecord,
        old: &[u8],
        new: &[u8],
    ) -> Result<bool, StoreError>;

    /// How many stored envelopes don't start with the version 1 header of
    /// `key_id`: those under other keys, and malformed ones.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count_not_under(&mut self, key_id: u32) -> Result<u64, StoreError>;
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use rstest::rstest;

    use super::*;

    fn user() -> UserId {
        UserId::new_v7(
            DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
            [1; 10],
        )
        .unwrap()
    }

    fn account() -> AccountId {
        AccountId::new_v7(
            DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
            [2; 10],
        )
        .unwrap()
    }

    #[rstest]
    #[case::totp_secret(
        SealedRecord::TotpSecret(user()),
        VaultPurpose::TotpSecret,
        "user_mfa",
        AuditTarget::User(user())
    )]
    #[case::refresh_token(
        SealedRecord::RefreshToken(account()),
        VaultPurpose::MsRefreshToken,
        "mc_accounts",
        AuditTarget::Account(account())
    )]
    fn each_record_names_its_purpose_record_table_and_target(
        #[case] record: SealedRecord,
        #[case] purpose: VaultPurpose,
        #[case] table: &str,
        #[case] target: AuditTarget,
    ) {
        assert_eq!(record.purpose(), purpose);
        assert_eq!(record.record(), target.id());
        assert_eq!(record.table(), table);
        assert_eq!(record.audit_target(), target);
        assert_eq!(record.to_string(), format!("{table} {}", target.id()));
    }

    #[test]
    fn debug_shows_the_envelopes_length_not_its_bytes() {
        let sealed = SealedEnvelope {
            record: SealedRecord::TotpSecret(user()),
            envelope: vec![0xAB; 65],
        };

        let debug = format!("{sealed:?}");

        assert!(debug.contains("65 bytes"), "{debug}");
        assert!(!debug.contains("171"), "{debug}");
        assert!(!debug.to_lowercase().contains("ab, "), "{debug}");
    }
}
