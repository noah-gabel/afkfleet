//! The vault's port: [`Vault`] encrypts the secrets the server stores, bound
//! to their record and purpose (Plan.md §7.5, P7.4, ADR-0016).
//!
//! TOTP secrets go through it from group D on, Microsoft refresh tokens from
//! P9 (security rule 6). Its errors name no crate's type.

use core::fmt;

use fleet_core::system::RandomError;
use secrecy::SecretBox;
use uuid::Uuid;

/// What a sealed secret is for. Its name is part of the envelope's AAD, so
/// a secret sealed for one purpose never opens as another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VaultPurpose {
    /// A user's TOTP secret (group D).
    TotpSecret,
}

impl VaultPurpose {
    /// The name bound into the AAD. Renaming one makes every secret of that
    /// purpose undecryptable, so a test pins each.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TotpSecret => "totp_secret",
        }
    }
}

/// Why a secret couldn't be sealed or opened.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The envelope names a key ID no configured key has.
    #[error("the envelope's key {key_id} isn't configured")]
    UnknownKey {
        /// The key ID in the envelope.
        key_id: u32,
    },
    /// The envelope is too short or has an unknown version.
    #[error("the envelope is malformed")]
    Malformed,
    /// Authentication failed: the envelope was changed, or opened with the
    /// wrong key, record or purpose. These look the same on purpose.
    #[error("the envelope doesn't authenticate")]
    Rejected,
    /// The OS's randomness failed while drawing the nonce.
    #[error("no nonce could be drawn")]
    Random(#[from] RandomError),
    /// The plaintext is beyond the cipher's limit (about 256 GiB), which no
    /// secret the server stores comes near.
    #[error("the plaintext is too large to seal")]
    TooLarge,
}

/// Encrypting secrets at rest (§7.5): `seal` binds a secret to the record
/// that stores it and to its purpose, and `open` checks both.
pub trait Vault: Send + Sync + fmt::Debug {
    /// Encrypts `plaintext` for `record` and `purpose` with the active key,
    /// as a version 1 envelope.
    ///
    /// # Errors
    /// [`VaultError::Random`] when no nonce could be drawn, or
    /// [`VaultError::TooLarge`].
    fn seal(
        &self,
        purpose: VaultPurpose,
        record: Uuid,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, VaultError>;

    /// Decrypts `envelope`, which must have been sealed for `record` and
    /// `purpose`, with the key it names.
    ///
    /// # Errors
    /// [`VaultError::Malformed`], [`VaultError::UnknownKey`] or
    /// [`VaultError::Rejected`].
    fn open(
        &self,
        purpose: VaultPurpose,
        record: Uuid,
        envelope: &[u8],
    ) -> Result<SecretBox<Vec<u8>>, VaultError>;
}
