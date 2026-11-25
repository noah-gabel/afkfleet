//! Sign editors a server opens, and the declared sign messages a script
//! answers them with (ADR-0036, M16; ADR-0050).
//!
//! - [`SignTemplate`] is a sign the mode declares: up to four lines of
//!   fixed text and typed values, as a declared message is (D8).
//! - [`SignEditor`] is the event: the server opened an editor for a sign,
//!   at its position and on one of its sides.
//! - [`SignDesk`] decides which editor a script may answer: only the one
//!   the server opened last, and only once. Its answer is a
//!   [`SignUpdate`], what fleet-mc sends.

use core::fmt;

use serde::{Deserialize, Serialize};

use super::super::placeholder;
use super::super::template::{
    Given, MessageValue, Part, RenderError, TemplateError, Unresolved, ValueDraft, assemble,
    check_values, longest, resolve, write_values,
};
use super::super::word::is_forbidden_in_declaration;
use super::super::{BlockPosition, Ident, IdentError};
use crate::text;

/// Which side of a sign an editor writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignSide {
    /// The side it was placed facing out.
    Front,
    /// The other side.
    Back,
}

impl SignSide {
    /// The side as a script reads it: `front` or `back`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::Back => "back",
        }
    }
}

/// Why text isn't a [`SignLine`]. It never holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignLineError {
    /// The line is longer than [`SignLine::MAX_LEN`] UTF-16 code units.
    #[error("the line is {units} UTF-16 code units long; a sign takes {max}", max = SignLine::MAX_LEN)]
    TooLong {
        /// Its length in UTF-16 code units.
        units: usize,
    },
    /// The line holds a control character, `§`, a text-direction mark or
    /// an invisible character.
    #[error("the line holds a character it may not, at position {index}")]
    ForbiddenChar {
        /// The character's position, counted in characters.
        index: usize,
    },
}

/// One line a bot writes on a sign: what every server takes as it is.
///
/// At most [`SignLine::MAX_LEN`] UTF-16 code units: Paper cuts a longer
/// line there (its `Paper.maxSignLength`, 80 by default, read in Paper
/// 26.1.2's own code), and vanilla takes up to 384. Nothing that hides or
/// reorders text, and no formatting code, as in a declaration. It may be
/// empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SignLine(String);

impl SignLine {
    /// The longest line, in UTF-16 code units, as a server measures it.
    pub const MAX_LEN: usize = 80;

    /// The line.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for SignLine {
    type Error = SignLineError;

    /// # Errors
    /// [`SignLineError::ForbiddenChar`] for the first such character, then
    /// [`SignLineError::TooLong`].
    fn try_from(line: &str) -> Result<Self, SignLineError> {
        if let Some(index) = line.chars().position(is_forbidden_in_declaration) {
            return Err(SignLineError::ForbiddenChar { index });
        }
        let units = text::utf16_len(line);
        if units > Self::MAX_LEN {
            return Err(SignLineError::TooLong { units });
        }
        Ok(Self(line.to_owned()))
    }
}

/// A sign's four lines, as a bot writes them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SignText([SignLine; 4]);

impl SignText {
    /// Four empty lines: what [`SignAnswer::Cancel`] writes.
    #[must_use]
    pub fn blank() -> Self {
        Self::default()
    }

    /// The four lines, top first.
    #[must_use]
    pub fn lines(&self) -> [&str; 4] {
        let [first, second, third, fourth] = &self.0;
        [
            first.as_str(),
            second.as_str(),
            third.as_str(),
            fourth.as_str(),
        ]
    }
}

/// A declared sign as written, before [`SignTemplate::try_from`] has
/// checked it. Its JSON is `{"key":…,"lines":[…],"values":[…]}`; every
/// field is required, and unknown fields are ignored (ADR-0010).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignDraft {
    /// The key a script names the sign by.
    pub key: String,
    /// One to four lines, top first, each with a `{name}` where a value
    /// goes. The lines it doesn't give are empty.
    pub lines: Vec<String>,
    /// The values the lines hold, all of them together.
    pub values: Vec<ValueDraft>,
}

