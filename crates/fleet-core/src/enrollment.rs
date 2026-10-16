//! Enrolling an agent (Plan.md P10.3; ADR-0019): the enrollment token's
//! rules and [`EnrollmentBundle`], the one line the Owner hands an agent.
//!
//! - **A token stands for one new agent.** The Owner creates it (or the
//!   server's host operator, with `afkfleet-server agent
//!   create-enrollment-token`), it lives [`TOKEN_TTL`] and is used once.
//! - **The bundle** is the token, a `.`, and the CA certificate's
//!   fingerprint as openssl prints it: `afk_et_…` plus 43 base64url
//!   characters, then 95 for the fingerprint, 146 in all. The agent pins
//!   the fingerprint: it checks its CA file against it before it sends the
//!   token anywhere. The token keeps its prefix, so a secret scanner still
//!   finds it.

use core::fmt;

use chrono::TimeDelta;
use secrecy::{ExposeSecret as _, SecretString};

use crate::pki::{CertFingerprint, CertFingerprintError};

/// How long an enrollment token can be used: 15 minutes.
pub const TOKEN_TTL: TimeDelta = TimeDelta::minutes(15);

/// How many unused, unexpired tokens may exist at once.
pub const MAX_PENDING_TOKENS: u32 = 20;

/// How many refused enrollments with one real token are audited; later ones
/// are only counted.
pub const MAX_AUDITED_REFUSALS: u32 = 10;

/// The prefix of every enrollment token.
pub const TOKEN_PREFIX: &str = "afk_et_";

/// The length of a token: its prefix and 43 base64url characters (32 bytes).
pub const TOKEN_LEN: usize = 50;

/// Why text isn't an [`EnrollmentBundle`]. No variant holds any part of the
/// text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BundleError {
    /// The bundle isn't 146 characters long.
    #[error("an enrollment bundle has {expected} characters, not {len}", expected = EnrollmentBundle::TEXT_LEN)]
    WrongLength {
        /// The text's length in characters.
        len: usize,
    },
    /// The part before the `.` isn't an enrollment token's shape.
    #[error("an enrollment bundle starts with an enrollment token (afk_et_…)")]
    NotAToken,
    /// The token and the fingerprint aren't joined by a `.`.
    #[error("an enrollment bundle joins its token and its fingerprint with a `.`")]
    NoSeparator,
    /// The part after the `.` isn't a fingerprint.
    #[error("an enrollment bundle ends with the CA's fingerprint: {0}")]
    Fingerprint(CertFingerprintError),
}

/// What an agent needs to enroll: a token, which is secret, and the CA
/// certificate's fingerprint, which it pins. Its `Debug` shows only the
/// fingerprint.
pub struct EnrollmentBundle {
    token: SecretString,
    ca_fingerprint: CertFingerprint,
}

impl EnrollmentBundle {
    /// The length of a bundle's text.
    pub const TEXT_LEN: usize = 146;

    /// The bundle of `token` and `ca_fingerprint`.
    ///
    /// # Errors
    /// [`BundleError::NotAToken`] when `token` isn't an enrollment token's
    /// shape.
    pub fn new(token: SecretString, ca_fingerprint: CertFingerprint) -> Result<Self, BundleError> {
        if !is_token_shaped(token.expose_secret()) {
            return Err(BundleError::NotAToken);
        }
        Ok(Self {
            token,
            ca_fingerprint,
        })
    }

    /// The token, for the enrollment request.
    #[must_use]
    pub fn token(&self) -> &SecretString {
        &self.token
    }

    /// The CA certificate's fingerprint, which the agent pins.
    #[must_use]
    pub const fn ca_fingerprint(&self) -> CertFingerprint {
        self.ca_fingerprint
    }

    /// The bundle's text: the token, `.`, the fingerprint. It holds the
    /// token, so it's a secret too.
    #[must_use]
    pub fn to_text(&self) -> SecretString {
        // Written once into a buffer of its exact size.
        let mut text = String::with_capacity(Self::TEXT_LEN);
        text.push_str(self.token.expose_secret());
        text.push('.');
        text.push_str(&self.ca_fingerprint.to_string());
        SecretString::from(text)
    }
}

