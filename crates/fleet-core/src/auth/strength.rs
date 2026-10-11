//! [`WeakWarning`] and [`WeakSuggestion`]: why the server's strength check
//! refused a password, and what would help.
//!
//! They're our own types, one variant per variant of zxcvbn 3.1.1's
//! `feedback::Warning` and `feedback::Suggestion`, with the same English
//! texts, so the server's password port names no crate's type (ADR-0016).
//! fleet-server maps zxcvbn's enums onto these with exhaustive `match`es, so
//! a zxcvbn upgrade that adds a variant fails to compile instead of dropping
//! a hint, and a test there checks every text against zxcvbn's own. They
//! describe the pattern the check found, never the password.

use core::fmt;

/// What made the strength check refuse a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WeakWarning {
    /// "Straight rows of keys are easy to guess."
    StraightRowsOfKeysAreEasyToGuess,
    /// "Short keyboard patterns are easy to guess."
    ShortKeyboardPatternsAreEasyToGuess,
    /// "Repeats like "aaa" are easy to guess."
    RepeatsLikeAaaAreEasyToGuess,
    /// "Repeats like "abcabcabc" are only slightly harder to guess than "abc"."
    RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess,
    /// "This is a top-10 common password."
    ThisIsATop10Password,
    /// "This is a top-100 common password."
    ThisIsATop100Password,
    /// "This is a very common password."
    ThisIsACommonPassword,
    /// "This is similar to a commonly used password."
    ThisIsSimilarToACommonlyUsedPassword,
    /// "Sequences like abc or 6543 are easy to guess."
    SequencesLikeAbcAreEasyToGuess,
    /// "Recent years are easy to guess."
    RecentYearsAreEasyToGuess,
    /// "A word by itself is easy to guess."
    AWordByItselfIsEasyToGuess,
    /// "Dates are often easy to guess."
    DatesAreOftenEasyToGuess,
    /// "Names and surnames by themselves are easy to guess."
    NamesAndSurnamesByThemselvesAreEasyToGuess,
    /// "Common names and surnames are easy to guess."
    CommonNamesAndSurnamesAreEasyToGuess,
}

impl WeakWarning {
    /// Every warning, once each.
    pub const ALL: [Self; 14] = [
        Self::StraightRowsOfKeysAreEasyToGuess,
        Self::ShortKeyboardPatternsAreEasyToGuess,
        Self::RepeatsLikeAaaAreEasyToGuess,
        Self::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess,
        Self::ThisIsATop10Password,
        Self::ThisIsATop100Password,
        Self::ThisIsACommonPassword,
        Self::ThisIsSimilarToACommonlyUsedPassword,
        Self::SequencesLikeAbcAreEasyToGuess,
        Self::RecentYearsAreEasyToGuess,
        Self::AWordByItselfIsEasyToGuess,
        Self::DatesAreOftenEasyToGuess,
        Self::NamesAndSurnamesByThemselvesAreEasyToGuess,
        Self::CommonNamesAndSurnamesAreEasyToGuess,
    ];

    /// The warning as one English sentence, zxcvbn 3.1.1's own words.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::StraightRowsOfKeysAreEasyToGuess => "Straight rows of keys are easy to guess.",
            Self::ShortKeyboardPatternsAreEasyToGuess => {
                "Short keyboard patterns are easy to guess."
            }
            Self::RepeatsLikeAaaAreEasyToGuess => "Repeats like \"aaa\" are easy to guess.",
            Self::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess => {
                "Repeats like \"abcabcabc\" are only slightly harder to guess than \"abc\"."
            }
            Self::ThisIsATop10Password => "This is a top-10 common password.",
            Self::ThisIsATop100Password => "This is a top-100 common password.",
            Self::ThisIsACommonPassword => "This is a very common password.",
            Self::ThisIsSimilarToACommonlyUsedPassword => {
                "This is similar to a commonly used password."
            }
            Self::SequencesLikeAbcAreEasyToGuess => "Sequences like abc or 6543 are easy to guess.",
            Self::RecentYearsAreEasyToGuess => "Recent years are easy to guess.",
            Self::AWordByItselfIsEasyToGuess => "A word by itself is easy to guess.",
            Self::DatesAreOftenEasyToGuess => "Dates are often easy to guess.",
            Self::NamesAndSurnamesByThemselvesAreEasyToGuess => {
                "Names and surnames by themselves are easy to guess."
            }
            Self::CommonNamesAndSurnamesAreEasyToGuess => {
                "Common names and surnames are easy to guess."
            }
        }
    }
}