/// Why a [`SignDraft`] isn't a sign a mode may declare.
///
/// `line` is a line's place, from 0; a `position` is a character's place in
/// the trimmed line, counted in characters; `value` is a value's place in
/// the list, from 0. The text is never included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignTemplateError {
    /// The key isn't an [`Ident`].
    #[error("the sign's key: {0}")]
    Key(IdentError),
    /// There are no lines, or more than [`SignTemplate::MAX_LINES`].
    #[error("the sign has {count} lines; it takes 1 to {max}", max = SignTemplate::MAX_LINES)]
    Lines {
        /// How many it has.
        count: usize,
    },
    /// Every line is empty or only whitespace.
    #[error("the sign is empty")]
    Empty,
    /// A line holds a control character, `§`, a text-direction mark or an
    /// invisible character.
    #[error("line {line} holds a character it may not, at position {position}")]
    ForbiddenChar {
        /// The line's place.
        line: usize,
        /// The character's place.
        position: usize,
    },
    /// A line holds a brace that isn't part of a `{name}`, `{{` or `}}`.
    #[error(
        "line {line} holds a brace that isn't part of a value or doubled, at position {position}"
    )]
    Brace {
        /// The line's place.
        line: usize,
        /// The brace's place.
        position: usize,
    },
    /// The values aren't ones a declared message could hold: the same
    /// checks and the same errors as a message's values (P15.2).
    #[error("the sign's values: {0}")]
    Values(TemplateError),
    /// A line names a value that isn't declared.
    #[error("line {line} names a value it doesn't declare, at position {position}")]
    Undeclared {
        /// The line's place.
        line: usize,
        /// The `{`'s place.
        position: usize,
    },
    /// A line names a value a second time, in it or after another line.
    #[error("line {line} names a value a second time, at position {position}")]
    Repeated {
        /// The line's place.
        line: usize,
        /// The second `{`'s place.
        position: usize,
    },
    /// A declared value isn't in any line.
    #[error("value {value} isn't on the sign")]
    Unused {
        /// The value's place.
        value: usize,
    },
    /// The longest text a line can become is longer than
    /// [`SignLine::MAX_LEN`] UTF-16 code units.
    #[error("line {line} can be {units} UTF-16 code units long; a sign takes {max}", max = SignLine::MAX_LEN)]
    TooLong {
        /// The line's place.
        line: usize,
        /// Its longest length in UTF-16 code units.
        units: usize,
    },
}

/// A sign a scripted mode declares: up to four lines of fixed text and
/// typed values, the only text its script can write on a sign (ADR-0036,
/// M16; D8).
///
/// It's held to a declared message's rules (P15.2): a line's text is
/// trimmed and holds no line break, no `§`, no text-direction mark and no
/// invisible character; a value is a whole number or a decimal number in a
/// range, or one of a closed list of words; the values, at most
/// [`MessageTemplate::MAX_VALUES`](super::super::MessageTemplate::MAX_VALUES)
/// for the whole sign, are each used once on it. **Each line fits a sign
/// with every value at its longest,** checked when it's declared, and
/// [`render`](Self::render) puts every finished line through
/// [`SignLine`]'s own validation again.
///
/// Two things differ from a message, since a sign's line is never a
/// command: a line may start with a value, as a price alone on its line
/// does, and a line may be empty. It reads and writes the JSON of a
/// [`SignDraft`], validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SignDraft", into = "SignDraft")]
pub struct SignTemplate {
    key: Ident,
    lines: Vec<String>,
    parts: Vec<Vec<Part>>,
    values: Vec<MessageValue>,
}

impl SignTemplate {
    /// The most lines a sign has.
    pub const MAX_LINES: usize = 4;

    /// The key a script names the sign by.
    #[must_use]
    pub const fn key(&self) -> &Ident {
        &self.key
    }

    /// The lines as declared, trimmed, with their values in braces.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The values, in the order they were declared.
    #[must_use]
    pub fn values(&self) -> &[MessageValue] {
        &self.values
    }

    /// The longest each declared line can be, in UTF-16 code units.
    #[must_use]
    pub fn max_lens(&self) -> Vec<usize> {
        self.parts
            .iter()
            .map(|parts| longest(parts, &self.values))
            .collect()
    }

