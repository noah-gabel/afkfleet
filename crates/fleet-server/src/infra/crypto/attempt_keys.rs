//! [`HmacAttemptKeys`]: the keys of the `login_attempts` rows (Plan.md
//! P7.10, ADR-0016), and `hmac_sha256`, the HMAC they and the vault's key
//! derivation share.
//!
//! A row's key is HMAC-SHA256 of the normalized username under the lockout
//! key, which the vault derives at startup from its active key, so the name
//! is never stored, and a leaked database or backup doesn't let anyone test
//! guesses offline: the key is in neither.

use core::fmt;

use fleet_core::value::Username;
use hmac::{Hmac, KeyInit as _, Mac as _};
use secrecy::{ExposeSecret as _, SecretBox};
use sha2::Sha256;
use zeroize::{Zeroize as _, Zeroizing};

use crate::ports::login_attempts::{AttemptKey, AttemptKeys};

/// HMAC-SHA256's block, the length its key is padded to.
const BLOCK_BYTES: usize = 64;

/// HMAC-SHA256 of `message` under the 32-byte `key`, written into `out`.
///
/// RFC 2104 pads a key shorter than the hash's 64-byte block with zeros, so
/// the key is passed as that zero-padded block: the same HMAC, and a
/// construction that can't fail. The known-answer tests, computed with
/// Python's `hmac` and Node's `crypto`, confirm it. The padded block and the
/// tag are wiped; the keyed state is wiped on drop (hmac's `zeroize`).
pub(crate) fn hmac_sha256(key: &[u8; 32], message: &[u8], out: &mut [u8; 32]) {
    let block = Zeroizing::new(core::array::from_fn::<u8, BLOCK_BYTES, _>(|index| {
        key.get(index).copied().unwrap_or(0)
    }));
    let mut mac = Hmac::<Sha256>::new((&*block).into());
    mac.update(message);
    let mut tag = mac.finalize().into_bytes();
    out.copy_from_slice(&tag);
    tag.as_mut_slice().zeroize();
}

/// The keys of the `login_attempts` rows: HMAC-SHA256 of the normalized
/// username under the lockout key. Its `Debug` shows no key.
pub struct HmacAttemptKeys {
    key: SecretBox<[u8; 32]>,
}

impl HmacAttemptKeys {
    /// The keys under `key`, the lockout key the vault derives.
    #[must_use]
    pub const fn new(key: SecretBox<[u8; 32]>) -> Self {
        Self { key }
    }
}

impl fmt::Debug for HmacAttemptKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HmacAttemptKeys").finish_non_exhaustive()
    }
}

impl AttemptKeys for HmacAttemptKeys {
    fn key(&self, username: &Username) -> AttemptKey {
        let mut out = [0_u8; AttemptKey::LEN];
        hmac_sha256(
            self.key.expose_secret(),
            username.as_str().as_bytes(),
            &mut out,
        );
        AttemptKey::from_bytes(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derived lockout key for the vault key `[0x42; 32]`, computed with
    /// Python's `hmac` and Node's `crypto`.
    const DERIVED: &str = "b1712d4846d39fd7e27355abfc46370f5074be7dc4174ba670db7da2186194e0";

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;

        bytes.iter().fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        })
    }

    fn unhex(text: &str) -> [u8; 32] {
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect();
        bytes.try_into().unwrap()
    }

    fn keys() -> HmacAttemptKeys {
        HmacAttemptKeys::new(SecretBox::new(Box::new(unhex(DERIVED))))
    }

    fn username(text: &str) -> Username {
        Username::try_from(text).unwrap()
    }

    #[test]
    fn the_hmac_is_hmac_sha256_with_the_whole_key() {
        let mut out = [0_u8; 32];

        hmac_sha256(&[0x0b; 32], b"Hi There", &mut out);

        assert_eq!(
            hex(&out),
            "198a607eb44bfbc69903a0f1cf2bbdc5ba0aa3f3d9ae3c1c7a3b1696a0b68cf7"
        );
    }

    #[test]
    fn the_lockout_key_derivation_is_pinned() {
        let mut out = [0_u8; 32];

        hmac_sha256(&[0x42; 32], b"afkfleet login_attempts v1", &mut out);

        assert_eq!(hex(&out), DERIVED);
    }

    #[test]
    fn a_rows_key_is_the_hmac_of_the_normalized_name() {
        assert_eq!(
            hex(keys().key(&username("alice")).as_bytes()),
            "41d67f6248c9b7c6999555b749b4a6e0463413f1498a5b1497a99a1289cc450f"
        );
        assert_eq!(
            hex(keys().key(&username("bob")).as_bytes()),
            "cd14573f83f00506bd8ab528b3ee0e4e82a2c86f9597d8a872e4bad47b6959ed"
        );
    }

    #[test]
    fn every_spelling_of_a_name_has_one_key() {
        let lower = keys().key(&username("alice"));

        assert!(lower.ct_eq(&keys().key(&username("ALICE"))));
        assert!(lower.ct_eq(&keys().key(&username("Alice"))));
    }

    #[test]
    fn another_lockout_key_gives_other_keys() {
        let other = HmacAttemptKeys::new(SecretBox::new(Box::new([7; 32])));

        assert!(
            !keys()
                .key(&username("alice"))
                .ct_eq(&other.key(&username("alice")))
        );
    }

    #[test]
    fn the_debug_form_shows_no_key() {
        let shown = format!("{:?}", keys());

        assert_eq!(shown, "HmacAttemptKeys { .. }");
    }
}
