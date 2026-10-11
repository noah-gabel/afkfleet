//! [`Sha256RecoveryCodes`]: the recovery codes' hashes (Plan.md P7.7,
//! ADR-0016): plain SHA-256 of the normalized code.
//!
//! Plain, not keyed, on purpose: each code carries 100 bits from the
//! `SecureRandom` port, so a leaked database can't be brute-forced offline,
//! and a key derived from the vault's would make every code unusable after
//! a key rotation, since the hashes can't be recomputed without the codes.

use fleet_core::auth::RecoveryCode;
use sha2::{Digest as _, Sha256};

use crate::ports::recovery_codes::{RecoveryCodeHash, RecoveryCodes};

/// SHA-256 of a recovery code's normalized text.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sha256RecoveryCodes;

impl Sha256RecoveryCodes {
    /// The adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl RecoveryCodes for Sha256RecoveryCodes {
    fn hash(&self, code: &RecoveryCode) -> RecoveryCodeHash {
        RecoveryCodeHash::from_bytes(Sha256::digest(code.normalized().as_bytes()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code `abz27a7ahijklmnopqrs`, typed as shown.
    fn code() -> RecoveryCode {
        RecoveryCode::parse("ABZ2-7A7A-HIJK-LMNO-PQRS").unwrap()
    }

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        })
    }

    /// SHA-256 of `abz27a7ahijklmnopqrs`, computed with Python's hashlib and
    /// Node's crypto.
    #[test]
    fn the_hash_is_sha256_of_the_normalized_text() {
        let hash = Sha256RecoveryCodes.hash(&code());

        assert_eq!(
            hex(hash.as_bytes()),
            "f796a4a72445192d70a8dcacdd8d07a990736b47c6bbccba7a099cf16e2c7dc1"
        );
    }

    #[test]
    fn the_same_code_typed_differently_has_the_same_hash() {
        let typed = RecoveryCode::parse("abz2 7a7a hijk lmno pqrs").unwrap();

        assert!(
            Sha256RecoveryCodes
                .hash(&typed)
                .ct_eq(&Sha256RecoveryCodes.hash(&code()))
        );
    }

    #[test]
    fn another_code_has_another_hash() {
        let other = RecoveryCode::parse("abz2-7a7a-hijk-lmno-pqrt").unwrap();

        assert!(
            !Sha256RecoveryCodes
                .hash(&other)
                .ct_eq(&Sha256RecoveryCodes.hash(&code()))
        );
    }
}
