//! TOTP's crypto (Plan.md P7.7, §7.2, ADR-0016): [`TotpSecret`] and
//! [`TotpCodes`], which computes a time step's code and the otpauth URI.
//!
//! Which steps to try, and that none is accepted twice, is decided by
//! `fleet_core::auth::candidate_steps`; this port only knows HMAC-SHA1 and
//! the URI's format. The only implementation is infra's `TotpRs`.

use core::fmt;

use fleet_core::auth::{TOTP_SECRET_LEN, TotpCode};
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, ExposeSecretMut as _, SecretBox, SecretString};

/// A TOTP secret: [`TOTP_SECRET_LEN`] bytes, wiped on drop, never in
/// `Debug`.
pub struct TotpSecret(SecretBox<[u8; TOTP_SECRET_LEN]>);

/// Why bytes aren't a [`TotpSecret`]: a stored secret of another length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a TOTP secret has {expected} bytes, not {len}")]
pub struct TotpSecretLengthError {
    /// The length found.
    pub len: usize,
    /// [`TOTP_SECRET_LEN`].
    pub expected: usize,
}

impl TotpSecret {
    /// Wraps fresh random bytes.
    #[must_use]
    pub fn new(bytes: SecretBox<[u8; TOTP_SECRET_LEN]>) -> Self {
        Self(bytes)
    }

    /// Copies an opened secret's bytes, as the vault returns them; the
    /// caller's copy is wiped when it drops.
    ///
    /// # Errors
    /// [`TotpSecretLengthError`] for any other length.
    pub fn from_opened(bytes: &[u8]) -> Result<Self, TotpSecretLengthError> {
        let mut secret = SecretBox::new(Box::new([0_u8; TOTP_SECRET_LEN]));
        let target = secret.expose_secret_mut();
        if bytes.len() != target.len() {
            return Err(TotpSecretLengthError {
                len: bytes.len(),
                expected: TOTP_SECRET_LEN,
            });
        }
        target.copy_from_slice(bytes);
        Ok(Self(secret))
    }

    /// The bytes, for the TOTP adapter and the vault.
    #[must_use]
    pub fn expose(&self) -> &[u8; TOTP_SECRET_LEN] {
        self.0.expose_secret()
    }
}

/// Redacted.
impl fmt::Debug for TotpSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TotpSecret(..)")
    }
}

/// Why the otpauth URI couldn't be built. It can't happen for a valid
/// `Username`, which has no `:`; the adapter maps the crate's error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the otpauth URI couldn't be built")]
pub struct TotpUriError;

/// TOTP's crypto (RFC 6238 with SHA-1, 6 digits and 30-second steps).
/// Microseconds of work, so it's synchronous.
pub trait TotpCodes: Send + Sync + fmt::Debug {
    /// Whether `code` is `secret`'s code for time step `step`, compared in
    /// constant time.
    fn matches(&self, secret: &TotpSecret, step: u64, code: &TotpCode) -> bool;

    /// The `otpauth://totp/afkfleet:<username>?…` URI an authenticator app
    /// scans, holding `secret`.
    ///
    /// # Errors
    /// [`TotpUriError`], which can't happen for a valid `Username`.
    fn otpauth_uri(
        &self,
        secret: &TotpSecret,
        username: &Username,
    ) -> Result<SecretString, TotpUriError>;

    /// `secret` as base32 without padding, for typing it in by hand.
    fn base32(&self, secret: &TotpSecret) -> SecretString;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opened_secret_of_the_right_length_is_copied() {
        let bytes = [7_u8; TOTP_SECRET_LEN];

        assert_eq!(TotpSecret::from_opened(&bytes).unwrap().expose(), &bytes);
    }

    #[test]
    fn any_other_length_is_refused() {
        for len in [0, 19, 21] {
            assert_eq!(
                TotpSecret::from_opened(&vec![1; len]).err(),
                Some(TotpSecretLengthError { len, expected: 20 })
            );
        }
    }

    #[test]
    fn debug_shows_no_byte() {
        let secret = TotpSecret::new(SecretBox::new(Box::new([0xAB; TOTP_SECRET_LEN])));

        assert_eq!(format!("{secret:?}"), "TotpSecret(..)");
    }
}
