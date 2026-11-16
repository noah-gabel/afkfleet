//! [`ScriptSource`]: a script's text, checked before anything reads it.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::text;

/// Why text isn't a [`ScriptSource`]. It never holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScriptSourceError {
    /// The script is longer than [`ScriptSource::MAX_BYTES`] bytes.
    #[error("the script is {bytes} bytes long; the limit is {max}", max = ScriptSource::MAX_BYTES)]
    TooLong {
        /// Its length in bytes of UTF-8, line ends counted as one.
        bytes: usize,
    },
    /// The script holds a control character other than a line break and a
    /// tab, a text-direction mark, or an invisible character.
    #[error("the script holds a character it may not, at line {line}, column {column}")]
    ForbiddenChar {
        /// The character's line, from 1.
        line: usize,
        /// Its place in that line, counted in characters from 1.
        column: usize,
    },
}

/// A script's text.
///
/// It's at most [`ScriptSource::MAX_BYTES`] bytes of UTF-8, and it reads
/// as it's shown: no control character but a line break and a tab, no
/// text-direction mark (which can reorder what a reviewer sees, "Trojan
/// Source") and no invisible character. Whether it's a valid program is
/// `fleet-script`'s to say; this type only makes sure that whatever
/// compiles it, stores it or shows it gets bounded, honest text.
///
/// A Windows line end (`\r\n`) is kept as a plain line break, so a line
/// number means the same everywhere; a lone `\r` is refused like any other
/// control character. Nothing else is changed: the text isn't trimmed, so
/// an error's line is the line the author sees. An empty script is valid
/// and does nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ScriptSource(String);

impl ScriptSource {
    /// The longest script, in bytes: 16 KiB (ADR-0036, D3).
    pub const MAX_BYTES: usize = 16 * 1024;

    /// Returns the text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ScriptSource {
    type Error = ScriptSourceError;

    /// # Errors
    /// [`ScriptSourceError::TooLong`] first, then
    /// [`ScriptSourceError::ForbiddenChar`] for the first such character.
    fn try_from(text: &str) -> Result<Self, ScriptSourceError> {
        let text = text.replace("\r\n", "\n");
        let bytes = text.len();
        if bytes > Self::MAX_BYTES {
            return Err(ScriptSourceError::TooLong { bytes });
        }
        let (mut line, mut column) = (1_usize, 0_usize);
        for c in text.chars() {
            column += 1;
            if is_forbidden_in_source(c) {
                return Err(ScriptSourceError::ForbiddenChar { line, column });
            }
            if c == '\n' {
                line += 1;
                column = 0;
            }
        }
        Ok(Self(text))
    }
}

/// Whether `c` may never be in a script: a control character other than
/// a line break and a tab, a text-direction mark or an invisible character.
fn is_forbidden_in_source(c: char) -> bool {
    (c.is_control() && !matches!(c, '\n' | '\t'))
        || text::is_bidi_control(c)
        || text::is_invisible(c)
}

impl TryFrom<String> for ScriptSource {
    type Error = ScriptSourceError;

    fn try_from(text: String) -> Result<Self, ScriptSourceError> {
        Self::try_from(text.as_str())
    }
}

impl From<ScriptSource> for String {
    fn from(source: ScriptSource) -> Self {
        source.0
    }
}

