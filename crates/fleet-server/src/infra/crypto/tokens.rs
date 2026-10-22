//! [`Sha256Tokens`]: the session tokens (Plan.md P7.3, §7.2, ADR-0006,
//! ADR-0016).
//!
//! - **Minting:** 32 bytes from the [`SecureRandom`] port, base64url without
//!   padding (43 characters) behind the kind's prefix: 50 ASCII characters.
//!   The random bytes and the text are built in `Zeroizing` buffers of fixed
//!   size, and the token is copied once into a `SecretString` of exactly its
//!   length, so no unwiped copy is left behind.
//! - **Hashing:** SHA-256 of the whole text, prefix included, so the same
//!   bytes as an access and as a refresh token hash differently. sha2's
//!   `zeroize` feature wipes the hasher's buffer.
//! - **Recognizing:** a presented token must be its kind's prefix and exactly
//!   43 base64url characters; anything else is refused before it's hashed,
//!   let alone looked up.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fleet_core::system::SecureRandom;
use secrecy::SecretString;
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::ports::tokens::{MintTokenError, MintedToken, TokenHash, TokenKind, Tokens};

/// The random bytes of a token: 256 bits.
const SECRET_BYTES: usize = 32;

/// Their base64url text, without padding.
const SECRET_CHARS: usize = 43;

/// A prefix's length: both are 7 characters.
const PREFIX_CHARS: usize = 7;

/// A whole token's length.
const TOKEN_CHARS: usize = PREFIX_CHARS + SECRET_CHARS;

/// The session tokens over the OS's randomness (production) or a seeded
/// source (tests).
#[derive(Debug, Clone)]
pub struct Sha256Tokens {
    random: Arc<dyn SecureRandom>,
}

impl Sha256Tokens {
    /// Tokens drawn from `random`.
    #[must_use]
    pub fn new(random: Arc<dyn SecureRandom>) -> Self {
        Self { random }
    }
}

impl Tokens for Sha256Tokens {
    fn mint(&self, kind: TokenKind) -> Result<MintedToken, MintTokenError> {
        let mut secret = Zeroizing::new([0_u8; SECRET_BYTES]);
        self.random
            .fill(secret.as_mut_slice())
            .map_err(MintTokenError::Random)?;
        let mut text = Zeroizing::new([0_u8; TOKEN_CHARS]);
        let (prefix, encoded) = text
            .split_at_mut_checked(PREFIX_CHARS)
            .ok_or(MintTokenError::Encoding)?;
        for (byte, prefix_byte) in prefix.iter_mut().zip(kind.prefix().bytes()) {
            *byte = prefix_byte;
        }
        let written = URL_SAFE_NO_PAD
            .encode_slice(secret.as_slice(), encoded)
            .map_err(|_| MintTokenError::Encoding)?;
        if written != SECRET_CHARS {
            return Err(MintTokenError::Encoding);
        }
        let text = core::str::from_utf8(text.as_slice()).map_err(|_| MintTokenError::Encoding)?;
        Ok(MintedToken {
            // `to_owned` allocates exactly the text's length, so boxing it
            // into the secret never reallocates.
            token: SecretString::from(text.to_owned()),
            hash: hash(text),
        })
    }

    fn hash_presented(&self, kind: TokenKind, presented: &str) -> Option<TokenHash> {
        let secret = presented.strip_prefix(kind.prefix())?;
        (secret.len() == SECRET_CHARS && secret.bytes().all(is_base64url)).then(|| hash(presented))
    }
}

/// The SHA-256 of a token's whole text.
fn hash(text: &str) -> TokenHash {
    TokenHash::from_bytes(Sha256::digest(text.as_bytes()).into())
}

/// Whether `byte` is in the base64url alphabet: `A`–`Z`, `a`–`z`, `0`–`9`,
/// `-` and `_`.
const fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

#[cfg(test)]
mod tests {
    use fleet_core::system::RandomError;
    use fleet_testkit::system::SeededRandom;
    use rstest::rstest;
    use secrecy::ExposeSecret as _;

    use super::*;

    fn tokens(seed: u64) -> Sha256Tokens {
        Sha256Tokens::new(Arc::new(SeededRandom::new(seed)))
    }

