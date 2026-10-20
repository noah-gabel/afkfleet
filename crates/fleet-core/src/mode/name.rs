//! [`ModeName`]: a mode's name, as users give it (ADR-0020, S13, U8).

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

use super::ModePreset;
use crate::text;

/// The punctuation outside ASCII a name may hold (U8): quotation marks,
/// guillemets, dashes, the ellipsis, the inverted marks and the middle dot.
const OTHER_PUNCTUATION: [char; 13] = [
    '„', '“', '”', '‘', '’', '«', '»', '–', '—', '…', '¡', '¿', '·',
];

/// Why text isn't a valid [`ModeName`]. Like every fleet-core error, it
/// names the rule and a position, never the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ModeNameError {
    /// The name is empty or only whitespace.
    #[error("the name is empty")]
    Empty,
    /// The name holds a character [`ModeName::allows`] refuses.
    #[error("the name holds a character that isn't allowed at position {index}")]
    ForbiddenChar {
        /// The character's position (counted in characters) in the trimmed name.
        index: usize,
    },
    /// The name is longer than [`ModeName::MAX_LEN`] characters.
    #[error("the name is {chars} characters long; the limit is {max}", max = ModeName::MAX_LEN)]
    TooLong {
        /// The trimmed name's length in characters.
        chars: usize,
    },
}

/// A mode's name.
///
/// It's trimmed, 1 to [`ModeName::MAX_LEN`] characters long (Unicode scalar
/// values, as the API's `BoundedText` counts them), and holds only what
/// [`ModeName::allows`]: letters and digits of every script, plain spaces
/// and punctuation, never an emoji or another symbol (U8). None of those is
/// a character the API's text cleaning would strip, so a name always comes
/// back exactly as it was given.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModeName(String);

impl ModeName {
    /// The maximum length in characters.
    pub const MAX_LEN: usize = 32;

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `other` is the same name when ASCII case is ignored, as the
    /// database's `lower()` compares names (S13).
    #[must_use]
    pub fn same_name(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }

    /// The built-in mode whose name this is, ASCII case ignored: no user's
    /// mode may take it (S13).
    #[must_use]
    pub fn preset(&self) -> Option<ModePreset> {
        ModePreset::ALL
            .into_iter()
            .find(|preset| self.0.eq_ignore_ascii_case(preset.name()))
    }

    /// Whether a name may hold `c` (U8): a letter or a digit of any script
    /// (Unicode's Alphabetic and Numeric properties), a plain space, ASCII
    /// punctuation, or one of a few others (`„ “ ” ‘ ’ « » – — … ¡ ¿ ·`).
    /// Never an invisible character, though a few letters are (the Hangul
    /// fillers), so nothing a name holds is stripped when it's displayed.
    #[must_use]
    pub fn allows(c: char) -> bool {
        let wanted = c.is_alphabetic()
            || c.is_numeric()
            || c == ' '
            || c.is_ascii_punctuation()
            || OTHER_PUNCTUATION.contains(&c);
        wanted && !text::is_invisible(c)
    }
}

impl TryFrom<&str> for ModeName {
    type Error = ModeNameError;

    fn try_from(text: &str) -> Result<Self, ModeNameError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ModeNameError::Empty);
        }
        if let Some(index) = trimmed.chars().position(|c| !Self::allows(c)) {
            return Err(ModeNameError::ForbiddenChar { index });
        }
        let chars = trimmed.chars().count();
        if chars > Self::MAX_LEN {
            return Err(ModeNameError::TooLong { chars });
        }
        Ok(Self(trimmed.to_owned()))
    }
}

impl TryFrom<String> for ModeName {
    type Error = ModeNameError;

    fn try_from(text: String) -> Result<Self, ModeNameError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for ModeName {
    type Err = ModeNameError;

    fn from_str(text: &str) -> Result<Self, ModeNameError> {
        Self::try_from(text)
    }
}

impl From<ModeName> for String {
    fn from(name: ModeName) -> Self {
        name.0
    }
}

impl fmt::Display for ModeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::sanitize_untrusted;
    use proptest::prelude::*;
    use rstest::rstest;

    fn name(text: &str) -> ModeName {
        ModeName::try_from(text).unwrap()
    }