    /// Makes the sign's lines from `given`, as
    /// [`MessageTemplate::render`](super::super::MessageTemplate::render)
    /// makes a message's text: the lines it doesn't declare are empty.
    ///
    /// # Errors
    /// As a message's, and [`RenderError::SignLine`] for a finished line
    /// that isn't a [`SignLine`].
    pub fn render(&self, given: &[(&str, Given<'_>)]) -> Result<SignText, RenderError> {
        let written = write_values(&self.values, given)?;
        let mut text = SignText::blank();
        for (line, (parts, slot)) in self.parts.iter().zip(text.0.iter_mut()).enumerate() {
            *slot = SignLine::try_from(assemble(parts, &written).as_str())
                .map_err(|reason| RenderError::SignLine { line, reason })?;
        }
        Ok(text)
    }
}

impl TryFrom<SignDraft> for SignTemplate {
    type Error = SignTemplateError;

    /// Checks a declared sign.
    ///
    /// The checks run in this order, the first problem winning: the key;
    /// how many lines; each line's characters and braces, top first; an
    /// empty sign; the values, as a message's; each line's names, top
    /// first; a declared value no line holds; each line's longest text,
    /// top first.
    fn try_from(draft: SignDraft) -> Result<Self, SignTemplateError> {
        let key = Ident::try_from(draft.key).map_err(SignTemplateError::Key)?;
        let count = draft.lines.len();
        if !(1..=Self::MAX_LINES).contains(&count) {
            return Err(SignTemplateError::Lines { count });
        }
        let lines: Vec<String> = draft
            .lines
            .iter()
            .map(|line| line.trim().to_owned())
            .collect();
        let mut parsed = Vec::with_capacity(count);
        for (at, line) in lines.iter().enumerate() {
            if let Some(position) = line.chars().position(is_forbidden_in_declaration) {
                return Err(SignTemplateError::ForbiddenChar { line: at, position });
            }
            let parts = placeholder::parse(line).map_err(|brace| SignTemplateError::Brace {
                line: at,
                position: brace.position,
            })?;
            parsed.push(parts);
        }
        if lines.iter().all(String::is_empty) {
            return Err(SignTemplateError::Empty);
        }
        let values = check_values(draft.values).map_err(SignTemplateError::Values)?;
        let mut used = vec![false; values.len()];
        let mut parts = Vec::with_capacity(count);
        for (at, line) in parsed.into_iter().enumerate() {
            let line_parts =
                resolve(line, &values, &mut used).map_err(|unresolved| match unresolved {
                    Unresolved::Undeclared(position) => {
                        SignTemplateError::Undeclared { line: at, position }
                    }
                    Unresolved::Repeated(position) => {
                        SignTemplateError::Repeated { line: at, position }
                    }
                })?;
            parts.push(line_parts);
        }
        if let Some(value) = used.iter().position(|used| !used) {
            return Err(SignTemplateError::Unused { value });
        }
        for (at, line_parts) in parts.iter().enumerate() {
            let units = longest(line_parts, &values);
            if units > SignLine::MAX_LEN {
                return Err(SignTemplateError::TooLong { line: at, units });
            }
        }
        Ok(Self {
            key,
            lines,
            parts,
            values,
        })
    }
}

impl From<SignTemplate> for SignDraft {
    fn from(template: SignTemplate) -> Self {
        Self {
            key: template.key.into(),
            lines: template.lines,
            values: template
                .values
                .iter()
                .map(|value| ValueDraft {
                    name: value.name().as_str().to_owned(),
                    kind: value.kind(),
                })
                .collect(),
        }
    }
}

/// The server opened a sign's editor: the event a script's `on_sign_open`
/// hook runs for (ADR-0036, M16).
///
/// Only [`SignDesk::opened`] makes one, so the desk can tell one opening
/// from the next, even of the same sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignEditor {
    at: BlockPosition,
    side: SignSide,
    opening: u64,
}

impl SignEditor {
    /// Where the sign is.
    #[must_use]
    pub const fn at(&self) -> BlockPosition {
        self.at
    }

    /// Which side the editor writes.
    #[must_use]
    pub const fn side(&self) -> SignSide {
        self.side
    }
}

/// How a script answers a sign editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignAnswer {
    /// `sign.write(key, values)`: a declared sign's lines.
    Write(SignText),
    /// `sign.cancel()`: nothing written, four empty lines, as a vanilla
    /// client sends when its player leaves an empty sign.
    Cancel,
}

/// The answer that goes out to the server for one editor: the sign's
/// position and side with its four lines, as `ServerboundSignUpdate`
/// carries them (ADR-0036, M16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignUpdate {
    at: BlockPosition,
    side: SignSide,
    text: SignText,
    cancelled: bool,
}

impl SignUpdate {
    /// Where the sign is.
    #[must_use]
    pub const fn at(&self) -> BlockPosition {
        self.at
    }

    /// Which side it writes.
    #[must_use]
    pub const fn side(&self) -> SignSide {
        self.side
    }

    /// The four lines, top first.
    #[must_use]
    pub fn lines(&self) -> [&str; 4] {
        self.text.lines()
    }

    /// Whether the script cancelled: the lines are empty.
    #[must_use]
    pub const fn cancelled(&self) -> bool {
        self.cancelled
    }
}

