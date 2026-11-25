//! [`ServerText`] and [`Lore`]: text a server shows on an item or a
//! screen, the first free text from a server a script reads (ADR-0036, M1).

use crate::script::amount::{DecimalMark, find_amount};
use crate::text::{self, LineBreaks};

/// One line of text a server chose: an item's name, a lore line or a
/// screen's title, made safe to read as plain text (ADR-0036, M1).
///
/// It's cleaned as a chat message's sender is (ADR-0010): no legacy
/// formatting code (`§` and the character after it), no control character,
/// line breaks included, no bidirectional control and no invisible
/// character, and at most the bound its owner sets, in characters, cut on
/// a character boundary. So it's one line wherever it's shown, and
/// cleaning it again changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ServerText(String);

impl ServerText {
    /// Cleans `raw` and keeps at most `max_chars` characters of it.
    #[must_use]
    pub fn clean(raw: &str, max_chars: usize) -> Self {
        Self(text::sanitize(raw, max_chars, LineBreaks::Strip).text)
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether there's no text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// An item's lore: the lines under its name, each a [`ServerText`]
/// (ADR-0036, M1).
///
/// At most [`Lore::MAX_LINES`] lines of [`Lore::MAX_LINE`] characters: the
/// lines past that are left out, and a line past that is cut. An empty line
/// is a line, as it is on the screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Lore(Vec<ServerText>);

impl Lore {
    /// The most lines kept.
    pub const MAX_LINES: usize = 16;
    /// The longest line kept, in characters.
    pub const MAX_LINE: usize = 128;

    /// Reads lore from its lines as the server sent them, rendered as plain
    /// text: the first [`Lore::MAX_LINES`], each cleaned.
    #[must_use]
    pub fn read<S: AsRef<str>>(raw: impl IntoIterator<Item = S>) -> Self {
        Self(
            raw.into_iter()
                .take(Self::MAX_LINES)
                .map(|line| ServerText::clean(line.as_ref(), Self::MAX_LINE))
                .collect(),
        )
    }

    /// The lines, top first.
    #[must_use]
    pub fn lines(&self) -> &[ServerText] {
        &self.0
    }

    /// Whether a line holds `text`: `slot.lore_has(text)`. An empty text
    /// is in no line.
    #[must_use]
    pub fn has(&self, text: &str) -> bool {
        !text.is_empty() && self.0.iter().any(|line| line.as_str().contains(text))
    }

    /// The number on the first line that starts with `label`, leading
    /// spaces aside: the first number after the label, read as
    /// [`find_amount`] reads it; `slot.lore_number(label)` (ADR-0036, M6;
    /// P16.7). `None` when no line starts with it, or that line has no
    /// number there; a later line is never read instead.
    #[must_use]
    pub fn number(&self, label: &str, mark: DecimalMark) -> Option<f64> {
        let rest = self
            .0
            .iter()
            .find_map(|line| line.as_str().trim_start().strip_prefix(label))?;
        find_amount(rest, mark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{is_bidi_control, is_invisible};
    use proptest::prelude::*;
    use rstest::rstest;

    /// Whether `c` may be left in a [`ServerText`].
    fn is_kept(c: char) -> bool {
        !c.is_control() && c != '\u{a7}' && !is_bidi_control(c) && !is_invisible(c)
    }

    #[rstest]
    #[case::plain("Create", "Create")]
    #[case::formatting_codes("\u{a7}6\u{a7}lCreate", "Create")]
    #[case::a_trailing_section_sign("Buy\u{a7}", "Buy")]
    #[case::a_line_break("Buy\nnow", "Buynow")]
    #[case::a_carriage_return("Buy\r\nnow", "Buynow")]
    #[case::a_tab("Buy\tnow", "Buynow")]
    #[case::an_escape("\u{1b}[31mred", "[31mred")]
    #[case::a_direction_override("Price: \u{202e}0001", "Price: 0001")]
    #[case::a_zero_width_space("Cre\u{200b}ate", "Create")]
    #[case::a_tag_character("Create\u{e0041}", "Create")]
    #[case::ordinary_text_stays("Gr\u{fc}n \u{1f600} \u{6f22}", "Gr\u{fc}n \u{1f600} \u{6f22}")]
    #[case::spaces_stay("  Create  ", "  Create  ")]
    fn text_is_cleaned_as_chat_is_and_kept_on_one_line(#[case] raw: &str, #[case] text: &str) {
        assert_eq!(ServerText::clean(raw, 64).as_str(), text);
    }

    #[rstest]
    #[case::under("abc", 4, "abc")]
    #[case::at("abcd", 4, "abcd")]
    #[case::over("abcde", 4, "abcd")]
    #[case::on_a_character_boundary("\u{1f600}\u{1f600}\u{1f600}", 2, "\u{1f600}\u{1f600}")]
    #[case::after_cleaning("\u{a7}cab\u{200b}cd", 4, "abcd")]
    #[case::nothing("", 4, "")]
    fn text_is_cut_at_its_bound_in_characters(
        #[case] raw: &str,
        #[case] max: usize,
        #[case] text: &str,
    ) {
        let cleaned = ServerText::clean(raw, max);

        assert_eq!(cleaned.as_str(), text);
        assert_eq!(cleaned.is_empty(), text.is_empty());
    }

    #[test]
    fn lore_keeps_sixteen_lines_of_128_characters() {
        let long = "x".repeat(200);
        let raw: Vec<String> = (0..20)
            .map(|line| {
                if line == 3 {
                    long.clone()
                } else {
                    format!("line {line}")
                }
            })
            .collect();

        let lore = Lore::read(&raw);

        assert_eq!(lore.lines().len(), Lore::MAX_LINES);
        assert_eq!(lore.lines()[0].as_str(), "line 0");
        assert_eq!(lore.lines()[3].as_str(), "x".repeat(Lore::MAX_LINE));
        assert_eq!(lore.lines()[15].as_str(), "line 15");
    }

    #[test]
    fn a_lore_line_is_cleaned_and_an_empty_one_stays() {
        let lore = Lore::read(["\u{a7}7Seller: AfkBot1", "", "Price:\n 12,500"]);

        let lines: Vec<&str> = lore.lines().iter().map(ServerText::as_str).collect();
        assert_eq!(lines, ["Seller: AfkBot1", "", "Price: 12,500"]);
    }

    #[test]
    fn no_lore_is_no_lines() {
        assert_eq!(Lore::read(Vec::<String>::new()).lines(), []);
        assert_eq!(Lore::default().lines(), []);
    }

    /// A listing's lore, as the harness's auction house writes it.
    fn listing() -> Lore {
        Lore::read([
            "",
            "\u{a7}7Seller: AfkBot1",
            "  \u{a7}ePrice: \u{a7}612,500",
            "Price history: 9,000",
            "Expires in 3h",
        ])
    }

    #[rstest]
    #[case::a_price("Price", Some(12_500.0))]
    #[case::with_its_colon("Price:", Some(12_500.0))]
    #[case::the_first_line_that_starts_with_it_only("Price h", Some(9000.0))]
    #[case::a_number_later_in_the_line("Expires in", Some(3.0))]
    #[case::a_line_without_a_number("Seller", None)]
    #[case::no_such_line("Cost", None)]
    #[case::case_counts("price", None)]
    #[case::inside_a_line_isnt_at_its_start("AfkBot1", None)]
    fn a_number_is_read_from_the_first_line_its_label_starts(
        #[case] label: &str,
        #[case] value: Option<f64>,
    ) {
        assert_eq!(listing().number(label, DecimalMark::Point), value);
    }

    #[test]
    fn a_later_line_never_stands_in_for_the_first() {
        let lore = Lore::read(["Price: ask the seller", "Price: 5"]);

        assert_eq!(lore.number("Price", DecimalMark::Point), None);
    }

    #[test]
    fn the_declared_mark_reads_the_number() {
        let lore = Lore::read(["Preis: 1.234,56"]);

        assert_eq!(lore.number("Preis", DecimalMark::Comma), Some(1234.56));
        assert_eq!(lore.number("Preis", DecimalMark::Point), Some(1.234));
    }

    #[rstest]
    #[case::a_seller("Seller: AfkBot1", true)]
    #[case::part_of_a_line("AfkBot", true)]
    #[case::an_empty_line_isnt_found_by_nothing("", false)]
    #[case::another_seller("Seller: AfkBot2", false)]
    #[case::cleaned_text_only("\u{a7}7Seller", false)]
    fn a_text_is_found_in_any_line(#[case] text: &str, #[case] found: bool) {
        assert_eq!(listing().has(text), found);
    }

    proptest! {
        /// Any text a server could send: clean, one line, bounded, and the
        /// same when cleaned again.
        #[test]
        fn any_text_becomes_one_clean_bounded_line(raw in any::<String>(), max in 0_usize..80) {
            let cleaned = ServerText::clean(&raw, max);

            prop_assert!(cleaned.as_str().chars().count() <= max);
            prop_assert!(cleaned.as_str().chars().all(is_kept));
            prop_assert_eq!(ServerText::clean(cleaned.as_str(), max), cleaned);
        }

        /// The same for text made of what a server uses to hide or reorder.
        #[test]
        fn hostile_text_becomes_one_clean_line(
            raw in "[a\u{a7}\n\r\t\u{1b}\u{202e}\u{2066}\u{200b}\u{feff}\u{e0041}\u{85}]{0,64}",
        ) {
            let cleaned = ServerText::clean(&raw, 64);

            prop_assert!(cleaned.as_str().chars().all(|c| c == 'a'));
        }

        #[test]
        fn any_lore_is_bounded_and_clean(raw in proptest::collection::vec(any::<String>(), 0..40)) {
            let lore = Lore::read(&raw);

            prop_assert!(lore.lines().len() <= Lore::MAX_LINES);
            prop_assert_eq!(lore.lines().len(), raw.len().min(Lore::MAX_LINES));
            for line in lore.lines() {
                prop_assert!(line.as_str().chars().count() <= Lore::MAX_LINE);
                prop_assert!(line.as_str().chars().all(is_kept));
            }
        }
    }
}
