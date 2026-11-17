//! [`Word`] and [`WordList`]: the closed lists of text a declaration holds.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::text;

/// Why text isn't a [`Word`]. It never holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WordError {
    /// The word is empty.
    #[error("the word is empty")]
    Empty,
    /// The word is longer than [`Word::MAX_LEN`] characters.
    #[error("the word is {chars} characters long; the limit is {max}", max = Word::MAX_LEN)]
    TooLong {
        /// Its length in characters.
        chars: usize,
    },
    /// The word holds whitespace, a control character, `§`, a
    /// text-direction mark or an invisible character.
    #[error("the word holds a character it may not, at position {index}")]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
}

/// One word of a declaration's closed list: a declared message's value that
/// is one of a few texts (`/home {place}` with `farm` and `base`), or a chat
/// pattern's capture that reads one of a few (`on`, `off`).
///
/// It's 1 to [`Word::MAX_LEN`] characters without whitespace, and without a
/// character a bot may not send or received chat never has: a control
/// character, `§`, a text-direction mark or an invisible character. Case
/// counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Word(String);

impl Word {
    /// The longest word, in characters.
    pub const MAX_LEN: usize = 32;

    /// Returns the word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for Word {
    type Error = WordError;

    fn try_from(text: &str) -> Result<Self, WordError> {
        let chars = text.chars().count();
        if chars == 0 {
            return Err(WordError::Empty);
        }
        if chars > Self::MAX_LEN {
            return Err(WordError::TooLong { chars });
        }
        match text
            .chars()
            .position(|c| c.is_whitespace() || is_forbidden_in_declaration(c))
        {
            Some(index) => Err(WordError::InvalidChar { index }),
            None => Ok(Self(text.to_owned())),
        }
    }
}

impl TryFrom<String> for Word {
    type Error = WordError;

    fn try_from(text: String) -> Result<Self, WordError> {
        Self::try_from(text.as_str())
    }
}

impl From<Word> for String {
    fn from(word: Word) -> Self {
        word.0
    }
}

impl fmt::Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `c` may never be in a declaration's text: what a bot may not
/// send ([`text::is_forbidden_in_message`]) and what received chat never
/// has (an invisible character), so a declaration reads as it's shown.
pub(crate) fn is_forbidden_in_declaration(c: char) -> bool {
    text::is_forbidden_in_message(c) || text::is_invisible(c)
}

/// Why texts can't form a [`WordList`].
///
/// `option` is a word's place in the list, counting from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WordListError {
    /// The list is empty, or holds more than [`WordList::MAX_WORDS`] words.
    #[error("the list has {count} words; it must have 1 to {max}", max = WordList::MAX_WORDS)]
    Count {
        /// How many it has.
        count: usize,
    },
    /// An entry isn't a [`Word`].
    #[error("word {option}: {reason}")]
    Word {
        /// The entry's place, from 0.
        option: usize,
        /// Why it isn't a word.
        reason: WordError,
    },
    /// An entry is in the list a second time.
    #[error("word {option} is in the list twice")]
    Duplicate {
        /// The second one's place, from 0.
        option: usize,
    },
}

/// A closed list of 1 to [`WordList::MAX_WORDS`] different [`Word`]s, in
/// the order they were declared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WordList(Vec<Word>);

impl WordList {
    /// The most words a list holds.
    pub const MAX_WORDS: usize = 32;

    /// Builds the list from `entries`, in order.
    ///
    /// # Errors
    /// [`WordListError::Count`] for no entry or more than
    /// [`WordList::MAX_WORDS`]; otherwise the first entry's error, in order:
    /// [`WordListError::Word`] for one that isn't a word, or
    /// [`WordListError::Duplicate`] for one that came before.
    pub fn try_new<S: AsRef<str>>(entries: &[S]) -> Result<Self, WordListError> {
        let count = entries.len();
        if count == 0 || count > Self::MAX_WORDS {
            return Err(WordListError::Count { count });
        }
        let mut words: Vec<Word> = Vec::with_capacity(count);
        for (option, entry) in entries.iter().enumerate() {
            let word = Word::try_from(entry.as_ref())
                .map_err(|reason| WordListError::Word { option, reason })?;
            if words.contains(&word) {
                return Err(WordListError::Duplicate { option });
            }
            words.push(word);
        }
        Ok(Self(words))
    }

    /// The words, in order.
    pub fn iter(&self) -> impl Iterator<Item = &Word> {
        self.0.iter()
    }

    /// Whether `text` is one of the words, exactly.
    #[must_use]
    pub fn contains(&self, text: &str) -> bool {
        self.0.iter().any(|word| word.as_str() == text)
    }

    /// The longest word that `text` starts with, if one does.
    #[must_use]
    pub fn longest_prefix_of(&self, text: &str) -> Option<&Word> {
        self.0
            .iter()
            .filter(|word| text.starts_with(word.as_str()))
            .max_by_key(|word| word.as_str().len())
    }

