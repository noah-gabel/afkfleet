//! [`NewPassword`]: a password someone chose, before it's hashed.

use secrecy::{ExposeSecret as _, SecretString};

/// Why a text can't be a new password. The variants carry only the limits,
/// never the password, its length or any of its characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NewPasswordError {
    /// It has fewer than [`NewPassword::MIN_CHARS`] characters.
    #[error("a password has at least {min} characters")]
    TooShort {
        /// The shortest length allowed.
        min: usize,
    },
    /// It has more than [`NewPassword::MAX_CHARS`] characters.
    #[error("a password has at most {max} characters")]
    TooLong {
        /// The longest length allowed.
        max: usize,
    },
    /// It contains a control character, such as a line break or a tab.
    #[error("a password can't contain control characters, such as a line break or a tab")]
    ControlCharacter,
}

/// A password someone chose for themselves (§7.2): 12 to 128 characters,
/// none of them a control character.
///
/// It applies only to new passwords (the Owner's, a registration, a password
/// change), never to a login attempt, which is checked against the stored
/// hash whatever it contains.
///
/// - **Characters** are Unicode scalar values (`chars()`), so 12 `ä`s are 12
///   characters. Spaces and any Unicode are allowed; nothing is trimmed or
///   normalized.
/// - **Control characters** (C0, DEL and C1: `char::is_control`) are refused:
///   nobody types them, and a password pasted from a file with a trailing
///   line break would otherwise silently include it.
///
/// The server's strength check takes a `&NewPassword`, so this rule always
/// runs first and the check never sees more than 128 characters. `Debug` is
/// redacted, and the memory is zeroized when it's dropped.
#[derive(Debug)]
pub struct NewPassword(SecretString);

impl NewPassword {
    /// The shortest password allowed, in characters.
    pub const MIN_CHARS: usize = 12;
    /// The longest password allowed, in characters.
    pub const MAX_CHARS: usize = 128;

    /// Returns the password, for the server's password service only.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl TryFrom<SecretString> for NewPassword {
    type Error = NewPasswordError;

    /// Checks the length first, then the characters.
    ///
    /// # Errors
    /// [`NewPasswordError::TooShort`], [`NewPasswordError::TooLong`] or
    /// [`NewPasswordError::ControlCharacter`]. The text is dropped, and
    /// zeroized, on any error.
    fn try_from(password: SecretString) -> Result<Self, NewPasswordError> {
        let text = password.expose_secret();
        // Counting stops one past the limit, so a huge input isn't walked
        // to its end.
        let len = text.chars().take(Self::MAX_CHARS + 1).count();
        if len < Self::MIN_CHARS {
            return Err(NewPasswordError::TooShort {
                min: Self::MIN_CHARS,
            });
        }
        if len > Self::MAX_CHARS {
            return Err(NewPasswordError::TooLong {
                max: Self::MAX_CHARS,
            });
        }
        if text.chars().any(char::is_control) {
            return Err(NewPasswordError::ControlCharacter);
        }
        Ok(Self(password))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    fn new(text: &str) -> Result<NewPassword, NewPasswordError> {
        NewPassword::try_from(SecretString::from(text))
    }

    #[rstest]
    #[case::twelve_ascii("a".repeat(12))]
    #[case::twelve_umlauts("ä".repeat(12))]
    #[case::twelve_emoji("🔑".repeat(12))]
    #[case::spaces_inside("correct horse battery staple")]
    #[case::leading_and_trailing_spaces("  spaced out password  ")]
    #[case::max_ascii("b".repeat(128))]
    #[case::max_multibyte("ß".repeat(128))]
    fn passwords_of_12_to_128_characters_are_accepted_unchanged(#[case] text: String) {
        let password = new(&text).unwrap();

        assert_eq!(password.expose_secret(), text);
    }

    #[rstest]
    #[case::empty(String::new())]
    #[case::eleven_ascii("a".repeat(11))]
    #[case::eleven_umlauts_but_22_bytes("ä".repeat(11))]
    fn fewer_than_12_characters_are_too_short(#[case] text: String) {
        assert_eq!(
            new(&text).err(),
            Some(NewPasswordError::TooShort { min: 12 })
        );
    }

    #[rstest]
    #[case::ascii("a".repeat(129))]
    #[case::multibyte("ä".repeat(129))]
    #[case::far_too_long("x".repeat(100_000))]
    fn more_than_128_characters_are_too_long(#[case] text: String) {
        assert_eq!(
            new(&text).err(),
            Some(NewPasswordError::TooLong { max: 128 })
        );
    }

    #[rstest]
    #[case::trailing_newline("long enough password\n")]
    #[case::carriage_return("long enough\rpassword")]
    #[case::tab("long enough\tpassword")]
    #[case::nul("long enough\0password")]
    #[case::delete("long enough\u{7f}password")]
    #[case::c1_next_line("long enough\u{85}password")]
    #[case::c1_csi("long enough\u{9b}password")]
    fn control_characters_are_refused(#[case] text: &str) {
        assert_eq!(new(text).err(), Some(NewPasswordError::ControlCharacter));
    }

    #[test]
    fn the_length_is_checked_before_the_characters() {
        assert_eq!(
            new("short\n").err(),
            Some(NewPasswordError::TooShort { min: 12 })
        );
        assert_eq!(
            new(&format!("{}\n", "a".repeat(200))).err(),
            Some(NewPasswordError::TooLong { max: 128 })
        );
    }

    #[test]
    fn errors_name_only_the_limits() {
        assert_eq!(
            NewPasswordError::TooShort { min: 12 }.to_string(),
            "a password has at least 12 characters"
        );
        assert_eq!(
            NewPasswordError::TooLong { max: 128 }.to_string(),
            "a password has at most 128 characters"
        );
        assert_eq!(
            NewPasswordError::ControlCharacter.to_string(),
            "a password can't contain control characters, such as a line break or a tab"
        );
    }

    #[test]
    fn debug_never_shows_the_password() {
        let password = new("hunter2-but-much-longer").unwrap();

        let debug = format!("{password:?}");

        assert!(!debug.contains("hunter2"), "{debug}");
    }

    fn printable() -> impl Strategy<Value = char> {
        any::<char>().prop_filter("not a control character", |c| !c.is_control())
    }

    proptest! {
        #[test]
        fn any_12_to_128_printable_characters_are_accepted(
            chars in proptest::collection::vec(printable(), 12..=128)
        ) {
            let text: String = chars.into_iter().collect();

            let password = new(&text).unwrap();

            prop_assert_eq!(password.expose_secret(), text.as_str());
        }

        #[test]
        fn one_control_character_anywhere_is_refused(
            chars in proptest::collection::vec(printable(), 11..=127),
            position in any::<prop::sample::Index>(),
            control in any::<char>().prop_filter("a control character", |c| c.is_control()),
        ) {
            let mut chars = chars;
            let at = position.index(chars.len() + 1);
            chars.insert(at, control);
            let text: String = chars.into_iter().collect();

            prop_assert_eq!(new(&text).err(), Some(NewPasswordError::ControlCharacter));
        }
    }
}
