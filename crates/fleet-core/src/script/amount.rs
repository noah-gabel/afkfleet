//! Reading numbers as Minecraft servers print them: a whole number, and an
//! amount with thousands separators, decimals and the suffixes `k`, `M` and
//! `B` (ADR-0037, C3).
//!
//! A chat pattern's captures read with these, and Phase 16's price helpers
//! use the same reader, so a number means the same wherever a script meets
//! it. Both readers take the longest text their grammar allows at the start
//! of what they're given and never try a shorter one: reading is linear,
//! and a line has one reading or none.

use serde::{Deserialize, Serialize};

/// Which character an [`amount`](read_amount) writes before its decimals.
/// The other of the two is its thousands separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecimalMark {
    /// `1,234.56`.
    #[serde(rename = ".")]
    Point,
    /// `1.234,56`.
    #[serde(rename = ",")]
    Comma,
}

impl DecimalMark {
    /// The decimal mark's character.
    #[must_use]
    pub const fn mark(self) -> char {
        match self {
            Self::Point => '.',
            Self::Comma => ',',
        }
    }

    /// The thousands separator's character: the other one.
    #[must_use]
    pub const fn separator(self) -> char {
        match self {
            Self::Point => ',',
            Self::Comma => '.',
        }
    }
}

/// The most digits a number has before its decimal mark.
pub const MAX_DIGITS: usize = 15;
/// The most digits an amount has after its decimal mark.
pub const MAX_DECIMALS: usize = 4;
/// The largest amount, in either direction, after its suffix: 10^15.
pub const MAX_AMOUNT: f64 = 1_000_000_000_000_000.0;

/// A number read from the start of a text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Read<T> {
    /// The number.
    pub value: T,
    /// How many bytes of the text it took. They're all ASCII.
    pub len: usize,
}

/// Reads a whole number at the start of `text`: an optional `-` and 1 to
/// [`MAX_DIGITS`] digits, all the digits there are.
///
/// `None` when `text` doesn't start with one, or has more digits than that.
/// No sign but `-`, no separators, no spaces.
#[must_use]
pub fn read_integer(text: &str) -> Option<Read<i64>> {
    let bytes = text.as_bytes();
    let sign = usize::from(bytes.first() == Some(&b'-'));
    let digits = digits_at(bytes, sign);
    if digits == 0 || digits > MAX_DIGITS {
        return None;
    }
    let len = sign + digits;
    // An optional minus and at most 15 digits always fit an `i64`.
    let value = text.get(..len)?.parse().ok()?;
    Some(Read { value, len })
}