/// Whether `text` is an enrollment token's shape: [`TOKEN_PREFIX`] and 43
/// base64url characters.
#[must_use]
pub fn is_token_shaped(text: &str) -> bool {
    text.len() == TOKEN_LEN
        && text.strip_prefix(TOKEN_PREFIX).is_some_and(|tail| {
            tail.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

impl TryFrom<&str> for EnrollmentBundle {
    type Error = BundleError;

    fn try_from(text: &str) -> Result<Self, BundleError> {
        let len = text.chars().count();
        if len != Self::TEXT_LEN {
            return Err(BundleError::WrongLength { len });
        }
        // The token's part is ASCII when it's shaped, so the split is at a
        // character boundary whenever it matters.
        let (Some(token), Some(rest)) = (text.get(..TOKEN_LEN), text.get(TOKEN_LEN..)) else {
            return Err(BundleError::NotAToken);
        };
        if !is_token_shaped(token) {
            return Err(BundleError::NotAToken);
        }
        let fingerprint = rest.strip_prefix('.').ok_or(BundleError::NoSeparator)?;
        let ca_fingerprint = fingerprint.parse().map_err(BundleError::Fingerprint)?;
        Ok(Self {
            token: SecretString::from(token),
            ca_fingerprint,
        })
    }
}

impl fmt::Debug for EnrollmentBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrollmentBundle")
            .field("ca_fingerprint", &self.ca_fingerprint)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    const TOKEN: &str = "afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq";
    const FINGERPRINT: &str = "17:25:5B:0A:45:82:70:4B:AE:77:5D:9E:55:B4:EF:BD:F0:0E:72:76:7D:1F:B5:7E:1A:A2:7D:06:4D:3A:A1:3C";

    fn bundle_text() -> String {
        format!("{TOKEN}.{FINGERPRINT}")
    }

    fn fingerprint() -> CertFingerprint {
        FINGERPRINT.parse().unwrap()
    }

    #[test]
    fn the_constants_hold_together() {
        assert_eq!(TOKEN.len(), TOKEN_LEN);
        assert_eq!(bundle_text().len(), EnrollmentBundle::TEXT_LEN);
        assert_eq!(TOKEN_TTL, TimeDelta::minutes(15));
        assert_eq!(MAX_PENDING_TOKENS, 20);
        assert_eq!(MAX_AUDITED_REFUSALS, 10);
    }

    #[test]
    fn a_bundle_is_its_token_a_dot_and_its_fingerprint() {
        let bundle = EnrollmentBundle::new(SecretString::from(TOKEN), fingerprint()).unwrap();

        assert_eq!(bundle.to_text().expose_secret(), bundle_text());
        assert_eq!(bundle.token().expose_secret(), TOKEN);
        assert_eq!(bundle.ca_fingerprint(), fingerprint());
    }

    #[test]
    fn a_bundle_is_read_back_from_its_text() {
        let bundle = EnrollmentBundle::try_from(bundle_text().as_str()).unwrap();

        assert_eq!(bundle.token().expose_secret(), TOKEN);
        assert_eq!(bundle.ca_fingerprint(), fingerprint());
    }

    #[test]
    fn debug_shows_the_fingerprint_and_never_the_token() {
        let bundle = EnrollmentBundle::try_from(bundle_text().as_str()).unwrap();

        let shown = format!("{bundle:?} {bundle:#?}");

        assert!(shown.contains(FINGERPRINT), "{shown}");
        assert!(!shown.contains("afk_et_"), "{shown}");
        assert!(!shown.contains("ABCDEFGH"), "{shown}");
    }

    #[rstest]
    #[case::token(TOKEN, true)]
    #[case::digits_dash_underscore("afk_et_0123456789-_abcdefghijklmnopqrstuvwxyzABCDE", true)]
    #[case::another_kind("afk_at_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq", false)]
    #[case::short("afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop", false)]
    #[case::long("afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqr", false)]
    #[case::plus("afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop+", false)]
    #[case::padding("afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop=", false)]
    #[case::non_ascii("afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnoä", false)]
    fn a_token_has_its_prefix_and_43_base64url_characters(
        #[case] text: &str,
        #[case] shaped: bool,
    ) {
        assert_eq!(is_token_shaped(text), shaped, "{text}");
    }

    #[test]
    fn a_token_of_another_shape_makes_no_bundle() {
        let refused = EnrollmentBundle::new(SecretString::from("afk_at_x"), fingerprint());

        assert_eq!(refused.map(|_| ()), Err(BundleError::NotAToken));
    }

    #[rstest]
    #[case::empty(String::new(), BundleError::WrongLength { len: 0 })]
    #[case::token_only(TOKEN.to_owned(), BundleError::WrongLength { len: 50 })]
    #[case::trailing_newline(format!("{TOKEN}.{FINGERPRINT}\n"), BundleError::WrongLength { len: 147 })]
    #[case::other_kind(format!("afk_at_{}.{FINGERPRINT}", &TOKEN[7..]), BundleError::NotAToken)]
    #[case::colon_for_dot(format!("{TOKEN}:{FINGERPRINT}"), BundleError::NoSeparator)]
    #[case::lowercase_fingerprint(format!("{TOKEN}.{}", FINGERPRINT.to_lowercase()), BundleError::Fingerprint(CertFingerprintError::InvalidChar { index: 7 }))]
    fn anything_else_is_refused(#[case] text: String, #[case] error: BundleError) {
        assert_eq!(
            EnrollmentBundle::try_from(text.as_str()).map(|_| ()),
            Err(error)
        );
    }

    #[test]
    fn errors_never_show_the_token() {
        let errors = [
            BundleError::WrongLength { len: 3 },
            BundleError::NotAToken,
            BundleError::NoSeparator,
            BundleError::Fingerprint(CertFingerprintError::WrongLength { len: 1 }),
        ];

        for error in errors {
            assert!(!error.to_string().contains("ABCDEFGH"), "{error}");
        }
        assert_eq!(
            BundleError::WrongLength { len: 3 }.to_string(),
            "an enrollment bundle has 146 characters, not 3"
        );
    }

    proptest! {
        #[test]
        fn reading_never_panics(text in any::<String>()) {
            let _ = EnrollmentBundle::try_from(text.as_str());
        }

        #[test]
        fn every_shaped_token_round_trips(tail in "[A-Za-z0-9_-]{43}", digest in any::<[u8; 32]>()) {
            let token = format!("{TOKEN_PREFIX}{tail}");
            let bundle = EnrollmentBundle::new(
                SecretString::from(token.as_str()),
                CertFingerprint::from_digest(digest),
            ).unwrap();

            let again = EnrollmentBundle::try_from(bundle.to_text().expose_secret()).unwrap();

            prop_assert_eq!(again.token().expose_secret(), token.as_str());
            prop_assert_eq!(again.ca_fingerprint(), CertFingerprint::from_digest(digest));
        }
    }
}