/// Why a sign editor wasn't answered: `result.reason` (ADR-0036, M16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignRefusal {
    /// No sign editor is open: none was opened, or the one opened was
    /// ended by a screen, a death or a world change.
    NoSign,
    /// The server opened another editor after the one the hook saw.
    SignChanged,
    /// The editor was answered already.
    Answered,
}

impl SignRefusal {
    /// Every reason, in a fixed order.
    pub const ALL: [Self; 3] = [Self::NoSign, Self::SignChanged, Self::Answered];

    /// The reason as a script reads it in `result.reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoSign => "no_sign",
            Self::SignChanged => "sign_changed",
            Self::Answered => "sign_answered",
        }
    }
}

impl fmt::Display for SignRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the last editor stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// Waiting for its answer.
    Open,
    /// Answered.
    Answered,
    /// Ended without an answer: a screen took its place, the bot died or
    /// changed worlds.
    Ended,
}

/// Which sign editor a script may answer: **only the one the server opened
/// last, and only once** (ADR-0036, M16; ADR-0050). A pure decision; its
/// owner is the session.
///
/// An editor nobody answers is never answered by anything else: the desk
/// sends nothing by itself, though a vanilla client sends its lines
/// whenever its editor closes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignDesk {
    /// How many editors the session has opened.
    openings: u64,
    /// The last one, and where it stands.
    last: Option<(SignEditor, Standing)>,
}

impl SignDesk {
    /// A desk for a new session: no editor opened.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The server opened an editor for the sign at `at`, on its side
    /// `side`. Any earlier one can't be answered any more.
    pub fn opened(&mut self, at: BlockPosition, side: SignSide) -> SignEditor {
        // A session opens far fewer than 2^64 editors; saturating keeps
        // the count from going round to an opening handed out before.
        self.openings = self.openings.saturating_add(1);
        let editor = SignEditor {
            at,
            side,
            opening: self.openings,
        };
        self.last = Some((editor, Standing::Open));
        editor
    }

    /// The open editor ended unanswered: a screen opened in its place, as
    /// in a vanilla client, or the bot died or changed worlds.
    pub fn ended(&mut self) {
        if let Some((_, standing @ Standing::Open)) = &mut self.last {
            *standing = Standing::Ended;
        }
    }

    /// The editor waiting for an answer.
    #[must_use]
    pub fn open(&self) -> Option<&SignEditor> {
        match &self.last {
            Some((editor, Standing::Open)) => Some(editor),
            Some((_, Standing::Answered | Standing::Ended)) | None => None,
        }
    }