/// How many ASCII digits `bytes` holds from `at` on.
fn digits_at(bytes: &[u8], at: usize) -> usize {
    bytes
        .iter()
        .skip(at)
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

/// Whether `bytes` holds exactly `count` digits at `at`: that many, and no
/// digit after them.
fn exactly_digits_at(bytes: &[u8], at: usize, count: usize) -> bool {
    digits_at(bytes, at) == count
}

/// Reads an amount at the start of `text`, as servers print money:
///
/// - an optional `-`;
/// - 1 to [`MAX_DIGITS`] digits, plain or with thousands separators in
///   groups of exactly three after a first group of one to three;
/// - optionally the decimal mark and 1 to [`MAX_DECIMALS`] digits;
/// - optionally a suffix, `k` (thousand), `M` (million) or `B` (billion),
///   in either case, when no letter or digit follows it.
///
/// Each part is taken only whole: a separator that isn't followed by
/// exactly three digits, a decimal mark that isn't followed by one to four,
/// or a suffix letter that starts a word ends the amount before it, and what
/// follows is left for the caller. `1,234.56.` reads `1234.56` and leaves
/// the last point; `5 min` reads `5`; `12,34` reads `12`.
///
/// `None` when `text` doesn't start with an amount, when it has more digits
/// than that, or when the amount with its suffix is larger than
/// [`MAX_AMOUNT`].
#[must_use]
pub fn read_amount(text: &str, mark: DecimalMark) -> Option<Read<f64>> {
    let bytes = text.as_bytes();
    let is = |at: usize, c: char| bytes.get(at).is_some_and(|byte| char::from(*byte) == c);
    let negative = is(0, '-');
    let mut at = usize::from(negative);

    // The digits before the mark, without their separators.
    let mut whole = String::new();
    let mut take = |from: usize, count: usize| {
        whole.extend(
            bytes
                .iter()
                .skip(from)
                .take(count)
                .map(|byte| char::from(*byte)),
        );
    };
    let first = digits_at(bytes, at);
    if first == 0 {
        return None;
    }
    take(at, first);
    at += first;
    if first <= GROUP {
        while is(at, mark.separator()) && exactly_digits_at(bytes, at + 1, GROUP) {
            take(at + 1, GROUP);
            at += 1 + GROUP;
        }
    }
    if whole.len() > MAX_DIGITS {
        return None;
    }

    let mut decimals = String::new();
    let after_mark = digits_at(bytes, at + 1);
    if is(at, mark.mark()) && (1..=MAX_DECIMALS).contains(&after_mark) {
        decimals.extend(
            bytes
                .iter()
                .skip(at + 1)
                .take(after_mark)
                .map(|byte| char::from(*byte)),
        );
        at += 1 + after_mark;
    }

    // A suffix letter counts only when no letter or digit follows it, so
    // `5 min` and `5min` both read 5.
    let zeros = bytes
        .get(at)
        .and_then(|byte| suffix_zeros(char::from(*byte)))
        .filter(|_| {
            !text
                .get(at + 1..)
                .and_then(|rest| rest.chars().next())
                .is_some_and(char::is_alphanumeric)
        });
    if zeros.is_some() {
        at += 1;
    }

    let value = scaled(negative, &whole, &decimals, zeros.unwrap_or(0))?;
    Some(Read { value, len: at })
}

/// How many digits a thousands group has.
const GROUP: usize = 3;

/// How many zeros a suffix letter stands for.
const fn suffix_zeros(letter: char) -> Option<usize> {
    match letter {
        'k' | 'K' => Some(3),
        'm' | 'M' => Some(6),
        'b' | 'B' => Some(9),
        _ => None,
    }
}

/// The number `whole.decimals` times ten to the `zeros`, exactly: the point
/// is moved through the digits, and the result is read as a decimal text,
/// so it's the nearest `f64` to what was printed. `None` beyond
/// [`MAX_AMOUNT`].
fn scaled(negative: bool, whole: &str, decimals: &str, zeros: usize) -> Option<f64> {
    let moved: String = decimals
        .chars()
        .chain(core::iter::repeat('0'))
        .take(zeros)
        .collect();
    let left: String = decimals.chars().skip(zeros).collect();
    let sign = if negative { "-" } else { "" };
    let value: f64 = format!("{sign}{whole}{moved}.{left}0").parse().ok()?;
    (value.abs() <= MAX_AMOUNT).then_some(value)
}

/// Reads `text` as one amount, the whole of it, edge whitespace aside.
#[must_use]
pub fn parse_amount(text: &str, mark: DecimalMark) -> Option<f64> {
    let text = text.trim();
    read_amount(text, mark)
        .filter(|read| read.len == text.len())
        .map(|read| read.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn integer(text: &str) -> Option<(i64, &str)> {
        read_integer(text).map(|read| (read.value, &text[read.len..]))
    }

    fn amount(text: &str) -> Option<(f64, &str)> {
        read_amount(text, DecimalMark::Point).map(|read| (read.value, &text[read.len..]))
    }

    fn comma(text: &str) -> Option<(f64, &str)> {
        read_amount(text, DecimalMark::Comma).map(|read| (read.value, &text[read.len..]))
    }

    #[rstest]
    #[case::one_digit("7", 7, "")]
    #[case::many("1500", 1500, "")]
    #[case::negative("-42", -42, "")]
    #[case::zero("0", 0, "")]
    #[case::leading_zeros("007", 7, "")]
    #[case::fifteen_digits("999999999999999", 999_999_999_999_999, "")]
    #[case::then_text("5 coins", 5, " coins")]
    #[case::then_a_point("5.", 5, ".")]
    #[case::no_separators("1,234", 1, ",234")]
    #[case::no_suffix("5k", 5, "k")]
    #[case::no_decimals("1.5", 1, ".5")]
    fn reads_a_whole_number(#[case] text: &str, #[case] value: i64, #[case] rest: &str) {
        assert_eq!(integer(text), Some((value, rest)));
    }

    #[rstest]
    #[case::nothing("")]
    #[case::text("five")]
    #[case::a_minus_alone("-")]
    #[case::a_minus_then_text("-x")]
    #[case::a_plus("+5")]
    #[case::a_space_first(" 5")]
    #[case::two_minuses("--5")]
    #[case::sixteen_digits("1000000000000000")]
    #[case::a_full_width_digit("\u{ff15}")]
    fn reads_no_whole_number(#[case] text: &str) {
        assert_eq!(read_integer(text), None);
    }

    #[rstest]
    #[case::plain("1500", 1500.0, "")]
    #[case::zero("0", 0.0, "")]
    #[case::negative("-12.5", -12.5, "")]
    #[case::decimals("1234.56", 1234.56, "")]
    #[case::one_decimal("0.5", 0.5, "")]
    #[case::four_decimals("0.1234", 0.1234, "")]
    #[case::separators("1,234,567", 1_234_567.0, "")]
    #[case::separators_and_decimals("1,234.56", 1234.56, "")]
    #[case::a_short_first_group("12,345", 12_345.0, "")]
    #[case::thousand("12.5k", 12_500.0, "")]
    #[case::thousand_upper("3K", 3000.0, "")]
    #[case::million("3M", 3_000_000.0, "")]
    #[case::million_lower("1.25m", 1_250_000.0, "")]
    #[case::billion("2B", 2_000_000_000.0, "")]
    #[case::billion_lower("0.5b", 500_000_000.0, "")]
    #[case::separators_and_a_suffix("1,500k", 1_500_000.0, "")]
    #[case::the_most_digits("999,999,999,999,999", 999_999_999_999_999.0, "")]
    #[case::the_largest("1000000B", 1e15, "")]
    fn reads_an_amount(#[case] text: &str, #[case] value: f64, #[case] rest: &str) {
        assert_eq!(amount(text), Some((value, rest)));
    }

    #[rstest]
    #[case::then_text("5 coins", 5.0, " coins")]
    #[case::a_sentences_point("1,234.56.", 1234.56, ".")]
    #[case::a_point_without_digits("5.", 5.0, ".")]
    #[case::a_point_then_text("5.x", 5.0, ".x")]
    #[case::five_decimals_arent_decimals("1.23456", 1.0, ".23456")]
    #[case::a_group_of_two("12,34", 12.0, ",34")]
    #[case::a_group_of_four("1,2345", 1.0, ",2345")]
    #[case::a_group_after_four_digits("1234,567", 1234.0, ",567")]
    #[case::a_separator_without_digits("5,", 5.0, ",")]
    #[case::a_second_group_too_short("1,234,56", 1234.0, ",56")]
    #[case::a_separator_after_the_decimals("1.5,000", 1.5, ",000")]
    #[case::a_suffix_that_starts_a_word("5min", 5.0, "min")]
    #[case::a_suffix_before_a_digit("5k2", 5.0, "k2")]
    #[case::a_suffix_before_a_letter("3Mio", 3.0, "Mio")]
    #[case::a_suffix_then_a_space("5k left", 5000.0, " left")]
    #[case::a_suffix_then_punctuation("5k.", 5000.0, ".")]
    #[case::one_suffix_only("5kk", 5.0, "kk")]
    #[case::another_letter("5t", 5.0, "t")]
    #[case::a_second_point("1.2.3", 1.2, ".3")]
    fn an_amount_ends_where_its_grammar_does(
        #[case] text: &str,
        #[case] value: f64,
        #[case] rest: &str,
    ) {
        assert_eq!(amount(text), Some((value, rest)));
    }

    #[rstest]
    #[case::nothing("")]
    #[case::text("lots")]
    #[case::a_minus_alone("-")]
    #[case::a_point_first(".5")]
    #[case::a_separator_first(",500")]
    #[case::a_currency_sign("$5")]
    #[case::a_plus("+5")]
    #[case::a_space_first(" 5")]
    #[case::a_suffix_alone("k")]
    #[case::sixteen_digits("1000000000000000")]
    #[case::sixteen_digits_in_groups("1,000,000,000,000,000")]
    #[case::sixteen_digits_before_decimals("1000000000000000.5")]
    #[case::past_the_largest_by_suffix("1000001B")]
    #[case::past_the_largest_by_a_decimal("1000000.5B")]
    #[case::a_full_width_digit("\u{ff15}")]
    fn reads_no_amount(#[case] text: &str) {
        assert_eq!(read_amount(text, DecimalMark::Point), None);
    }

    #[rstest]
    #[case::the_other_way_round("1.234,56", 1234.56, "")]
    #[case::a_comma_is_the_mark("0,5", 0.5, "")]
    #[case::a_point_groups("1.234", 1234.0, "")]
    #[case::a_point_that_groups_nothing("1.23", 1.0, ".23")]
    #[case::with_a_suffix("12,5k", 12_500.0, "")]
    fn the_declared_mark_decides_what_a_point_means(
        #[case] text: &str,
        #[case] value: f64,
        #[case] rest: &str,
    ) {
        assert_eq!(comma(text), Some((value, rest)));
    }

    #[test]
    fn the_same_text_reads_differently_under_each_mark() {
        assert_eq!(amount("1.234"), Some((1.234, "")));
        assert_eq!(comma("1.234"), Some((1234.0, "")));
        assert_eq!(amount("1,234"), Some((1234.0, "")));
        assert_eq!(comma("1,234"), Some((1.234, "")));
    }

    #[rstest]
    #[case::plain("1500", Some(1500.0))]
    #[case::with_edge_spaces("  12.5k ", Some(12_500.0))]
    #[case::text_after("5 coins", None)]
    #[case::text_before("$5", None)]
    #[case::nothing("", None)]
    fn a_whole_text_is_one_amount_or_none(#[case] text: &str, #[case] value: Option<f64>) {
        assert_eq!(parse_amount(text, DecimalMark::Point), value);
    }

    #[test]
    fn each_mark_names_its_two_characters() {
        assert_eq!(
            (DecimalMark::Point.mark(), DecimalMark::Point.separator()),
            ('.', ',')
        );
        assert_eq!(
            (DecimalMark::Comma.mark(), DecimalMark::Comma.separator()),
            (',', '.')
        );
        assert_eq!(serde_json::to_string(&DecimalMark::Point).unwrap(), "\".\"");
        assert_eq!(
            serde_json::from_str::<DecimalMark>("\",\"").unwrap(),
            DecimalMark::Comma
        );
        assert!(serde_json::from_str::<DecimalMark>("\";\"").is_err());
    }

    proptest! {
        #[test]
        fn never_panics_and_takes_only_ascii_from_the_start(text in any::<String>()) {
            for mark in [DecimalMark::Point, DecimalMark::Comma] {
                if let Some(read) = read_amount(&text, mark) {
                    prop_assert!(read.len > 0);
                    prop_assert!(text.is_char_boundary(read.len));
                    prop_assert!(text[..read.len].is_ascii());
                    prop_assert!(read.value.is_finite());
                    prop_assert!(read.value.abs() <= MAX_AMOUNT);
                }
            }
            if let Some(read) = read_integer(&text) {
                prop_assert!(read.len > 0);
                prop_assert!(text[..read.len].is_ascii());
            }
        }

        /// Any text of the amount's own characters: what's read is what a
        /// decimal parser reads from the same digits.
        #[test]
        fn an_amount_is_the_number_its_digits_spell(text in "-?[0-9]{1,6}(,[0-9]{3}){0,2}(\\.[0-9]{1,4})?[kMB]?") {
            let read = read_amount(&text, DecimalMark::Point);
            let digits: String = text.chars().filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-')).collect();
            let times = match text.chars().last() {
                Some('k') => 1e3,
                Some('M') => 1e6,
                Some('B') => 1e9,
                _ => 1.0,
            };
            let spelled: f64 = digits.parse().unwrap();
            if let Some(read) = read
                && read.len == text.len()
            {
                let expected = spelled * times;
                prop_assert!(
                    (read.value - expected).abs() <= expected.abs() * 1e-12,
                    "{} vs {expected}",
                    read.value
                );
            }
        }

        /// A whole number written out reads back as itself, and so does an
        /// amount written with two decimals.
        #[test]
        fn a_written_number_reads_back(number in -999_999_999_999_999_i64..=999_999_999_999_999, cents in 0_u8..100) {
            let written = number.to_string();
            prop_assert_eq!(read_integer(&written), Some(Read { value: number, len: written.len() }));

            let written = format!("{number}.{cents:02}");
            let read = read_amount(&written, DecimalMark::Point).unwrap();
            prop_assert_eq!(read.len, written.len());
            // Bit for bit: both are the nearest `f64` to the same digits.
            let parsed: f64 = written.parse().unwrap();
            prop_assert_eq!(read.value.to_bits(), parsed.to_bits());
        }
    }
}
