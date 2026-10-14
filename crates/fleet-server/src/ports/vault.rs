//! The vault's port: [`Vault`] encrypts the secrets the server stores, bound
//! to their record and purpose (Plan.md §7.5, P7.4, P9.2, ADR-0016,
//! ADR-0018).
//!
//! TOTP secrets go through it (P7.7), and Microsoft refresh tokens (P9,
//! security rule 6). Its errors name no crate's type.
//!
//! # The envelope's header
//! Every sealed secret is a version 1 envelope: `0x01 | key_id (u32,
//! big-endian) | nonce (24 bytes) | ciphertext + tag (16 bytes)`. The header
//! names the key that sealed it, so [`key_id_of`] reads that without any key:
//! `vault status` counts what each key still seals, and `vault rotate-key`
//! picks the secrets under other keys than the active one.

use core::fmt;

use fleet_core::system::RandomError;
use secrecy::SecretBox;
use uuid::Uuid;

/// What a sealed secret is for. Its name is part of the envelope's AAD, so
/// a secret sealed for one purpose never opens as another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VaultPurpose {
    /// A user's TOTP secret, bound to the user's ID (P7.7).
    TotpSecret,
    /// A Microsoft account's refresh token, bound to the account's ID
    /// (P9.2).
    MsRefreshToken,
}

impl VaultPurpose {
    /// The name bound into the AAD. Renaming one makes every secret of that
    /// purpose undecryptable, so a test pins each.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TotpSecret => "totp_secret",
            Self::MsRefreshToken => "ms_refresh",
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
    /// wrong key, record or purpose. These look the same on purpose; the
    /// key ID the envelope names is kept for the operator's log.
    #[error("the envelope doesn't authenticate under key {key_id}")]
    Rejected {
        /// The key ID in the envelope.
        key_id: u32,
    },
    /// The OS's randomness failed while drawing the nonce.
    #[error("no nonce could be drawn")]
    Random(#[from] RandomError),
    /// The plaintext is beyond the cipher's limit (about 256 GiB), which no
    /// secret the server stores comes near.
    #[error("the plaintext is too large to seal")]
    TooLarge,
}

/// The envelope's version byte.
pub const VERSION_1: u8 = 0x01;
/// The header: the version byte and the key ID.
pub const HEADER_BYTES: usize = 1 + 4;
/// What an envelope adds to its plaintext: the header, the 24-byte nonce
/// and the 16-byte tag. A shorter envelope is malformed.
pub const OVERHEAD: usize = HEADER_BYTES + 24 + 16;

/// The ID of the key that sealed `envelope`, read from its header without
/// opening it.
///
/// # Errors
/// [`VaultError::Malformed`] for an envelope shorter than [`OVERHEAD`] or of
/// another version than 1, exactly the envelopes [`Vault::open`] calls
/// malformed.
pub fn key_id_of(envelope: &[u8]) -> Result<u32, VaultError> {
    if envelope.len() < OVERHEAD {
        return Err(VaultError::Malformed);
    }
    match envelope.get(..HEADER_BYTES) {
        Some(&[VERSION_1, a, b, c, d]) => Ok(u32::from_be_bytes([a, b, c, d])),
        _ => Err(VaultError::Malformed),
    }
}

/// The header of a version 1 envelope sealed under `key_id`: the bytes an
/// envelope under that key starts with.
#[must_use]
pub const fn header_of(key_id: u32) -> [u8; HEADER_BYTES] {
    let [a, b, c, d] = key_id.to_be_bytes();
    [VERSION_1, a, b, c, d]
}

/// Encrypting secrets at rest (§7.5): `seal` binds a secret to the record
/// that stores it and to its purpose, and `open` checks both.
pub trait Vault: Send + Sync + fmt::Debug {
    /// The ID of the key [`Vault::seal`] seals with.
    fn active_key_id(&self) -> u32;

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

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// A version 1 envelope's bytes: the header for `key_id`, then `rest`
    /// bytes of `0xAB`.
    fn envelope(key_id: u32, rest: usize) -> Vec<u8> {
        let mut bytes = vec![VERSION_1];
        bytes.extend_from_slice(&key_id.to_be_bytes());
        bytes.extend(core::iter::repeat_n(0xAB, rest));
        bytes
    }

    #[rstest]
    #[case::one(1)]
    #[case::big_endian(0x0102_0304)]
    #[case::largest(u32::MAX)]
    fn the_key_id_is_read_from_the_header(#[case] key_id: u32) {
        let sealed = envelope(key_id, OVERHEAD - HEADER_BYTES + 20);

        assert_eq!(key_id_of(&sealed).unwrap(), key_id);
        assert_eq!(
            sealed.get(..HEADER_BYTES),
            Some(header_of(key_id).as_slice())
        );
    }

    #[test]
    fn the_shortest_envelope_has_no_plaintext() {
        assert_eq!(OVERHEAD, 45);
        assert_eq!(key_id_of(&envelope(7, OVERHEAD - HEADER_BYTES)).unwrap(), 7);
    }

    #[rstest]
    #[case::empty(Vec::new())]
    #[case::header_only(envelope(7, 0))]
    #[case::one_short(envelope(7, OVERHEAD - HEADER_BYTES - 1))]
    fn a_short_envelope_is_malformed(#[case] sealed: Vec<u8>) {
        assert!(
            matches!(key_id_of(&sealed), Err(VaultError::Malformed)),
            "{} bytes",
            sealed.len()
        );
    }

    #[rstest]
    #[case::zero(0)]
    #[case::two(2)]
    fn another_version_is_malformed(#[case] version: u8) {
        let mut sealed = envelope(7, OVERHEAD);
        sealed[0] = version;

        assert!(matches!(key_id_of(&sealed), Err(VaultError::Malformed)));
    }
}