impl fmt::Display for ScriptSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    const AFK: &str = "// Stays awake.\nfn on_join(first) {\n\ttimer.every(45000, \"move\");\n}\n";

    #[test]
    fn a_script_is_kept_as_written() {
        let source = ScriptSource::try_from(AFK).unwrap();

        assert_eq!(source.as_str(), AFK);
        assert_eq!(source.to_string(), AFK);
    }

    #[rstest]
    #[case::empty("")]
    #[case::only_whitespace("  \n\t\n")]
    #[case::umlauts_and_emoji("// gr\u{fc}\u{df}e \u{1f600}\nfn on_join(first) {}")]
    #[case::the_emoji_selector("// \u{263a}\u{fe0f}")]
    #[case::not_a_program("this isn't Rhai {{{")]
    fn accepts_text_that_reads_as_its_shown(#[case] text: &str) {
        assert_eq!(ScriptSource::try_from(text).unwrap().as_str(), text);
    }

    #[test]
    fn a_windows_line_end_is_a_line_break() {
        let source = ScriptSource::try_from("fn a() {}\r\nfn b() {}\r\n").unwrap();

        assert_eq!(source.as_str(), "fn a() {}\nfn b() {}\n");
    }

    #[rstest]
    #[case::a_lone_carriage_return("a\rb", 1, 2)]
    #[case::a_carriage_return_at_the_end("ab\r", 1, 3)]
    #[case::a_bell("fn a() {}\n\u{7}", 2, 1)]
    #[case::a_null("\0", 1, 1)]
    #[case::a_form_feed("a\n\nb\u{c}", 3, 2)]
    #[case::a_c1_control("x\u{85}", 1, 2)]
    #[case::a_right_to_left_override("// ok\nlet a = \"\u{202e}\";", 2, 10)]
    #[case::an_isolate("\u{2066}", 1, 1)]
    #[case::a_zero_width_space("fn on_\u{200b}join() {}", 1, 7)]
    #[case::a_byte_order_mark("\u{feff}fn a() {}", 1, 1)]
    #[case::a_soft_hyphen("// a\n// b\n// c\u{ad}", 3, 5)]
    #[case::columns_count_characters("// \u{fc}\u{fc}\u{7}", 1, 6)]
    #[case::after_a_windows_line_end("a\r\n\u{7}", 2, 1)]
    #[case::the_first_one("a\u{7}\n\u{7}", 1, 2)]
    fn refuses_a_character_that_hides_or_reorders(
        #[case] text: &str,
        #[case] line: usize,
        #[case] column: usize,
    ) {
        assert_eq!(
            ScriptSource::try_from(text),
            Err(ScriptSourceError::ForbiddenChar { line, column })
        );
    }

    #[test]
    fn a_script_of_16_kib_is_accepted_and_a_byte_more_isnt() {
        let largest = "x".repeat(ScriptSource::MAX_BYTES);
        assert!(ScriptSource::try_from(largest.as_str()).is_ok());

        let larger = "x".repeat(ScriptSource::MAX_BYTES + 1);
        assert_eq!(
            ScriptSource::try_from(larger.as_str()),
            Err(ScriptSourceError::TooLong { bytes: 16_385 })
        );
    }

    #[test]
    fn the_length_counts_bytes_with_line_ends_as_one() {
        // 8,192 two-byte characters fill it exactly.
        let umlauts = "\u{fc}".repeat(ScriptSource::MAX_BYTES / 2);
        assert!(ScriptSource::try_from(umlauts.as_str()).is_ok());
        assert_eq!(
            ScriptSource::try_from(format!("{umlauts}x")),
            Err(ScriptSourceError::TooLong { bytes: 16_385 })
        );

        // 8,192 Windows line ends are 8,192 bytes once they're line breaks.
        let lines = "\r\n".repeat(ScriptSource::MAX_BYTES / 2);
        assert_eq!(
            ScriptSource::try_from(lines.as_str())
                .unwrap()
                .as_str()
                .len(),
            8_192
        );
    }

    #[test]
    fn the_length_comes_before_a_character() {
        let text = format!("\u{7}{}", "x".repeat(ScriptSource::MAX_BYTES));

        assert_eq!(
            ScriptSource::try_from(text),
            Err(ScriptSourceError::TooLong { bytes: 16_385 })
        );
    }

    #[test]
    fn reads_and_writes_as_a_plain_string() {
        let source = ScriptSource::try_from("fn on_stop() {}\n").unwrap();

        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(json, r#""fn on_stop() {}\n""#);
        assert_eq!(serde_json::from_str::<ScriptSource>(&json).unwrap(), source);
        assert!(serde_json::from_str::<ScriptSource>(r#""\u0007""#).is_err());
    }

    #[test]
    fn an_error_names_a_place_and_never_the_text() {
        assert_eq!(
            ScriptSourceError::ForbiddenChar {
                line: 2,
                column: 10
            }
            .to_string(),
            "the script holds a character it may not, at line 2, column 10"
        );
        assert_eq!(
            ScriptSourceError::TooLong { bytes: 20_000 }.to_string(),
            "the script is 20000 bytes long; the limit is 16384"
        );
    }

    proptest! {
        /// Whatever is accepted is bounded, holds no carriage return, and
        /// is accepted again unchanged.
        #[test]
        fn never_panics_and_what_it_accepts_is_stable(text in "[ -~\\r\\n\\t\u{7}\u{fc}\u{202e}\u{200b}]{0,80}") {
            if let Ok(source) = ScriptSource::try_from(text.as_str()) {
                prop_assert!(source.as_str().len() <= ScriptSource::MAX_BYTES);
                prop_assert!(!source.as_str().contains('\r'));
                prop_assert_eq!(source.as_str(), text.replace("\r\n", "\n"));
                prop_assert_eq!(ScriptSource::try_from(source.as_str()), Ok(source.clone()));
            }
        }

        #[test]
        fn any_text_never_panics(text in any::<String>()) {
            let _ = ScriptSource::try_from(text);
        }
    }
}
