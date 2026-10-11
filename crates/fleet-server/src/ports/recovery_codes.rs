//! The recovery codes (Plan.md P7.7, §7.2, ADR-0016): [`RecoveryCodeHash`],
//! the only form of a recovery code the database holds.
//!
//! A recovery code is 20 base32 characters (100 bits) from the
//! `SecureRandom` port, shown once. Only the SHA-256 of its normalized text
//! is stored; a presented code is compared with a stored hash in constant
//! time ([`RecoveryCodeHash::ct_eq`]). The hash's `Debug` shows none of its
//! bytes.

use core::fmt;

use subtle::ConstantTimeEq as _;

/// The SHA-256 hash of a recovery code's normalized text.
///
/// It has no `PartialEq`: two hashes are compared only through
/// [`RecoveryCodeHash::ct_eq`], in constant time.
#[derive(Clone, Copy)]
pub struct RecoveryCodeHash([u8; RecoveryCodeHash::LEN]);

/// Why stored bytes aren't a [`RecoveryCodeHash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a recovery code hash has {expected} bytes, not {len}")]
pub struct RecoveryCodeHashLengthError {
    /// The length found.
    pub len: usize,
    /// [`RecoveryCodeHash::LEN`].
    pub expected: usize,
}

impl RecoveryCodeHash {
    /// A SHA-256 hash's length in bytes.
    pub const LEN: usize = 32;

    /// Wraps a hash.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The hash's bytes, as the database stores them.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Whether both hashes are equal, compared in constant time.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

/// For reading a stored hash back.
impl TryFrom<&[u8]> for RecoveryCodeHash {
    type Error = RecoveryCodeHashLengthError;

    fn try_from(bytes: &[u8]) -> Result<Self, RecoveryCodeHashLengthError> {
        <[u8; Self::LEN]>::try_from(bytes)
            .map(Self)
            .map_err(|_| RecoveryCodeHashLengthError {
                len: bytes.len(),
                expected: Self::LEN,
            })
    }
}

/// Shows none of the hash's bytes.
impl fmt::Debug for RecoveryCodeHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryCodeHash(..)")
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn equal_hashes_compare_equal() {
        let a = RecoveryCodeHash::from_bytes([7; 32]);
        let b = RecoveryCodeHash::from_bytes([7; 32]);

        assert!(a.ct_eq(&b));
    }

    #[rstest]
    #[case::first_byte(0)]
    #[case::last_byte(31)]
    fn one_different_byte_compares_unequal(#[case] index: usize) {
        let a = RecoveryCodeHash::from_bytes([7; 32]);
        let mut bytes = [7; 32];
        bytes[index] = 8;

        assert!(!a.ct_eq(&RecoveryCodeHash::from_bytes(bytes)));
    }

    #[test]
    fn stored_bytes_read_back() {
        let bytes = [9_u8; 32];

        let hash = RecoveryCodeHash::try_from(bytes.as_slice()).unwrap();

        assert_eq!(hash.as_bytes(), &bytes);
    }

    #[rstest]
    #[case::empty(0)]
    #[case::one_short(31)]
    #[case::one_long(33)]
    fn any_other_length_is_refused(#[case] len: usize) {
        let bytes = vec![1_u8; len];

        assert_eq!(
            RecoveryCodeHash::try_from(bytes.as_slice()).err(),
            Some(RecoveryCodeHashLengthError { len, expected: 32 })
        );
    }

    #[test]
    fn debug_shows_no_byte() {
        let hash = RecoveryCodeHash::from_bytes([0xAB; 32]);

        let debug = format!("{hash:?}");

        assert_eq!(debug, "RecoveryCodeHash(..)");
    }
}