    /// Answers `seen`, the editor the hook saw, with `answer`.
    ///
    /// # Errors
    /// [`SignRefusal::SignChanged`] if another editor opened since `seen`,
    /// then [`SignRefusal::Answered`] if `seen` was answered, then
    /// [`SignRefusal::NoSign`] if it ended or none was ever opened.
    pub fn answer(
        &mut self,
        seen: &SignEditor,
        answer: SignAnswer,
    ) -> Result<SignUpdate, SignRefusal> {
        let Some((last, standing)) = &mut self.last else {
            return Err(SignRefusal::NoSign);
        };
        if last != seen {
            return Err(SignRefusal::SignChanged);
        }
        match standing {
            Standing::Answered => return Err(SignRefusal::Answered),
            Standing::Ended => return Err(SignRefusal::NoSign),
            Standing::Open => *standing = Standing::Answered,
        }
        let (text, cancelled) = match answer {
            SignAnswer::Write(text) => (text, false),
            SignAnswer::Cancel => (SignText::blank(), true),
        };
        Ok(SignUpdate {
            at: last.at,
            side: last.side,
            text,
            cancelled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::ValueKind;
    use proptest::prelude::*;
    use rstest::rstest;

    fn integer(name: &str, min: i64, max: i64) -> ValueDraft {
        ValueDraft {
            name: name.to_owned(),
            kind: ValueKind::Integer { min, max },
        }
    }

    fn word(name: &str, options: &[&str]) -> ValueDraft {
        ValueDraft {
            name: name.to_owned(),
            kind: ValueKind::Word {
                options: options.iter().map(|&option| option.to_owned()).collect(),
            },
        }
    }

    fn draft(lines: &[&str], values: Vec<ValueDraft>) -> SignDraft {
        SignDraft {
            key: "price".to_owned(),
            lines: lines.iter().map(|&line| line.to_owned()).collect(),
            values,
        }
    }

    fn template(lines: &[&str], values: Vec<ValueDraft>) -> SignTemplate {
        SignTemplate::try_from(draft(lines, values)).unwrap()
    }

    fn refused(lines: &[&str], values: Vec<ValueDraft>) -> SignTemplateError {
        SignTemplate::try_from(draft(lines, values)).unwrap_err()
    }

    /// The user's server's price sign: the price alone on the first line.
    fn price_sign() -> SignTemplate {
        template(&["{price}"], vec![integer("price", 1, 1_000_000)])
    }

    // --- A sign's line ---

    #[rstest]
    #[case::empty("")]
    #[case::a_price("1500")]
    #[case::eighty("x".repeat(80))]
    #[case::forty_emoji("\u{1f600}".repeat(40))]
    fn a_line_a_server_takes_as_it_is_is_a_sign_line(#[case] line: String) {
        assert_eq!(SignLine::try_from(line.as_str()).unwrap().as_str(), line);
    }

    #[rstest]
    #[case::eighty_one("x".repeat(81), SignLineError::TooLong { units: 81 })]
    #[case::emoji_count_twice("\u{1f600}".repeat(41), SignLineError::TooLong { units: 82 })]
    #[case::a_line_break("a\nb".to_owned(), SignLineError::ForbiddenChar { index: 1 })]
    #[case::a_section_sign("\u{a7}c5".to_owned(), SignLineError::ForbiddenChar { index: 0 })]
    #[case::a_direction_mark("5\u{202e}".to_owned(), SignLineError::ForbiddenChar { index: 1 })]
    #[case::a_zero_width_space("1\u{200b}5".to_owned(), SignLineError::ForbiddenChar { index: 1 })]
    #[case::the_character_before_the_length(
        format!("{}\u{7}", "x".repeat(90)),
        SignLineError::ForbiddenChar { index: 90 }
    )]
    fn a_line_a_server_would_cut_or_that_hides_text_isnt_one(
        #[case] line: String,
        #[case] error: SignLineError,
    ) {
        assert_eq!(SignLine::try_from(line.as_str()), Err(error));
    }

    // --- Declaring ---

    #[test]
    fn a_price_alone_on_its_line_is_written_there() {
        let sign = price_sign();

        let text = sign.render(&[("price", Given::Integer(1500))]).unwrap();

        assert_eq!(text.lines(), ["1500", "", "", ""]);
        assert_eq!(sign.key().as_str(), "price");
        assert_eq!(sign.lines(), ["{price}"]);
        assert_eq!(sign.values().len(), 1);
        assert_eq!(sign.max_lens(), [7]);
    }

    #[test]
    fn values_go_on_any_line_each_once() {
        let sign = template(
            &[" Sell {n} ", "", "for {price}", "{unit}"],
            vec![
                integer("price", 1, 999),
                integer("n", 1, 64),
                word("unit", &["coins", "gems"]),
            ],
        );

        let text = sign
            .render(&[
                ("unit", Given::Text("gems")),
                ("price", Given::Integer(250)),
                ("n", Given::Integer(3)),
            ])
            .unwrap();

        assert_eq!(text.lines(), ["Sell 3", "", "for 250", "gems"]);
        assert_eq!(sign.lines(), ["Sell {n}", "", "for {price}", "{unit}"]);
        assert_eq!(sign.max_lens(), [7, 0, 7, 5]);
    }

    #[test]
    fn a_fixed_sign_needs_no_values() {
        let sign = template(&["^^^", "Enter a price"], Vec::new());

        assert_eq!(
            sign.render(&[]).unwrap().lines(),
            ["^^^", "Enter a price", "", ""]
        );
    }

    #[test]
    fn a_line_at_eighty_units_at_its_longest_is_declared_and_one_past_is_refused() {
        let fits = format!("{} {{price}}", "x".repeat(72));
        let past = format!("{} {{price}}", "x".repeat(73));

        let sign = template(&["", &fits], vec![integer("price", 1, 1_000_000)]);

        assert_eq!(sign.max_lens(), [0, 80]);
        let text = sign
            .render(&[("price", Given::Integer(1_000_000))])
            .unwrap();
        assert_eq!(crate::text::utf16_len(text.lines()[1]), 80);
        assert_eq!(
            refused(&["", &past], vec![integer("price", 1, 1_000_000)]),
            SignTemplateError::TooLong { line: 1, units: 81 }
        );
    }

    #[rstest]
    #[case::a_bad_key_first(
        SignDraft { key: "Price".to_owned(), lines: Vec::new(), values: Vec::new() },
        SignTemplateError::Key(IdentError::InvalidChar { index: 0 })
    )]
    #[case::no_lines(draft(&[], Vec::new()), SignTemplateError::Lines { count: 0 })]
    #[case::five_lines(draft(&["a", "b", "c", "d", "e"], Vec::new()), SignTemplateError::Lines { count: 5 })]
    #[case::only_empty_lines(draft(&["", "  ", ""], Vec::new()), SignTemplateError::Empty)]
    #[case::a_line_break(
        draft(&["ok", "a\nb"], Vec::new()),
        SignTemplateError::ForbiddenChar { line: 1, position: 1 }
    )]
    #[case::a_section_sign(
        draft(&["\u{a7}c{price}"], vec![integer("price", 1, 9)]),
        SignTemplateError::ForbiddenChar { line: 0, position: 0 }
    )]
    #[case::positions_are_in_the_trimmed_line(
        draft(&["  a\u{200b}"], Vec::new()),
        SignTemplateError::ForbiddenChar { line: 0, position: 1 }
    )]
    #[case::a_lone_brace(draft(&["a", "{"], Vec::new()), SignTemplateError::Brace { line: 1, position: 0 })]
    #[case::the_values_as_a_message_checks_them(
        draft(&["{a}"], vec![integer("a", 2, 1)]),
        SignTemplateError::Values(TemplateError::Range { value: 0 })
    )]
    #[case::nine_values(
        draft(
            &["{a}{b}{c}", "{d}{e}{f}", "{g}{h}{i}"],
            ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
                .into_iter()
                .map(|name| integer(name, 0, 1))
                .collect()
        ),
        SignTemplateError::Values(TemplateError::TooManyValues { count: 9 })
    )]
    #[case::a_name_that_isnt_declared(
        draft(&["{price}", "x {b}"], vec![integer("price", 1, 9)]),
        SignTemplateError::Undeclared { line: 1, position: 2 }
    )]
    #[case::a_value_twice_on_one_line(
        draft(&["{a} {a}"], vec![integer("a", 1, 9)]),
        SignTemplateError::Repeated { line: 0, position: 4 }
    )]
    #[case::a_value_on_two_lines(
        draft(&["{a}", "{a}"], vec![integer("a", 1, 9)]),
        SignTemplateError::Repeated { line: 1, position: 0 }
    )]
    #[case::a_value_on_no_line(
        draft(&["{a}"], vec![integer("a", 1, 9), integer("b", 1, 9)]),
        SignTemplateError::Unused { value: 1 }
    )]
    #[case::the_first_line_too_long_wins(
        draft(&[&"x".repeat(81), &"y".repeat(90)], Vec::new()),
        SignTemplateError::TooLong { line: 0, units: 81 }
    )]
    fn refuses_a_sign_that_isnt_one(#[case] draft: SignDraft, #[case] error: SignTemplateError) {
        assert_eq!(SignTemplate::try_from(draft), Err(error));
    }

    #[rstest]
    #[case::the_line_count_before_a_lines_characters(
        draft(&["\u{7}", "b", "c", "d", "e"], Vec::new()),
        SignTemplateError::Lines { count: 5 }
    )]
    #[case::characters_before_the_values(
        draft(&["{", "x"], vec![integer("a", 2, 1)]),
        SignTemplateError::Brace { line: 0, position: 0 }
    )]
    #[case::the_values_before_the_names(
        draft(&["{b}"], vec![integer("a", 2, 1)]),
        SignTemplateError::Values(TemplateError::Range { value: 0 })
    )]
    #[case::a_name_before_an_unused_value(
        draft(&["{b}"], vec![integer("a", 1, 2)]),
        SignTemplateError::Undeclared { line: 0, position: 0 }
    )]
    #[case::an_unused_value_before_a_long_line(
        draft(&[&"x".repeat(81)], vec![integer("a", 1, 2)]),
        SignTemplateError::Unused { value: 0 }
    )]
    fn the_first_problem_wins(#[case] draft: SignDraft, #[case] error: SignTemplateError) {
        assert_eq!(SignTemplate::try_from(draft), Err(error));
    }

    #[rstest]
    #[case::missing(&[], RenderError::Missing { value: 0 })]
    #[case::undeclared(&[("price", Given::Integer(5)), ("tip", Given::Integer(1))], RenderError::Undeclared)]
    #[case::out_of_range(&[("price", Given::Integer(0))], RenderError::OutOfRange { value: 0 })]
    #[case::wrong_kind(&[("price", Given::Text("5"))], RenderError::WrongKind { value: 0 })]
    fn values_are_checked_as_a_messages_are(
        #[case] given: &[(&str, Given<'_>)],
        #[case] error: RenderError,
    ) {
        assert_eq!(price_sign().render(given), Err(error));
    }

    /// The net behind a declaration's checks: a template whose parts hold
    /// what no declaration lets in still writes no sign.
    #[test]
    fn a_finished_line_that_isnt_a_sign_line_is_refused() {
        let broken = SignTemplate {
            key: Ident::try_from("price").unwrap(),
            lines: vec![String::new(), "x".to_owned()],
            parts: vec![Vec::new(), vec![Part::Literal("a\nb".to_owned())]],
            values: Vec::new(),
        };

        assert_eq!(
            broken.render(&[]),
            Err(RenderError::SignLine {
                line: 1,
                reason: SignLineError::ForbiddenChar { index: 1 }
            })
        );
    }

    #[test]
    fn errors_name_a_place_and_never_the_text() {
        assert_eq!(
            SignTemplateError::TooLong { line: 2, units: 90 }.to_string(),
            "line 2 can be 90 UTF-16 code units long; a sign takes 80"
        );
        assert_eq!(
            SignLineError::ForbiddenChar { index: 3 }.to_string(),
            "the line holds a character it may not, at position 3"
        );
        assert_eq!(
            RenderError::SignLine {
                line: 0,
                reason: SignLineError::TooLong { units: 81 }
            }
            .to_string(),
            "line 0 of the sign: the line is 81 UTF-16 code units long; a sign takes 80"
        );
    }

    #[test]
    fn a_declared_sign_reads_and_writes_its_json() {
        let json = concat!(
            r#"{"key":"price","lines":["{price}","^^^"],"values":["#,
            r#"{"name":"price","type":"integer","min":1,"max":1000000}]}"#
        );

        let sign: SignTemplate = serde_json::from_str(json).unwrap();

        assert_eq!(sign.key().as_str(), "price");
        assert_eq!(serde_json::to_string(&sign).unwrap(), json);
        assert!(
            serde_json::from_str::<SignTemplate>(r#"{"key":"p","lines":[],"values":[]}"#).is_err()
        );
        assert!(serde_json::from_str::<SignTemplate>(r#"{"key":"p","values":[]}"#).is_err());
    }

    // --- The desk ---

    const AT: BlockPosition = BlockPosition { x: 1, y: 64, z: -3 };

    fn written(price: i64) -> SignAnswer {
        SignAnswer::Write(
            price_sign()
                .render(&[("price", Given::Integer(price))])
                .unwrap(),
        )
    }

    #[test]
    fn the_editor_opened_last_is_answered_with_its_place_and_side() {
        let mut desk = SignDesk::new();
        let editor = desk.opened(AT, SignSide::Back);

        assert_eq!(desk.open(), Some(&editor));
        let update = desk.answer(&editor, written(1500)).unwrap();

        assert_eq!(update.at(), AT);
        assert_eq!(update.side(), SignSide::Back);
        assert_eq!(update.lines(), ["1500", "", "", ""]);
        assert!(!update.cancelled());
        assert_eq!(desk.open(), None);
        assert_eq!(editor.at(), AT);
        assert_eq!(editor.side(), SignSide::Back);
    }

    #[test]
    fn a_cancel_writes_four_empty_lines() {
        let mut desk = SignDesk::new();
        let editor = desk.opened(AT, SignSide::Front);

        let update = desk.answer(&editor, SignAnswer::Cancel).unwrap();

        assert_eq!(update.lines(), ["", "", "", ""]);
        assert!(update.cancelled());
    }

    #[test]
    fn an_editor_is_answered_only_once() {
        let mut desk = SignDesk::new();
        let editor = desk.opened(AT, SignSide::Front);
        desk.answer(&editor, written(1500)).unwrap();

        assert_eq!(desk.answer(&editor, written(9)), Err(SignRefusal::Answered));
        assert_eq!(
            desk.answer(&editor, SignAnswer::Cancel),
            Err(SignRefusal::Answered)
        );
    }

    #[test]
    fn only_the_editor_opened_last_is_answered() {
        let mut desk = SignDesk::new();
        let first = desk.opened(AT, SignSide::Front);
        // The same sign, opened again: a new editor.
        let second = desk.opened(AT, SignSide::Front);

        assert_ne!(first, second);
        assert_eq!(
            desk.answer(&first, written(1500)),
            Err(SignRefusal::SignChanged)
        );
        assert!(desk.answer(&second, written(1500)).is_ok());
        // Even once the last one is answered, the first stays out of reach.
        assert_eq!(
            desk.answer(&first, written(1500)),
            Err(SignRefusal::SignChanged)
        );
    }

    #[test]
    fn an_editor_that_ended_is_answered_by_nothing() {
        let mut desk = SignDesk::new();
        let editor = desk.opened(AT, SignSide::Front);

        desk.ended();

        assert_eq!(desk.open(), None);
        assert_eq!(
            desk.answer(&editor, written(1500)),
            Err(SignRefusal::NoSign)
        );
    }

    #[test]
    fn a_desk_that_opened_nothing_answers_nothing() {
        let mut other = SignDesk::new();
        let editor = other.opened(AT, SignSide::Front);
        let mut desk = SignDesk::new();

        assert_eq!(
            desk.answer(&editor, SignAnswer::Cancel),
            Err(SignRefusal::NoSign)
        );
        desk.ended();
        assert_eq!(desk.open(), None);
    }

    #[rstest]
    #[case::no_sign(SignRefusal::NoSign, "no_sign")]
    #[case::sign_changed(SignRefusal::SignChanged, "sign_changed")]
    #[case::answered(SignRefusal::Answered, "sign_answered")]
    fn each_reason_reads_as_a_script_reads_it(#[case] reason: SignRefusal, #[case] text: &str) {
        assert_eq!(reason.as_str(), text);
        assert_eq!(reason.to_string(), text);
        assert!(SignRefusal::ALL.contains(&reason));
    }

    #[test]
    fn each_side_reads_as_a_script_reads_it() {
        assert_eq!(SignSide::Front.as_str(), "front");
        assert_eq!(SignSide::Back.as_str(), "back");
    }

    #[derive(Debug, Clone)]
    enum Step {
        Open,
        End,
        /// The hook that saw the editor `back` openings ago answers.
        Answer {
            back: usize,
            cancel: bool,
        },
    }

    fn any_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            Just(Step::Open),
            Just(Step::End),
            (0_usize..3, any::<bool>()).prop_map(|(back, cancel)| Step::Answer { back, cancel }),
            Just(Step::Answer {
                back: 0,
                cancel: false
            }),
        ]
    }

    proptest! {
        /// Whatever opens and ends when, an answer goes out only to the
        /// editor opened last, only while it's open, and at most once.
        #[test]
        fn only_the_last_open_editor_is_answered_and_once(
            steps in proptest::collection::vec(any_step(), 1..60),
        ) {
            let mut desk = SignDesk::new();
            let mut editors: Vec<SignEditor> = Vec::new();
            let mut ended: Vec<SignEditor> = Vec::new();
            let mut answered: Vec<SignEditor> = Vec::new();
            for step in steps {
                match step {
                    Step::Open => editors.push(desk.opened(AT, SignSide::Front)),
                    Step::End => {
                        if let Some(last) = editors.last() {
                            ended.push(*last);
                        }
                        desk.ended();
                    }
                    Step::Answer { back, cancel } => {
                        let Some(seen) = editors.iter().rev().nth(back).copied() else {
                            continue;
                        };
                        let answer = if cancel { SignAnswer::Cancel } else { written(5) };
                        if desk.answer(&seen, answer).is_ok() {
                            prop_assert_eq!(editors.last(), Some(&seen));
                            prop_assert!(!ended.contains(&seen));
                            prop_assert!(!answered.contains(&seen));
                            answered.push(seen);
                        }
                    }
                }
                prop_assert_eq!(
                    desk.open(),
                    editors
                        .last()
                        .filter(|last| !ended.contains(last) && !answered.contains(last))
                );
            }
        }

        /// Any declared sign that's accepted writes lines a server takes
        /// as they are, at most its declared longest each.
        #[test]
        fn any_accepted_sign_writes_lines_a_server_takes(
            lines in proptest::collection::vec("[ a-c{}\u{a7}\u{1f600}]{0,50}|\\{n\\}", 0..6),
            min in -1000_i64..1000,
            more in 0_i64..1_000_000_000,
            n in any::<i64>(),
        ) {
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            let declared = SignTemplate::try_from(draft(&lines, vec![integer("n", min, min + more)]));
            if let Ok(sign) = declared
                && let Ok(text) = sign.render(&[("n", Given::Integer(n))])
            {
                for (line, max) in text.lines().iter().zip(sign.max_lens()) {
                    prop_assert!(crate::text::utf16_len(line) <= max);
                    prop_assert!(SignLine::try_from(*line).is_ok());
                }
                let again = SignTemplate::try_from(SignDraft::from(sign.clone()));
                prop_assert_eq!(again.as_ref(), Ok(&sign));
            }
        }
    }
}
