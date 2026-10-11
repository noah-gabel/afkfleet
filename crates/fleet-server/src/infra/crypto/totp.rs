//! [`TotpRs`]: TOTP's crypto through totp-rs (Plan.md P7.7, ADR-0016):
//! RFC 6238 with SHA-1, 6 digits and 30-second steps, one step at a time.
//!
//! totp-rs is built without `std`, so it never reads the wall clock: the
//! time step comes from `fleet_core::auth`, which decides which steps a
//! code may match. Its `zeroize` feature wipes the copy of the secret each
//! `Totp` holds. A step's code is compared with the typed one through
//! `subtle`, and the formatted code is wiped too; a `Token` is never
//! formatted with `Debug`, which would show it.

use fleet_core::auth::{TOTP_DIGITS, TOTP_ISSUER, TotpCode, step_start};
use fleet_core::value::Username;
use secrecy::SecretString;
use subtle::ConstantTimeEq as _;
use totp_rs::{Algorithm, Builder, Secret, Totp};
use zeroize::Zeroizing;

use crate::ports::totp::{TotpCodes, TotpSecret, TotpUriError};

/// TOTP through totp-rs. It holds nothing; each call builds its `Totp`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TotpRs;

impl TotpRs {
    /// The adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// A `Totp` of `secret` with afkfleet's parameters, and `username` as its
/// account when there is one. `None` if totp-rs refuses them, which it
/// can't for a 160-bit secret, 6 digits, 30 s and a name without `:`.
fn totp(secret: &TotpSecret, username: Option<&Username>) -> Option<Totp> {
    let builder = Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(u8::try_from(TOTP_DIGITS).ok()?)
        .with_skew(0)
        .with_step_duration(30)
        .with_secret(Secret::from(secret.expose().as_slice()))
        .with_issuer(Some(TOTP_ISSUER));
    let builder = match username {
        Some(username) => builder.with_account_name(username.as_str()),
        None => builder,
    };
    builder.build().ok()
}

impl TotpCodes for TotpRs {
    fn matches(&self, secret: &TotpSecret, step: u64, code: &TotpCode) -> bool {
        let (Some(totp), Some(time)) = (totp(secret, None), step_start(step)) else {
            return false;
        };
        let expected = Zeroizing::new(totp.generate(time).to_string());
        expected.as_bytes().ct_eq(code.digits()).into()
    }

    fn otpauth_uri(
        &self,
        secret: &TotpSecret,
        username: &Username,
    ) -> Result<SecretString, TotpUriError> {
        let totp = totp(secret, Some(username)).ok_or(TotpUriError)?;
        let uri = Zeroizing::new(totp.to_url().map_err(|_| TotpUriError)?);
        Ok(SecretString::from(uri.as_str()))
    }

    fn base32(&self, secret: &TotpSecret) -> SecretString {
        let text = Zeroizing::new(Secret::from(secret.expose().as_slice()).to_base32());
        SecretString::from(text.as_str())
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::auth::{TOTP_SECRET_LEN, current_step};
    use secrecy::{ExposeSecret as _, SecretBox};

    use super::*;

    /// RFC 6238's SHA-1 test secret: the ASCII digits "1234567890" twice.
    const RFC_SECRET: &[u8; TOTP_SECRET_LEN] = b"12345678901234567890";

    fn rfc_secret() -> TotpSecret {
        TotpSecret::new(SecretBox::new(Box::new(*RFC_SECRET)))
    }

    fn step_at(unix: i64) -> u64 {
        current_step(chrono::DateTime::from_timestamp(unix, 0).unwrap()).unwrap()
    }

    fn code(text: &str) -> TotpCode {
        TotpCode::parse(text).unwrap()
    }

    /// RFC 6238 appendix B's SHA-1 values, cut to 6 digits (the last six of
    /// its 8-digit codes, since both are the same value modulo 10^6).
    #[rstest::rstest]
    #[case::t_59(59, "287082")]
    #[case::t_1111111109(1_111_111_109, "081804")]
    #[case::t_1111111111(1_111_111_111, "050471")]
    #[case::t_1234567890(1_234_567_890, "005924")]
    #[case::t_2000000000(2_000_000_000, "279037")]
    #[case::t_20000000000(20_000_000_000, "353130")]
    fn the_rfc_6238_vectors_match_at_their_step(#[case] unix: i64, #[case] expected: &str) {
        assert!(TotpRs.matches(&rfc_secret(), step_at(unix), &code(expected)));
    }

    #[test]
    fn a_code_matches_only_its_own_step() {
        let step = step_at(59);

        assert!(!TotpRs.matches(&rfc_secret(), step + 1, &code("287082")));
        assert!(!TotpRs.matches(&rfc_secret(), step - 1, &code("287082")));
    }

    #[test]
    fn another_code_or_secret_doesnt_match() {
        let other = TotpSecret::new(SecretBox::new(Box::new([0x55; TOTP_SECRET_LEN])));

        assert!(!TotpRs.matches(&rfc_secret(), step_at(59), &code("287083")));
        assert!(!TotpRs.matches(&other, step_at(59), &code("287082")));
    }

    #[test]
    fn a_step_beyond_u64_seconds_matches_nothing() {
        assert!(!TotpRs.matches(&rfc_secret(), u64::MAX, &code("287082")));
    }

    #[test]
    fn the_uri_names_afkfleet_the_user_and_the_secret() {
        let username = Username::try_from("afkbot1").unwrap();

        let uri = TotpRs.otpauth_uri(&rfc_secret(), &username).unwrap();

        assert_eq!(
            uri.expose_secret(),
            "otpauth://totp/afkfleet:afkbot1?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=afkfleet"
        );
    }

    #[test]
    fn the_longest_username_needs_no_escaping() {
        let username = Username::try_from("a._-".repeat(8).as_str()).unwrap();

        let uri = TotpRs.otpauth_uri(&rfc_secret(), &username).unwrap();

        assert!(
            uri.expose_secret()
                .starts_with(&format!("otpauth://totp/afkfleet:{}?", "a._-".repeat(8))),
            "{}",
            uri.expose_secret()
        );
    }

    #[test]
    fn the_secret_is_32_base32_characters_without_padding() {
        assert_eq!(
            TotpRs.base32(&rfc_secret()).expose_secret(),
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
    }
}
