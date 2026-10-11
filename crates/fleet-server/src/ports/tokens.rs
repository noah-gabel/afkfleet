//! The tokens (Plan.md P7.3, §7.2, ADR-0006, ADR-0016): [`TokenHash`], the
//! only form of a token the database ever holds.
//!
//! A session's tokens are opaque: 256 random bits, base64url, behind the
//! prefix `afk_at_` or `afk_rt_`. Only their SHA-256 hash is stored, and a
//! presented token is compared with a stored hash in constant time
//! ([`TokenHash::ct_eq`]). The hash isn't a secret a client could use, but
//! its `Debug` shows none of its bytes all the same.

use core::fmt;

use subtle::ConstantTimeEq as _;

/// The SHA-256 hash of a token's text, prefix included.
///
/// It has no `PartialEq`: two hashes are compared only through
/// [`TokenHash::ct_eq`], in constant time.
#[derive(Clone, Copy)]
pub struct TokenHash([u8; TokenHash::LEN]);

/// Why stored bytes aren't a [`TokenHash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a token hash has {expected} bytes, not {len}")]
pub struct TokenHashLengthError {
    /// The length found.
    pub len: usize,
    /// [`TokenHash::LEN`].
    pub expected: usize,
}

impl TokenHash {
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
impl TryFrom<&[u8]> for TokenHash {
    type Error = TokenHashLengthError;

    fn try_from(bytes: &[u8]) -> Result<Self, TokenHashLengthError> {
        <[u8; Self::LEN]>::try_from(bytes)
            .map(Self)
            .map_err(|_| TokenHashLengthError {
                len: bytes.len(),
                expected: Self::LEN,
            })
    }
}

/// Shows none of the hash's bytes.
impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenHash(..)")
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn equal_hashes_compare_equal() {
        let a = TokenHash::from_bytes([7; 32]);
        let b = TokenHash::from_bytes([7; 32]);

        assert!(a.ct_eq(&b));
    }

    #[rstest]
    #[case::first_byte(0)]
    #[case::last_byte(31)]
    fn one_different_byte_compares_unequal(#[case] index: usize) {
        let a = TokenHash::from_bytes([7; 32]);
        let mut bytes = [7; 32];
        bytes[index] = 8;

        assert!(!a.ct_eq(&TokenHash::from_bytes(bytes)));
    }

    #[test]
    fn stored_bytes_read_back() {
        let bytes = [9_u8; 32];

        let hash = TokenHash::try_from(bytes.as_slice()).unwrap();

        assert_eq!(hash.as_bytes(), &bytes);
    }

    #[rstest]
    #[case::empty(0)]
    #[case::one_short(31)]
    #[case::one_long(33)]
    fn any_other_length_is_refused(#[case] len: usize) {
        let bytes = vec![1_u8; len];

        assert_eq!(
            TokenHash::try_from(bytes.as_slice()).err(),
            Some(TokenHashLengthError { len, expected: 32 })
        );
    }

    #[test]
    fn debug_shows_no_byte() {
        let hash = TokenHash::from_bytes([0xAB; 32]);

        let debug = format!("{hash:?}");

        assert_eq!(debug, "TokenHash(..)");
    }
}
