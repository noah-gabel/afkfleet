//! The recovery codes and the second factor as typed (Plan.md P7.7, §7.2,
//! ADR-0016): [`RecoveryCode`] and [`SecondFactor`].
//!
//! A recovery code is [`RECOVERY_CODE_LEN`] characters of RFC 4648's base32
//! alphabet (lowercase): 100 bits from the server's secure random source,
//! shown once as `abcd-efgh-ijkl-mnop-qrst`. What's typed back is
//! normalized first: ASCII letters lowercased, `-` and spaces dropped. The
//! server stores only a hash of the normalized text, which is safe only
//! because the codes are that long.
//!
//! The login's second step takes one field: after the same normalization,
//! 6 digits are a TOTP code and 20 base32 characters a recovery code. The
//! two can't be confused, since their lengths differ.

use core::fmt;

use secrecy::{ExposeSecret as _, SecretString};

use super::totp::TotpCode;

/// How many recovery codes an enrollment gets (§7.2).
pub const RECOVERY_CODES: usize = 10;

/// A recovery code's length in characters: 100 bits of base32.
pub const RECOVERY_CODE_LEN: usize = 20;

/// The alphabet: RFC 4648's base32, lowercase.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// How many characters a shown code groups together.
const GROUP: usize = 4;

/// The longest text worth normalizing: a code with a separator between
/// every character and a space around each. Anything longer can't be one.
const MAX_TYPED: usize = 64;

/// A recovery code, normalized: [`RECOVERY_CODE_LEN`] lowercase base32
/// characters. Its `Debug` is redacted, and the text is wiped on drop.
pub struct RecoveryCode(SecretString);

impl RecoveryCode {
    /// The code made from `random`: each byte's low 5 bits pick one
    /// character, so every character is uniform (256 is a multiple of 32)
    /// and the code carries 100 bits.
    #[must_use]
    pub fn from_random(random: &[u8; RECOVERY_CODE_LEN]) -> Self {
        let text: String = random
            .iter()
            .map(|byte| char::from(alphabet(*byte)))
            .collect();
        Self(SecretString::from(text))
    }

    /// The code typed as `text`, once normalized (ASCII lowercased, `-` and
    /// spaces dropped), if that's exactly [`RECOVERY_CODE_LEN`] base32
    /// characters.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let normalized = normalize(text)?;
        (normalized.len() == RECOVERY_CODE_LEN
            && normalized.bytes().all(|byte| ALPHABET.contains(&byte)))
        .then(|| Self(SecretString::from(normalized)))
    }

    /// The normalized text, which the server hashes.
    #[must_use]
    pub fn normalized(&self) -> &str {
        self.0.expose_secret()
    }

    /// The code as shown to the user: groups of four joined by `-`.
    #[must_use]
    pub fn display(&self) -> SecretString {
        let normalized = self.0.expose_secret();
        let mut shown = String::with_capacity(RECOVERY_CODE_LEN + RECOVERY_CODE_LEN / GROUP);
        for (index, character) in normalized.chars().enumerate() {
            if index > 0 && index % GROUP == 0 {
                shown.push('-');
            }
            shown.push(character);
        }
        SecretString::from(shown)
    }
}

/// Redacted.
impl fmt::Debug for RecoveryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryCode(..)")
    }
}

/// The second factor as typed into the login's one `code` field.
#[derive(Debug)]
pub enum SecondFactor {
    /// A code from the authenticator app.
    Totp(TotpCode),
    /// One of the recovery codes.
    Recovery(RecoveryCode),
}

impl SecondFactor {
    /// What `text` is once normalized (ASCII lowercased, `-` and spaces
    /// dropped): 6 digits are a TOTP code, 20 base32 characters a recovery
    /// code, anything else `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let normalized = normalize(text)?;
        if let Some(code) = TotpCode::parse(&normalized) {
            return Some(Self::Totp(code));
        }
        RecoveryCode::parse(&normalized).map(Self::Recovery)
    }
}

/// `text` with ASCII letters lowercased and `-` and spaces dropped; `None`
/// for a text too long to be a code.
fn normalize(text: &str) -> Option<String> {
    (text.len() <= MAX_TYPED).then(|| {
        text.chars()
            .filter(|character| !matches!(character, '-' | ' '))
            .map(|character| character.to_ascii_lowercase())
            .collect()
    })
}