impl fmt::Display for WeakWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// What would make a refused password stronger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WeakSuggestion {
    /// "Use a few words, avoid common phrases."
    UseAFewWordsAvoidCommonPhrases,
    /// "No need for symbols, digits, or uppercase letters."
    NoNeedForSymbolsDigitsOrUppercaseLetters,
    /// "Add another word or two. Uncommon words are better."
    AddAnotherWordOrTwo,
    /// "Capitalization doesn't help very much."
    CapitalizationDoesntHelpVeryMuch,
    /// "All-uppercase is almost as easy to guess as all-lowercase."
    AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase,
    /// "Reversed words aren't much harder to guess."
    ReversedWordsArentMuchHarderToGuess,
    /// "Predictable substitutions like '@' instead of 'a' don't help very much."
    PredictableSubstitutionsDontHelpVeryMuch,
    /// "Use a longer keyboard pattern with more turns."
    UseALongerKeyboardPatternWithMoreTurns,
    /// "Avoid repeated words and characters."
    AvoidRepeatedWordsAndCharacters,
    /// "Avoid sequences."
    AvoidSequences,
    /// "Avoid recent years."
    AvoidRecentYears,
    /// "Avoid years that are associated with you."
    AvoidYearsThatAreAssociatedWithYou,
    /// "Avoid dates and years that are associated with you."
    AvoidDatesAndYearsThatAreAssociatedWithYou,
}

impl WeakSuggestion {
    /// Every suggestion, once each.
    pub const ALL: [Self; 13] = [
        Self::UseAFewWordsAvoidCommonPhrases,
        Self::NoNeedForSymbolsDigitsOrUppercaseLetters,
        Self::AddAnotherWordOrTwo,
        Self::CapitalizationDoesntHelpVeryMuch,
        Self::AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase,
        Self::ReversedWordsArentMuchHarderToGuess,
        Self::PredictableSubstitutionsDontHelpVeryMuch,
        Self::UseALongerKeyboardPatternWithMoreTurns,
        Self::AvoidRepeatedWordsAndCharacters,
        Self::AvoidSequences,
        Self::AvoidRecentYears,
        Self::AvoidYearsThatAreAssociatedWithYou,
        Self::AvoidDatesAndYearsThatAreAssociatedWithYou,
    ];

    /// The suggestion as one English sentence, zxcvbn 3.1.1's own words.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::UseAFewWordsAvoidCommonPhrases => "Use a few words, avoid common phrases.",
            Self::NoNeedForSymbolsDigitsOrUppercaseLetters => {
                "No need for symbols, digits, or uppercase letters."
            }
            Self::AddAnotherWordOrTwo => "Add another word or two. Uncommon words are better.",
            Self::CapitalizationDoesntHelpVeryMuch => "Capitalization doesn't help very much.",
            Self::AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase => {
                "All-uppercase is almost as easy to guess as all-lowercase."
            }
            Self::ReversedWordsArentMuchHarderToGuess => {
                "Reversed words aren't much harder to guess."
            }
            Self::PredictableSubstitutionsDontHelpVeryMuch => {
                "Predictable substitutions like '@' instead of 'a' don't help very much."
            }
            Self::UseALongerKeyboardPatternWithMoreTurns => {
                "Use a longer keyboard pattern with more turns."
            }
            Self::AvoidRepeatedWordsAndCharacters => "Avoid repeated words and characters.",
            Self::AvoidSequences => "Avoid sequences.",
            Self::AvoidRecentYears => "Avoid recent years.",
            Self::AvoidYearsThatAreAssociatedWithYou => "Avoid years that are associated with you.",
            Self::AvoidDatesAndYearsThatAreAssociatedWithYou => {
                "Avoid dates and years that are associated with you."
            }
        }
    }
}