    /// The longest word's length in UTF-16 code units, as Minecraft counts
    /// a message's length.
    #[must_use]
    pub fn longest_units(&self) -> usize {
        self.0
            .iter()
            .map(|word| text::utf16_len(word.as_str()))
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn list(entries: &[&str]) -> WordList {
        WordList::try_new(entries).unwrap()
    }

    #[rstest]
    #[case::a_word("farm")]
    #[case::one_character("x")]
    #[case::upper_case_and_digits("AfkMain_2")]
    #[case::punctuation("north-east.2")]
    #[case::an_umlaut("gr\u{fc}n")]
    #[case::the_longest("abcdefghijklmnopqrstuvwxyz012345")]
    fn accepts_a_word(#[case] text: &str) {
        let word = Word::try_from(text).unwrap();

        assert_eq!(word.as_str(), text);
        assert_eq!(word.to_string(), text);
    }

    #[rstest]
    #[case::empty("", WordError::Empty)]
    #[case::too_long("abcdefghijklmnopqrstuvwxyz0123456", WordError::TooLong { chars: 33 })]
    #[case::a_space("a b", WordError::InvalidChar { index: 1 })]
    #[case::a_leading_space(" a", WordError::InvalidChar { index: 0 })]
    #[case::a_no_break_space("a\u{a0}b", WordError::InvalidChar { index: 1 })]
    #[case::a_line_break("a\nb", WordError::InvalidChar { index: 1 })]
    #[case::a_section_sign("\u{a7}c", WordError::InvalidChar { index: 0 })]
    #[case::a_direction_mark("ab\u{202e}", WordError::InvalidChar { index: 2 })]
    #[case::a_zero_width_space("a\u{200b}b", WordError::InvalidChar { index: 1 })]
    fn refuses_anything_else(#[case] text: &str, #[case] error: WordError) {
        assert_eq!(Word::try_from(text), Err(error));
    }

    #[test]
    fn a_word_reads_and_writes_as_a_plain_string() {
        let word = Word::try_from("farm").unwrap();

        assert_eq!(serde_json::to_string(&word).unwrap(), "\"farm\"");
        assert_eq!(serde_json::from_str::<Word>("\"farm\"").unwrap(), word);
        assert!(serde_json::from_str::<Word>("\"a b\"").is_err());
    }

    #[test]
    fn a_list_keeps_its_words_in_order() {
        let words = list(&["on", "off", "online"]);

        let texts: Vec<&str> = words.iter().map(Word::as_str).collect();
        assert_eq!(texts, ["on", "off", "online"]);
    }

    #[test]
    fn a_list_holds_up_to_32_words() {
        let entries: Vec<String> = (0..32).map(|n| format!("w{n}")).collect();
        assert!(WordList::try_new(&entries).is_ok());

        let entries: Vec<String> = (0..33).map(|n| format!("w{n}")).collect();
        assert_eq!(
            WordList::try_new(&entries),
            Err(WordListError::Count { count: 33 })
        );
    }

    #[rstest]
    #[case::none(&[], WordListError::Count { count: 0 })]
    #[case::not_a_word(
        &["on", "o ff"],
        WordListError::Word { option: 1, reason: WordError::InvalidChar { index: 1 } }
    )]
    #[case::an_empty_word(&["", "on"], WordListError::Word { option: 0, reason: WordError::Empty })]
    #[case::twice(&["on", "off", "on"], WordListError::Duplicate { option: 2 })]
    #[case::the_first_problem_wins(
        &["on", "on", " "],
        WordListError::Duplicate { option: 1 }
    )]
    fn refuses_a_list_that_isnt_one(#[case] entries: &[&str], #[case] error: WordListError) {
        assert_eq!(WordList::try_new(entries), Err(error));
    }

    #[test]
    fn case_makes_two_words() {
        let words = list(&["On", "on"]);

        assert!(words.contains("On"));
        assert!(words.contains("on"));
        assert!(!words.contains("ON"));
    }

    #[rstest]
    #[case::the_only_one("on now", Some("on"))]
    #[case::the_longest_of_two("online now", Some("online"))]
    #[case::the_whole_text("off", Some("off"))]
    #[case::none("away", None)]
    #[case::not_from_the_middle("is on", None)]
    #[case::nothing("", None)]
    fn the_longest_word_a_text_starts_with(#[case] text: &str, #[case] found: Option<&str>) {
        let words = list(&["on", "off", "online"]);

        assert_eq!(words.longest_prefix_of(text).map(Word::as_str), found);
    }

    #[test]
    fn the_longest_word_counts_in_utf16_units() {
        assert_eq!(list(&["on", "off"]).longest_units(), 3);
        assert_eq!(list(&["a", "\u{1f600}\u{1f600}"]).longest_units(), 4);
    }

    proptest! {
        #[test]
        fn a_word_never_panics_and_holds_no_whitespace(text in any::<String>()) {
            if let Ok(word) = Word::try_from(text.as_str()) {
                prop_assert_eq!(word.as_str(), text.as_str());
                prop_assert!(!text.chars().any(char::is_whitespace));
                prop_assert!(text.chars().count() <= Word::MAX_LEN);
            }
        }

        #[test]
        fn a_list_never_panics_and_finds_only_its_own_words(
            entries in proptest::collection::vec("[a-c]{0,3}", 0..6),
            text in "[a-c ]{0,6}",
        ) {
            if let Ok(words) = WordList::try_new(&entries) {
                prop_assert_eq!(words.contains(&text), entries.contains(&text));
                if let Some(found) = words.longest_prefix_of(&text) {
                    prop_assert!(text.starts_with(found.as_str()));
                    prop_assert!(entries
                        .iter()
                        .filter(|entry| text.starts_with(entry.as_str()))
                        .all(|entry| entry.len() <= found.as_str().len()));
                }
            }
        }
    }
}