/// The alphabet's character for `byte`'s low 5 bits.
fn alphabet(byte: u8) -> u8 {
    ALPHABET
        .get(usize::from(byte & 0x1f))
        .copied()
        .unwrap_or(b'a')
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// 20 bytes covering the alphabet's ends and the high bits' masking.
    const RANDOM: [u8; 20] = [
        0, 1, 25, 26, 31, 32, 255, 224, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ];

    #[test]
    fn a_code_from_random_bytes_uses_their_low_5_bits() {
        let code = RecoveryCode::from_random(&RANDOM);

        assert_eq!(code.normalized(), "abz27a7ahijklmnopqrs");
    }

    #[test]
    fn every_byte_value_maps_to_the_alphabet_uniformly() {
        let mut counts = [0_u32; 32];
        for byte in 0..=u8::MAX {
            let character = alphabet(byte);
            let index = ALPHABET.iter().position(|a| *a == character).unwrap();
            counts[index] += 1;
        }

        assert!(counts.iter().all(|count| *count == 8), "{counts:?}");
    }

    #[test]
    fn a_code_is_shown_in_groups_of_four() {
        let code = RecoveryCode::from_random(&RANDOM);

        assert_eq!(code.display().expose_secret(), "abz2-7a7a-hijk-lmno-pqrs");
    }

    #[rstest]
    #[case::as_shown("abz2-7a7a-hijk-lmno-pqrs")]
    #[case::uppercase("ABZ2-7A7A-HIJK-LMNO-PQRS")]
    #[case::no_separators("abz27a7ahijklmnopqrs")]
    #[case::spaces("abz2 7a7a hijk lmno pqrs")]
    #[case::stray_separators(" -abz27a7a hijk--lmnopqrs- ")]
    fn a_typed_code_is_normalized(#[case] typed: &str) {
        let code = RecoveryCode::parse(typed).unwrap();

        assert_eq!(code.normalized(), "abz27a7ahijklmnopqrs");
    }

    #[rstest]
    #[case::one_short("abz2-7a7a-hijk-lmno-pqr")]
    #[case::one_long("abz2-7a7a-hijk-lmno-pqrst")]
    #[case::digit_outside_base32("abz2-7a7a-hijk-lmno-pqr1")]
    #[case::digit_eight("abz2-7a7a-hijk-lmno-pqr8")]
    #[case::non_ascii("abz2-7a7a-hijk-lmno-pqr\u{e9}")]
    #[case::other_separator("abz2_7a7a_hijk_lmno_pqrs")]
    #[case::tab("abz2\t7a7a\thijk\tlmno\tpqrs")]
    #[case::empty("")]
    fn anything_else_isnt_a_recovery_code(#[case] typed: &str) {
        assert!(RecoveryCode::parse(typed).is_none(), "{typed}");
    }

    #[test]
    fn an_overlong_text_isnt_normalized_at_all() {
        let typed = format!("abz27a7ahijklmnopqrs{}", " ".repeat(MAX_TYPED));

        assert!(RecoveryCode::parse(&typed).is_none());
        assert!(SecondFactor::parse(&typed).is_none());
    }

    #[test]
    fn debug_shows_no_character() {
        let code = RecoveryCode::from_random(&RANDOM);

        assert_eq!(format!("{code:?}"), "RecoveryCode(..)");
        let factor = SecondFactor::parse("abz2-7a7a-hijk-lmno-pqrs").unwrap();
        assert!(!format!("{factor:?}").contains("abz"));
    }

    #[rstest]
    #[case::plain("123456")]
    #[case::grouped("123 456")]
    #[case::dashed("123-456")]
    fn six_digits_are_a_totp_code(#[case] typed: &str) {
        let Some(SecondFactor::Totp(code)) = SecondFactor::parse(typed) else {
            panic!("{typed} isn't a TOTP code");
        };

        assert_eq!(code.digits(), b"123456");
    }

    #[test]
    fn twenty_base32_characters_are_a_recovery_code() {
        let Some(SecondFactor::Recovery(code)) = SecondFactor::parse("ABZ2-7A7A-HIJK-LMNO-PQRS")
        else {
            panic!("not a recovery code");
        };

        assert_eq!(code.normalized(), "abz27a7ahijklmnopqrs");
    }

    #[rstest]
    #[case::five_digits("12345")]
    #[case::seven_digits("1234567")]
    #[case::letters_of_six("abcdef")]
    #[case::nineteen_characters("abz27a7ahijklmnopqr")]
    #[case::empty("")]
    fn anything_else_is_neither(#[case] typed: &str) {
        assert!(SecondFactor::parse(typed).is_none(), "{typed}");
    }

    #[test]
    fn the_counts_are_pinned() {
        assert_eq!(RECOVERY_CODES, 10);
        assert_eq!(RECOVERY_CODE_LEN, 20);
    }
}