impl fmt::Display for WeakSuggestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// Fails to compile when a variant is added, until `ALL` and this
    /// match list it.
    fn warning_index(warning: WeakWarning) -> usize {
        match warning {
            WeakWarning::StraightRowsOfKeysAreEasyToGuess => 0,
            WeakWarning::ShortKeyboardPatternsAreEasyToGuess => 1,
            WeakWarning::RepeatsLikeAaaAreEasyToGuess => 2,
            WeakWarning::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess => 3,
            WeakWarning::ThisIsATop10Password => 4,
            WeakWarning::ThisIsATop100Password => 5,
            WeakWarning::ThisIsACommonPassword => 6,
            WeakWarning::ThisIsSimilarToACommonlyUsedPassword => 7,
            WeakWarning::SequencesLikeAbcAreEasyToGuess => 8,
            WeakWarning::RecentYearsAreEasyToGuess => 9,
            WeakWarning::AWordByItselfIsEasyToGuess => 10,
            WeakWarning::DatesAreOftenEasyToGuess => 11,
            WeakWarning::NamesAndSurnamesByThemselvesAreEasyToGuess => 12,
            WeakWarning::CommonNamesAndSurnamesAreEasyToGuess => 13,
        }
    }

    fn suggestion_index(suggestion: WeakSuggestion) -> usize {
        match suggestion {
            WeakSuggestion::UseAFewWordsAvoidCommonPhrases => 0,
            WeakSuggestion::NoNeedForSymbolsDigitsOrUppercaseLetters => 1,
            WeakSuggestion::AddAnotherWordOrTwo => 2,
            WeakSuggestion::CapitalizationDoesntHelpVeryMuch => 3,
            WeakSuggestion::AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase => 4,
            WeakSuggestion::ReversedWordsArentMuchHarderToGuess => 5,
            WeakSuggestion::PredictableSubstitutionsDontHelpVeryMuch => 6,
            WeakSuggestion::UseALongerKeyboardPatternWithMoreTurns => 7,
            WeakSuggestion::AvoidRepeatedWordsAndCharacters => 8,
            WeakSuggestion::AvoidSequences => 9,
            WeakSuggestion::AvoidRecentYears => 10,
            WeakSuggestion::AvoidYearsThatAreAssociatedWithYou => 11,
            WeakSuggestion::AvoidDatesAndYearsThatAreAssociatedWithYou => 12,
        }
    }

    #[test]
    fn all_lists_every_warning_once_in_declaration_order() {
        let indexes: Vec<usize> = WeakWarning::ALL.iter().map(|w| warning_index(*w)).collect();

        assert_eq!(indexes, (0..14).collect::<Vec<_>>());
    }

    #[test]
    fn all_lists_every_suggestion_once_in_declaration_order() {
        let indexes: Vec<usize> = WeakSuggestion::ALL
            .iter()
            .map(|s| suggestion_index(*s))
            .collect();

        assert_eq!(indexes, (0..13).collect::<Vec<_>>());
    }

    #[test]
    fn every_text_is_one_distinct_sentence() {
        let texts: Vec<&str> = WeakWarning::ALL
            .iter()
            .map(|w| w.text())
            .chain(WeakSuggestion::ALL.iter().map(|s| s.text()))
            .collect();

        assert_eq!(texts.len(), 27);
        assert_eq!(texts.iter().collect::<HashSet<_>>().len(), 27);
        for text in texts {
            assert!(text.ends_with('.'), "{text}");
            assert!(text.starts_with(char::is_uppercase), "{text}");
        }
    }

    #[test]
    fn display_is_the_text() {
        assert_eq!(
            WeakWarning::ThisIsATop10Password.to_string(),
            "This is a top-10 common password."
        );
        assert_eq!(
            WeakSuggestion::AddAnotherWordOrTwo.to_string(),
            "Add another word or two. Uncommon words are better."
        );
    }
}