    #[rstest]
    #[case::plain("Greeter", "Greeter")]
    #[case::trimmed("  Farm 2 \n", "Farm 2")]
    #[case::umlauts("Grüße", "Grüße")]
    #[case::sharp_s("Fuß", "Fuß")]
    #[case::cyrillic("Ферма", "Ферма")]
    #[case::cjk("日本の農場", "日本の農場")]
    #[case::other_digits("½ and ²", "½ and ²")]
    #[case::ascii_punctuation(
        r##"!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"##,
        r##"!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"##
    )]
    #[case::listed_punctuation("„a“ ”‘’ «b» – — … ¡ ¿ ·", "„a“ ”‘’ «b» – — … ¡ ¿ ·")]
    #[case::a_built_in_name("afk", "afk")]
    fn accepts_and_trims(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(name(input).as_str(), expected);
    }

    #[rstest]
    #[case::empty("")]
    #[case::only_spaces("   ")]
    #[case::only_a_line_break("\n")]
    fn refuses_an_empty_name(#[case] input: &str) {
        assert_eq!(ModeName::try_from(input), Err(ModeNameError::Empty));
    }

    #[rstest]
    #[case::emoji("Farm 🐄", 5)]
    #[case::keycap_digit("1\u{FE0F}\u{20E3}", 1)]
    #[case::section_sign("§cred", 0)]
    #[case::copyright("a©", 1)]
    #[case::euro("10€", 2)]
    #[case::arrow("a→b", 1)]
    #[case::inner_no_break_space("a\u{A0}b", 1)]
    #[case::ideographic_space("a\u{3000}b", 1)]
    #[case::zero_width_space("a\u{200B}b", 1)]
    #[case::hangul_filler_a_letter_that_shows_nothing("a\u{3164}b", 1)]
    #[case::bidi_override("abc\u{202E}def", 3)]
    #[case::inner_line_break("a\nb", 1)]
    #[case::tab("a\tb", 1)]
    #[case::decomposed_umlaut("u\u{308}", 1)]
    #[case::position_counts_chars_not_bytes("üü🐄", 2)]
    fn refuses_a_character_u8_doesnt_allow_with_its_position(
        #[case] input: &str,
        #[case] index: usize,
    ) {
        assert_eq!(
            ModeName::try_from(input),
            Err(ModeNameError::ForbiddenChar { index })
        );
    }

    #[test]
    fn accepts_32_characters_and_refuses_33() {
        assert!(ModeName::try_from("ü".repeat(32).as_str()).is_ok());
        assert_eq!(
            ModeName::try_from("ü".repeat(33).as_str()),
            Err(ModeNameError::TooLong { chars: 33 })
        );
    }

    #[test]
    fn a_forbidden_character_is_named_before_the_length() {
        assert_eq!(
            ModeName::try_from(format!("{}🐄", "a".repeat(40)).as_str()),
            Err(ModeNameError::ForbiddenChar { index: 40 })
        );
    }

    #[rstest]
    #[case::exact("Farm", "Farm", true)]
    #[case::ascii_case("Farm", "fARM", true)]
    #[case::other("Farm", "Farm 2", false)]
    #[case::non_ascii_case_counts("Ärger", "ärger", false)]
    fn compares_names_without_ascii_case(#[case] a: &str, #[case] b: &str, #[case] same: bool) {
        assert_eq!(name(a).same_name(&name(b)), same);
        assert_eq!(name(b).same_name(&name(a)), same);
    }

    #[rstest]
    #[case::afk("afk", Some(ModePreset::Afk))]
    #[case::farm_upper_case("FARM", Some(ModePreset::Farm))]
    #[case::mixed_case("Afk", Some(ModePreset::Afk))]
    #[case::longer("afk 2", None)]
    #[case::other("fishing", None)]
    fn names_the_built_in_mode_it_takes(#[case] input: &str, #[case] preset: Option<ModePreset>) {
        assert_eq!(name(input).preset(), preset);
    }

    #[test]
    fn every_preset_name_is_a_valid_name() {
        for preset in ModePreset::ALL {
            assert_eq!(name(preset.name()).preset(), Some(preset));
        }
    }

    #[test]
    fn serde_goes_through_the_constructor() {
        let read: ModeName = serde_json::from_str(r#"" Farm ""#).unwrap();
        assert_eq!(read.as_str(), "Farm");
        assert_eq!(serde_json::to_string(&read).unwrap(), r#""Farm""#);
        assert!(serde_json::from_str::<ModeName>(r#""Farm 🐄""#).is_err());
    }

    #[test]
    fn errors_never_hold_the_text() {
        for error in [
            ModeName::try_from("").unwrap_err(),
            ModeName::try_from("secret🐄").unwrap_err(),
            ModeName::try_from("secret".repeat(10).as_str()).unwrap_err(),
        ] {
            assert!(!error.to_string().contains("secret"), "{error}");
        }
    }

    /// Characters that probe the rule's edges: letters that show nothing,
    /// symbols, whitespace, controls and listed punctuation.
    fn edgy_char() -> impl Strategy<Value = char> {
        prop_oneof![
            any::<char>(),
            prop::sample::select(vec![
                ' ', 'a', 'ü', '日', '½', '!', '~', '„', '·', '\u{A0}', '\u{3164}', '\u{115F}',
                '\u{FFA0}', '\u{17B4}', '\u{200B}', '\u{202E}', '\u{2066}', '\u{FEFF}', '§', '©',
                '\u{308}', '\n', '\t', '🐄',
            ]),
        ]
    }

    proptest! {
        /// A valid name passes the API's text cleaning unchanged, uncut, so
        /// it always comes back as it was given (S13).
        #[test]
        fn a_valid_name_passes_the_cleaning_unchanged(
            chars in prop::collection::vec(edgy_char(), 0..40)
        ) {
            let text: String = chars.into_iter().collect();
            if let Ok(name) = ModeName::try_from(text.as_str()) {
                let cleaned = sanitize_untrusted(name.as_str(), ModeName::MAX_LEN);
                prop_assert_eq!(cleaned.text.as_str(), name.as_str());
                prop_assert!(!cleaned.truncated);
            }
        }

        /// Whatever a name holds, each of its characters is one the rule
        /// allows, and the name is trimmed.
        #[test]
        fn a_valid_name_holds_only_allowed_characters(
            chars in prop::collection::vec(edgy_char(), 0..40)
        ) {
            let text: String = chars.into_iter().collect();
            if let Ok(name) = ModeName::try_from(text.as_str()) {
                prop_assert!(name.as_str().chars().all(ModeName::allows));
                prop_assert_eq!(name.as_str(), name.as_str().trim());
                prop_assert!(!name.as_str().is_empty());
                prop_assert!(name.as_str().chars().count() <= ModeName::MAX_LEN);
            }
        }
    }
}
