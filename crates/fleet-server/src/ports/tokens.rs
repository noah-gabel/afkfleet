//! The tokens (Plan.md P7.3, §7.2, ADR-0006, ADR-0016): [`Tokens`], which
//! makes and recognizes them, and [`TokenHash`], the only form of a token
//! the database ever holds.
//!
//! A token is opaque: 256 random bits from the `SecureRandom` port,
//! base64url without padding, behind its kind's prefix ([`TokenKind`]): a
//! session's `afk_at_` and `afk_rt_`, the login's challenges `afk_mt_` and
//! `afk_st_`, and the invites' `afk_iv_`, so secret scanners spot them. Only their SHA-256 hash is
//! stored, and a presented token is compared with a stored hash in constant
//! time ([`TokenHash::ct_eq`]). The hash isn't a secret a client could use,
//! but its `Debug` shows none of its bytes all the same, and no type here
//! shows a token.

use core::fmt;

use fleet_core::system::RandomError;
use secrecy::SecretString;
use subtle::ConstantTimeEq as _;

/// Which kind of token: a session's two, one of the login's challenges
/// (group D), or an invite's code (group E).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// The access token, sent with every request: `afk_at_…`.
    Access,
    /// The refresh token, sent only to refresh: `afk_rt_…`.
    Refresh,
    /// The password step's single-use token for the code step: `afk_mt_…`.
    Mfa,
    /// The token for enrolling or replacing a TOTP authenticator:
    /// `afk_st_…`.
    Setup,
    /// An invite's code, for registering: `afk_iv_…`.
    Invite,
}

impl TokenKind {
    /// The prefix the token's text starts with.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Access => "afk_at_",
            Self::Refresh => "afk_rt_",
            Self::Mfa => "afk_mt_",
            Self::Setup => "afk_st_",
            Self::Invite => "afk_iv_",
        }
    }
}

/// A freshly minted token and its hash. Its `Debug` shows neither: the
/// token is a [`SecretString`], the hash a [`TokenHash`].
#[derive(Debug)]
pub struct MintedToken {
    /// The token's text, for the client only.
    pub token: SecretString,
    /// Its hash, for the database.
    pub hash: TokenHash,
}

/// Why a token couldn't be minted.
#[derive(Debug, thiserror::Error)]
pub enum MintTokenError {
    /// The random source failed.
    #[error("no random bytes for the token")]
    Random(#[source] RandomError),
    /// The token's text couldn't be written. It can't happen: the buffer
    /// always fits 32 bytes in base64url, and the text is ASCII.
    #[error("the token's text can't be written")]
    Encoding,
}

/// Making and recognizing tokens (§7.2). The only implementation is infra's
/// `Sha256Tokens`; it takes microseconds, so it's synchronous.
pub trait Tokens: Send + Sync + fmt::Debug {
    /// Mints a new token of `kind` from 256 random bits.
    ///
    /// # Errors
    /// [`MintTokenError::Random`] when the random source fails, and
    /// [`MintTokenError::Encoding`], which can't happen.
    fn mint(&self, kind: TokenKind) -> Result<MintedToken, MintTokenError>;

    /// The hash of a presented token of `kind`, or `None` when it isn't
    /// shaped like one (its prefix, then exactly 43 base64url characters), so
    /// it's refused before any lookup.
    fn hash_presented(&self, kind: TokenKind, presented: &str) -> Option<TokenHash>;
}

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
    fn the_prefixes_are_pinned() {
        assert_eq!(TokenKind::Access.prefix(), "afk_at_");
        assert_eq!(TokenKind::Refresh.prefix(), "afk_rt_");
        assert_eq!(TokenKind::Mfa.prefix(), "afk_mt_");
        assert_eq!(TokenKind::Setup.prefix(), "afk_st_");
        assert_eq!(TokenKind::Invite.prefix(), "afk_iv_");
    }

    #[test]
    fn debug_shows_no_byte() {
        let hash = TokenHash::from_bytes([0xAB; 32]);

        let debug = format!("{hash:?}");

        assert_eq!(debug, "TokenHash(..)");
    }
}