    /// 32 bytes from 64 hex digits.
    fn hex(text: &str) -> [u8; 32] {
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
        }
        bytes
    }

    #[rstest]
    #[case::access(TokenKind::Access, "afk_at_")]
    #[case::refresh(TokenKind::Refresh, "afk_rt_")]
    #[case::invite(TokenKind::Invite, "afk_iv_")]
    #[case::enrollment(TokenKind::Enrollment, "afk_et_")]
    #[case::event_ticket(TokenKind::EventTicket, "afk_wt_")]
    fn a_minted_token_is_its_prefix_and_43_base64url_characters(
        #[case] kind: TokenKind,
        #[case] prefix: &str,
    ) {
        let minted = tokens(1).mint(kind).unwrap();

        let text = minted.token.expose_secret();
        assert_eq!(text.len(), 50, "{text}");
        let secret = text.strip_prefix(prefix).unwrap();
        assert!(secret.bytes().all(is_base64url), "{text}");
    }

    #[test]
    fn a_minted_enrollment_token_is_the_shape_an_enrollment_bundle_takes() {
        for seed in 0..32 {
            let minted = tokens(seed).mint(TokenKind::Enrollment).unwrap();

            assert!(
                fleet_core::enrollment::is_token_shaped(minted.token.expose_secret()),
                "seed {seed}"
            );
        }
    }

    #[test]
    fn the_token_carries_32_bytes_from_the_random_source() {
        let minted = tokens(7).mint(TokenKind::Access).unwrap();
        let mut expected = [0; 32];
        SeededRandom::new(7).fill(&mut expected).unwrap();

        let mut decoded = [0; 32];
        let len = URL_SAFE_NO_PAD
            .decode_slice(&minted.token.expose_secret()[7..], &mut decoded)
            .unwrap();

        assert_eq!(len, 32);
        assert_eq!(decoded, expected);
    }

    #[test]
    fn each_mint_draws_new_bytes() {
        let tokens = tokens(3);

        let first = tokens.mint(TokenKind::Refresh).unwrap();
        let second = tokens.mint(TokenKind::Refresh).unwrap();

        assert_ne!(first.token.expose_secret(), second.token.expose_secret());
        assert!(!first.hash.ct_eq(&second.hash));
    }

    #[test]
    fn a_minted_tokens_hash_is_the_hash_of_its_text() {
        let tokens = tokens(5);
        let minted = tokens.mint(TokenKind::Access).unwrap();

        let presented = tokens
            .hash_presented(TokenKind::Access, minted.token.expose_secret())
            .unwrap();

        assert!(presented.ct_eq(&minted.hash));
    }

    /// Known answers, computed with Python's hashlib and Node's crypto.
    #[rstest]
    #[case::access(
        TokenKind::Access,
        "afk_at_",
        "33d8623ba077249d97971776e1e9dcaefc0ffb754c3d61cc40d0daa8a5edf98d"
    )]
    #[case::refresh(
        TokenKind::Refresh,
        "afk_rt_",
        "4b97486afce7fe2c6d0f2352d5e8667afa68137739bb0834d0c787e291d6d8b8"
    )]
    fn the_hash_is_sha256_of_the_whole_text(
        #[case] kind: TokenKind,
        #[case] prefix: &str,
        #[case] sha256: &str,
    ) {
        let text = format!("{prefix}{}", "A".repeat(43));

        let hash = tokens(1).hash_presented(kind, &text).unwrap();

        assert_eq!(hash.as_bytes(), &hex(sha256));
    }

    #[test]
    fn a_token_presented_as_the_other_kind_is_refused() {
        let tokens = tokens(9);
        let refresh = tokens.mint(TokenKind::Refresh).unwrap();

        assert!(
            tokens
                .hash_presented(TokenKind::Access, refresh.token.expose_secret())
                .is_none()
        );
    }

    #[rstest]
    #[case::empty("")]
    #[case::prefix_only("afk_at_")]
    #[case::one_short("afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")]
    #[case::one_long("afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")]
    #[case::plus("afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA+")]
    #[case::slash("afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/")]
    #[case::padding("afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")]
    #[case::space("afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ")]
    #[case::leading_space(" afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")]
    #[case::uppercase_prefix("AFK_AT_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")]
    #[case::non_ascii("afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé")]
    fn anything_not_shaped_like_a_token_is_refused(#[case] presented: &str) {
        assert!(
            tokens(1)
                .hash_presented(TokenKind::Access, presented)
                .is_none(),
            "{presented:?}"
        );
    }

    #[test]
    fn both_url_safe_symbols_are_accepted() {
        let presented = format!("afk_rt_{}-_", "A".repeat(41));

        assert!(
            tokens(1)
                .hash_presented(TokenKind::Refresh, &presented)
                .is_some()
        );
    }

    #[test]
    fn a_failing_random_source_mints_nothing() {
        let random = SeededRandom::new(1);
        random.fail_next(RandomError::Unavailable);
        let tokens = Sha256Tokens::new(Arc::new(random));

        let minted = tokens.mint(TokenKind::Access);

        assert!(
            matches!(
                minted,
                Err(MintTokenError::Random(RandomError::Unavailable))
            ),
            "{minted:?}"
        );
    }

    #[test]
    fn debug_of_a_minted_token_shows_no_part_of_it() {
        let minted = tokens(11).mint(TokenKind::Access).unwrap();
        let text = minted.token.expose_secret().to_owned();

        let debug = format!("{minted:?}");

        let secret = &text[7..];
        assert!(!debug.contains(&text), "{debug}");
        for start in 0..=secret.len() - 8 {
            let window = &secret[start..start + 8];
            assert!(!debug.contains(window), "{window} in {debug}");
        }
    }
}
