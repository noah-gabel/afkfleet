//! [`ChatPattern`]: a line of the server's own chat a scripted mode
//! declares it reads, and [`candidate_line`], which lines may be read at
//! all (ADR-0037).

use serde::{Deserialize, Serialize};

use super::amount::{self, DecimalMark};
use super::placeholder::{self, Segment};
use super::word::{Word, WordList, WordListError, is_forbidden_in_declaration};
use super::{Ident, IdentError};
use crate::chat::{ChatFormats, ChatKind, IncomingChat};

/// When a [`ChatPattern`] counts. Every pattern says which: there is no
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Only inside a window the script opened after a command of its own
    /// (ADR-0037, C4).
    Reply,
    /// Whenever the bot is online, with no window: the riskier kind, which
    /// the app marks (ADR-0037, C5).
    AnyTime,
}

/// What a pattern's capture reads, as written in a mode's JSON: tagged with
/// its `"type"`, e.g. `{"type":"amount","min":0,"max":1000,"decimal_mark":"."}`.
///
/// There is no kind for free text (ADR-0037, C3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaptureKind {
    /// A whole number, as [`amount::read_integer`] reads it, from `min` to
    /// `max`.
    Integer {
        /// The smallest value that counts.
        min: i64,
        /// The largest value that counts.
        max: i64,
    },
    /// An amount, as [`amount::read_amount`] reads it, from `min` to `max`.
    Amount {
        /// The smallest value that counts.
        min: f64,
        /// The largest value that counts.
        max: f64,
        /// Which character comes before its decimals.
        decimal_mark: DecimalMark,
    },
    /// One of a closed list of words, the longest that fits.
    Word {
        /// The words, as [`WordList`] takes them.
        options: Vec<String>,
    },
}

/// A pattern's capture as written, before [`ChatPattern::try_from`] has
/// checked it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureDraft {
    /// The capture's name, as the text writes it in braces.
    pub name: String,
    /// What it reads.
    #[serde(flatten)]
    pub kind: CaptureKind,
}

/// A chat pattern as written, before [`ChatPattern::try_from`] has checked
/// it. Its JSON is `{"key":…,"when":…,"text":…,"captures":[…]}`; every
/// field is required, and unknown fields are ignored (ADR-0010).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternDraft {
    /// The key a script knows the pattern by.
    pub key: String,
    /// When it counts.
    pub when: When,
    /// The line, with a `{name}` where each capture is.
    pub text: String,
    /// The captures the text holds.
    pub captures: Vec<CaptureDraft>,
}

/// Why a [`PatternDraft`] isn't a pattern a mode may declare.
///
/// A `position` is a character's place in the trimmed text, counted in
/// characters; `capture` is a capture's place in the list, from 0. The text
/// is never included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    /// The key isn't an [`Ident`].
    #[error("the pattern's key: {0}")]
    Key(IdentError),
    /// The text is empty or only whitespace.
    #[error("the pattern is empty")]
    Empty,
    /// The text is longer than [`ChatPattern::MAX_LEN`] characters as
    /// written.
    #[error("the pattern is {chars} characters long; the limit is {max}", max = ChatPattern::MAX_LEN)]
    TooLong {
        /// Its length in characters, as written.
        chars: usize,
    },
    /// The text holds a character received chat never has: a line break,
    /// another control character, `§`, a text-direction mark or an
    /// invisible character. Received text is sanitized, so the pattern
    /// could never fit.
    #[error("the pattern holds a character received chat never has, at position {position}")]
    ForbiddenChar {
        /// The character's place.
        position: usize,
    },
    /// The text holds a brace that isn't part of a `{name}`, `{{` or `}}`.
    #[error(
        "the pattern holds a brace that isn't part of a capture or doubled, at position {position}"
    )]
    Brace {
        /// The brace's place.
        position: usize,
    },
    /// Two captures are side by side, with no text between them.
    #[error("the pattern has no text before the capture at position {position}")]
    AdjacentCaptures {
        /// The second capture's `{`'s place.
        position: usize,
    },
    /// The text outside the captures holds fewer than
    /// [`ChatPattern::MIN_LITERAL`] characters that aren't whitespace, so
    /// the pattern would say next to nothing about the line.
    #[error("the pattern has {chars} characters of its own text; it needs {min}", min = ChatPattern::MIN_LITERAL)]
    TooLittleText {
        /// How many it has.
        chars: usize,
    },
    /// There are more than [`ChatPattern::MAX_CAPTURES`] captures.
    #[error("the pattern has {count} captures; the limit is {max}", max = ChatPattern::MAX_CAPTURES)]
    TooManyCaptures {
        /// How many it has.
        count: usize,
    },
    /// A capture's name isn't an [`Ident`].
    #[error("capture {capture}'s name: {reason}")]
    CaptureName {
        /// The capture's place.
        capture: usize,
        /// Why it isn't a name.
        reason: IdentError,
    },
    /// Two captures have the same name.
    #[error("capture {capture} has the name of an earlier one")]
    DuplicateCapture {
        /// The second one's place.
        capture: usize,
    },
    /// A number's range is empty, isn't a number, or reaches past
    /// [`amount::MAX_AMOUNT`].
    #[error("capture {capture}'s range isn't one")]
    Range {
        /// The capture's place.
        capture: usize,
    },
    /// A capture's words aren't a [`WordList`].
    #[error("capture {capture}'s words: {reason}")]
    Words {
        /// The capture's place.
        capture: usize,
        /// Why they aren't a list.
        reason: WordListError,
    },
    /// The text names a capture that isn't declared.
    #[error("the pattern names a capture it doesn't declare, at position {position}")]
    Undeclared {
        /// The `{`'s place.
        position: usize,
    },
    /// The text names a capture a second time.
    #[error("the pattern names a capture a second time, at position {position}")]
    Repeated {
        /// The second `{`'s place.
        position: usize,
    },
    /// A declared capture isn't in the text.
    #[error("capture {capture} isn't in the pattern")]
    Unused {
        /// The capture's place.
        capture: usize,
    },
}

/// A value a pattern read from a line: a number, or one of the pattern's
/// own declared words. Never text a server or a player wrote (ADR-0037, C6).
#[derive(Debug, Clone, PartialEq)]
pub enum Captured {
    /// A whole number, inside its declared range.
    Integer(i64),
    /// An amount, inside its declared range.
    Amount(f64),
    /// One of the capture's declared words.
    Word(Word),
}

/// One capture of a [`ChatPattern`].
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    name: Ident,
    kind: Checked,
}

impl Capture {
    /// The capture's name.
    #[must_use]
    pub const fn name(&self) -> &Ident {
        &self.name
    }

    /// What it reads, as declared.
    #[must_use]
    pub fn kind(&self) -> CaptureKind {
        match &self.kind {
            Checked::Integer { min, max } => CaptureKind::Integer {
                min: *min,
                max: *max,
            },
            Checked::Amount { min, max, mark } => CaptureKind::Amount {
                min: *min,
                max: *max,
                decimal_mark: *mark,
            },
            Checked::Word(words) => CaptureKind::Word {
                options: words.iter().map(|word| word.as_str().to_owned()).collect(),
            },
        }
    }
}

/// A capture's kind, checked.
#[derive(Debug, Clone, PartialEq)]
enum Checked {
    Integer {
        min: i64,
        max: i64,
    },
    Amount {
        min: f64,
        max: f64,
        mark: DecimalMark,
    },
    Word(WordList),
}

/// A part of a pattern's text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    /// This text, exactly.
    Literal(String),
    /// The capture at this place in the list.
    Capture(usize),
}

/// A line of the server's own chat that a scripted mode declares it reads
/// (ADR-0037, C3).
///
/// A pattern is literal text with up to [`ChatPattern::MAX_CAPTURES`] typed
/// captures. What that guarantees, whatever a server or a player prints:
/// - **The whole line must fit.** [`matches`](Self::matches) is anchored at
///   both ends and compares the literal text exactly, case included. A line
///   that holds the pattern's text among other text doesn't fit.
/// - **Typed captures only:** a whole number, an amount, or one of a closed
///   list of words. There's no capture for free text and no wildcard, so no
///   text from chat reaches a script.
/// - **Bounded:** a number outside its declared range is no match.
/// - **One reading, in linear time:** each capture reads the longest text
///   its grammar allows and is never tried again shorter, and two captures
///   are never side by side.
/// - **Never "any number" alone:** the text outside the captures holds at
///   least [`ChatPattern::MIN_LITERAL`] characters that aren't whitespace.
///
/// Whether a line may be put to a pattern at all is [`candidate_line`]'s to
/// say, and whether a pattern counts now is
/// [`ChatWatch`](super::ChatWatch)'s. It reads and writes the JSON of a
/// [`PatternDraft`], validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PatternDraft", into = "PatternDraft")]
pub struct ChatPattern {
    key: Ident,
    when: When,
    text: String,
    parts: Vec<Part>,
    captures: Vec<Capture>,
}

impl ChatPattern {
    /// The longest pattern, in characters as written: `{amount}` counts 8.
    pub const MAX_LEN: usize = 128;
    /// The most captures a pattern holds.
    pub const MAX_CAPTURES: usize = 4;
    /// The fewest characters that aren't whitespace a pattern's own text
    /// holds, outside its captures.
    pub const MIN_LITERAL: usize = 4;

    /// The key a script knows the pattern by.
    #[must_use]
    pub const fn key(&self) -> &Ident {
        &self.key
    }

    /// When it counts.
    #[must_use]
    pub const fn when(&self) -> When {
        self.when
    }

    /// The text as declared, trimmed, with its captures in braces.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The captures, in the order they were declared.
    #[must_use]
    pub fn captures(&self) -> &[Capture] {
        &self.captures
    }

    /// What the pattern reads from `line`, if the whole line fits: each
    /// capture's name and value, in the order they were declared. `line`'s
    /// edge whitespace doesn't count.
    #[must_use]
    pub fn matches(&self, line: &str) -> Option<Vec<(Ident, Captured)>> {
        let mut rest = line.trim();
        let mut read: Vec<Option<Captured>> = vec![None; self.captures.len()];
        for part in &self.parts {
            match part {
                Part::Literal(literal) => rest = rest.strip_prefix(literal.as_str())?,
                Part::Capture(capture) => {
                    let (value, len) = self.captures.get(*capture)?.kind.read(rest)?;
                    // What a capture reads is a prefix of `rest`, so `len`
                    // is on a character boundary.
                    rest = rest.get(len..)?;
                    *read.get_mut(*capture)? = Some(value);
                }
            }
        }
        if !rest.is_empty() {
            return None;
        }
        self.captures
            .iter()
            .zip(read)
            .map(|(capture, value)| Some((capture.name.clone(), value?)))
            .collect()
    }
}

impl Checked {
    /// Checks the kind of the capture at place `capture`.
    fn try_new(capture: usize, kind: CaptureKind) -> Result<Self, PatternError> {
        match kind {
            CaptureKind::Integer { min, max } => {
                if min > max {
                    return Err(PatternError::Range { capture });
                }
                Ok(Self::Integer { min, max })
            }
            CaptureKind::Amount {
                min,
                max,
                decimal_mark,
            } => {
                let reaches = |end: f64| end.abs() <= amount::MAX_AMOUNT;
                // A comparison with NaN is false, so it's refused here too.
                if !(min <= max && reaches(min) && reaches(max)) {
                    return Err(PatternError::Range { capture });
                }
                Ok(Self::Amount {
                    min,
                    max,
                    mark: decimal_mark,
                })
            }
            CaptureKind::Word { options } => WordList::try_new(&options)
                .map(Self::Word)
                .map_err(|reason| PatternError::Words { capture, reason }),
        }
    }

    /// Reads a value of this kind at the start of `text`: the longest its
    /// grammar allows, and how many bytes it took. `None` when there is
    /// none there, or it's outside its declared range.
    fn read(&self, text: &str) -> Option<(Captured, usize)> {
        match self {
            Self::Integer { min, max } => amount::read_integer(text)
                .filter(|read| (*min..=*max).contains(&read.value))
                .map(|read| (Captured::Integer(read.value), read.len)),
            Self::Amount { min, max, mark } => amount::read_amount(text, *mark)
                .filter(|read| (*min..=*max).contains(&read.value))
                .map(|read| (Captured::Amount(read.value), read.len)),
            Self::Word(words) => words
                .longest_prefix_of(text)
                .map(|word| (Captured::Word(word.clone()), word.as_str().len())),
        }
    }
}

/// Checks a declaration's captures, in order.
fn check_captures(drafts: Vec<CaptureDraft>) -> Result<Vec<Capture>, PatternError> {
    let count = drafts.len();
    if count > ChatPattern::MAX_CAPTURES {
        return Err(PatternError::TooManyCaptures { count });
    }
    let mut captures: Vec<Capture> = Vec::with_capacity(count);
    for (capture, draft) in drafts.into_iter().enumerate() {
        let name = Ident::try_from(draft.name)
            .map_err(|reason| PatternError::CaptureName { capture, reason })?;
        if captures.iter().any(|earlier| earlier.name == name) {
            return Err(PatternError::DuplicateCapture { capture });
        }
        let kind = Checked::try_new(capture, draft.kind)?;
        captures.push(Capture { name, kind });
    }
    Ok(captures)
}

impl TryFrom<PatternDraft> for ChatPattern {
    type Error = PatternError;

    /// Checks a declared pattern.
    ///
    /// The checks run in this order, the first problem winning: the key;
    /// the text (empty, its length, a forbidden character, a brace, two
    /// captures side by side, too little text of its own); the captures
    /// (how many, then each in order: its name, a name used before, its
    /// range or words); the text's names (one that isn't declared, one that
    /// comes a second time); a declared capture the text doesn't hold.
    fn try_from(draft: PatternDraft) -> Result<Self, PatternError> {
        let key = Ident::try_from(draft.key).map_err(PatternError::Key)?;
        let text = draft.text.trim();
        if text.is_empty() {
            return Err(PatternError::Empty);
        }
        let chars = text.chars().count();
        if chars > Self::MAX_LEN {
            return Err(PatternError::TooLong { chars });
        }
        if let Some(position) = text.chars().position(is_forbidden_in_declaration) {
            return Err(PatternError::ForbiddenChar { position });
        }
        let parsed = placeholder::parse(text).map_err(|brace| PatternError::Brace {
            position: brace.position,
        })?;
        let mut positions = parsed.positions.iter().copied();
        let mut after_capture = false;
        let mut own_text = 0_usize;
        for segment in &parsed.segments {
            match segment {
                Segment::Literal(literal) => {
                    after_capture = false;
                    own_text += literal.chars().filter(|c| !c.is_whitespace()).count();
                }
                Segment::Named(_) => {
                    // `parse` gives one position for each name.
                    let position = positions.next().unwrap_or_default();
                    if after_capture {
                        return Err(PatternError::AdjacentCaptures { position });
                    }
                    after_capture = true;
                }
            }
        }
        if own_text < Self::MIN_LITERAL {
            return Err(PatternError::TooLittleText { chars: own_text });
        }
        let captures = check_captures(draft.captures)?;
        let mut used = vec![false; captures.len()];
        let mut positions = parsed.positions.iter().copied();
        let mut parts = Vec::with_capacity(parsed.segments.len());
        for segment in parsed.segments {
            match segment {
                Segment::Literal(literal) => parts.push(Part::Literal(literal)),
                Segment::Named(name) => {
                    let position = positions.next().unwrap_or_default();
                    let capture = captures
                        .iter()
                        .position(|capture| capture.name == name)
                        .ok_or(PatternError::Undeclared { position })?;
                    match used.get_mut(capture) {
                        Some(used) if !*used => *used = true,
                        _ => return Err(PatternError::Repeated { position }),
                    }
                    parts.push(Part::Capture(capture));
                }
            }
        }
        if let Some(capture) = used.iter().position(|used| !used) {
            return Err(PatternError::Unused { capture });
        }
        Ok(Self {
            key,
            when: draft.when,
            text: text.to_owned(),
            parts,
            captures,
        })
    }
}

impl From<ChatPattern> for PatternDraft {
    fn from(pattern: ChatPattern) -> Self {
        Self {
            key: pattern.key.into(),
            when: pattern.when,
            text: pattern.text,
            captures: pattern
                .captures
                .iter()
                .map(|capture| CaptureDraft {
                    name: capture.name.as_str().to_owned(),
                    kind: capture.kind(),
                })
                .collect(),
        }
    }
}

/// Why patterns can't form [`ChatPatterns`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatPatternsError {
    /// There are more than [`ChatPatterns::MAX_PATTERNS`] patterns.
    #[error("the mode has {count} chat patterns; the limit is {max}", max = ChatPatterns::MAX_PATTERNS)]
    TooMany {
        /// How many there are.
        count: usize,
    },
    /// Two patterns have the same key.
    #[error("chat pattern {pattern} has the key of an earlier one")]
    DuplicateKey {
        /// The second one's place in the list, from 0.
        pattern: usize,
    },
}

/// A pattern's match: its key and what it read.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMatch {
    /// The pattern's key.
    pub key: Ident,
    /// Each capture's name and value, in the order they were declared.
    pub values: Vec<(Ident, Captured)>,
}

/// The chat patterns a scripted mode declares: at most
/// [`ChatPatterns::MAX_PATTERNS`], each with a key of its own, in the order
/// they were declared. The default is none, which reads no chat.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatPatterns(Vec<ChatPattern>);

impl ChatPatterns {
    /// The most patterns a mode declares.
    pub const MAX_PATTERNS: usize = 8;

    /// Builds the list from `patterns`, in order.
    ///
    /// # Errors
    /// [`ChatPatternsError::TooMany`] for more than
    /// [`ChatPatterns::MAX_PATTERNS`]; otherwise
    /// [`ChatPatternsError::DuplicateKey`] for the first pattern whose key
    /// an earlier one has.
    pub fn try_new(patterns: Vec<ChatPattern>) -> Result<Self, ChatPatternsError> {
        let count = patterns.len();
        if count > Self::MAX_PATTERNS {
            return Err(ChatPatternsError::TooMany { count });
        }
        let duplicate = patterns.iter().enumerate().position(|(at, pattern)| {
            patterns
                .iter()
                .take(at)
                .any(|earlier| earlier.key == pattern.key)
        });
        match duplicate {
            Some(pattern) => Err(ChatPatternsError::DuplicateKey { pattern }),
            None => Ok(Self(patterns)),
        }
    }

    /// The patterns, in order.
    pub fn iter(&self) -> impl Iterator<Item = &ChatPattern> {
        self.0.iter()
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The pattern with this key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ChatPattern> {
        self.0.iter().find(|pattern| pattern.key.as_str() == key)
    }

    /// How many patterns may count at any time: what the app marks as
    /// riskier, and a mode's audit entry records (ADR-0037, C5).
    #[must_use]
    pub fn any_time_count(&self) -> usize {
        self.0
            .iter()
            .filter(|pattern| pattern.when == When::AnyTime)
            .count()
    }

    /// The first pattern, in declared order, that `counts` now and that
    /// `line` fits, with what it read.
    #[must_use]
    pub fn first_match(
        &self,
        line: &str,
        mut counts: impl FnMut(&ChatPattern) -> bool,
    ) -> Option<ChatMatch> {
        self.0.iter().find_map(|pattern| {
            if !counts(pattern) {
                return None;
            }
            pattern.matches(line).map(|values| ChatMatch {
                key: pattern.key.clone(),
                values,
            })
        })
    }
}

/// The line of `chat` a pattern may be put to, if it's one at all
/// (ADR-0037, C2). All four must hold:
///
/// 1. **It's a system message.** A player's chat, a `/me`, a whisper and a
///    `/say` are player messages and are never read.
/// 2. **None of the server's chat formats fits it,** as received or
///    trimmed. A format names the lines a plugin prints for a player, a
///    private message included; such a line is a player's.
/// 3. **It's one line.** A message with a line break never counts, so no
///    part of a longer message passes as a line of its own.
/// 4. **It arrived whole,** not cut at the length fleet-core keeps.
///
/// The line comes back trimmed; an empty one is none.
#[must_use]
pub fn candidate_line<'a>(chat: &'a IncomingChat, formats: &ChatFormats) -> Option<&'a str> {
    if chat.kind() != ChatKind::System || chat.is_truncated() || chat.text().contains('\n') {
        return None;
    }
    let line = chat.text().trim();
    if line.is_empty() {
        return None;
    }
    // A format is anchored at a line's start, so a player's line behind
    // spaces is tried trimmed too.
    let a_players = formats.matched(chat).is_some()
        || (!formats.is_empty() && formats.matched(&IncomingChat::system(line)).is_some());
    (!a_players).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::PlayerChatKind;
    use crate::script::WordError;
    use proptest::prelude::*;
    use rstest::rstest;

    fn integer(name: &str, min: i64, max: i64) -> CaptureDraft {
        CaptureDraft {
            name: name.to_owned(),
            kind: CaptureKind::Integer { min, max },
        }
    }

    fn amount(name: &str, min: f64, max: f64) -> CaptureDraft {
        CaptureDraft {
            name: name.to_owned(),
            kind: CaptureKind::Amount {
                min,
                max,
                decimal_mark: DecimalMark::Point,
            },
        }
    }

    fn word(name: &str, options: &[&str]) -> CaptureDraft {
        CaptureDraft {
            name: name.to_owned(),
            kind: CaptureKind::Word {
                options: options.iter().map(|&option| option.to_owned()).collect(),
            },
        }
    }

    fn draft(text: &str, captures: Vec<CaptureDraft>) -> PatternDraft {
        PatternDraft {
            key: "key".to_owned(),
            when: When::Reply,
            text: text.to_owned(),
            captures,
        }
    }

    fn pattern(text: &str, captures: Vec<CaptureDraft>) -> ChatPattern {
        ChatPattern::try_from(draft(text, captures)).unwrap()
    }

    /// The reference's pattern (docs/scripting.md).
    fn balance() -> ChatPattern {
        pattern(
            "Balance: ${amount}",
            vec![amount("amount", 0.0, 1_000_000_000.0)],
        )
    }

    fn read(pattern: &ChatPattern, line: &str) -> Option<Vec<(String, Captured)>> {
        pattern.matches(line).map(|values| {
            values
                .into_iter()
                .map(|(name, value)| (name.as_str().to_owned(), value))
                .collect()
        })
    }

    fn a_word(text: &str) -> Captured {
        Captured::Word(Word::try_from(text).unwrap())
    }

    // --- Declaring ---

    #[test]
    fn a_pattern_keeps_what_was_declared() {
        let pattern = ChatPattern::try_from(PatternDraft {
            key: "status".to_owned(),
            when: When::AnyTime,
            text: "  Shop is {state}, {n} items for {price}  ".to_owned(),
            captures: vec![
                word("state", &["open", "closed"]),
                integer("n", 0, 64),
                amount("price", 0.0, 100.0),
            ],
        })
        .unwrap();

        assert_eq!(pattern.key().as_str(), "status");
        assert_eq!(pattern.when(), When::AnyTime);
        assert_eq!(pattern.text(), "Shop is {state}, {n} items for {price}");
        let names: Vec<&str> = pattern
            .captures()
            .iter()
            .map(|capture| capture.name().as_str())
            .collect();
        assert_eq!(names, ["state", "n", "price"]);
        assert_eq!(
            pattern.captures()[1].kind(),
            CaptureKind::Integer { min: 0, max: 64 }
        );
        assert_eq!(
            pattern.captures()[2].kind(),
            CaptureKind::Amount {
                min: 0.0,
                max: 100.0,
                decimal_mark: DecimalMark::Point
            }
        );
    }

    #[test]
    fn a_pattern_without_captures_is_a_fixed_line() {
        let pattern = pattern("You are now AFK.", Vec::new());

        assert_eq!(read(&pattern, "You are now AFK."), Some(Vec::new()));
        assert_eq!(read(&pattern, "You are now AFK"), None);
    }

    #[rstest]
    #[case::a_bad_key_first(
        PatternDraft { key: "9".to_owned(), when: When::Reply, text: String::new(), captures: Vec::new() },
        PatternError::Key(IdentError::InvalidChar { index: 0 })
    )]
    #[case::empty(draft("", Vec::new()), PatternError::Empty)]
    #[case::only_spaces(draft("  ", Vec::new()), PatternError::Empty)]
    #[case::too_long(draft(&"x".repeat(129), Vec::new()), PatternError::TooLong { chars: 129 })]
    #[case::a_line_break(draft("Balance:\n5 coins", Vec::new()), PatternError::ForbiddenChar { position: 8 })]
    #[case::a_section_sign(draft("\u{a7}aBalance", Vec::new()), PatternError::ForbiddenChar { position: 0 })]
    #[case::a_zero_width_space(draft("Bal\u{200b}ance", Vec::new()), PatternError::ForbiddenChar { position: 3 })]
    #[case::a_lone_brace(draft("Balance: {", Vec::new()), PatternError::Brace { position: 9 })]
    #[case::captures_side_by_side(
        draft("Paid {a}{b} coins", vec![integer("a", 0, 9), integer("b", 0, 9)]),
        PatternError::AdjacentCaptures { position: 8 }
    )]
    #[case::only_a_capture(
        draft("{amount}", vec![amount("amount", 0.0, 9.0)]),
        PatternError::TooLittleText { chars: 0 }
    )]
    #[case::three_characters_of_its_own(
        draft("{amount} $$$", vec![amount("amount", 0.0, 9.0)]),
        PatternError::TooLittleText { chars: 3 }
    )]
    #[case::spaces_dont_count(
        draft("a b  c {amount}", vec![amount("amount", 0.0, 9.0)]),
        PatternError::TooLittleText { chars: 3 }
    )]
    #[case::five_captures(
        draft(
            "Got {a} and {b} and {c} and {d} and {e}",
            ["a", "b", "c", "d", "e"].into_iter().map(|name| integer(name, 0, 9)).collect()
        ),
        PatternError::TooManyCaptures { count: 5 }
    )]
    #[case::a_captures_name(
        draft("Balance: {a}", vec![integer("a", 0, 9), integer("B", 0, 9)]),
        PatternError::CaptureName { capture: 1, reason: IdentError::InvalidChar { index: 0 } }
    )]
    #[case::a_name_twice(
        draft("Balance: {a}", vec![integer("a", 0, 9), integer("a", 0, 9)]),
        PatternError::DuplicateCapture { capture: 1 }
    )]
    #[case::an_empty_range(
        draft("Balance: {a}", vec![integer("a", 9, 0)]),
        PatternError::Range { capture: 0 }
    )]
    #[case::an_empty_amount_range(
        draft("Balance: {a}", vec![amount("a", 9.0, 0.0)]),
        PatternError::Range { capture: 0 }
    )]
    #[case::a_range_that_isnt_a_number(
        draft("Balance: {a}", vec![amount("a", 0.0, f64::NAN)]),
        PatternError::Range { capture: 0 }
    )]
    #[case::an_endless_range(
        draft("Balance: {a}", vec![amount("a", f64::NEG_INFINITY, 0.0)]),
        PatternError::Range { capture: 0 }
    )]
    #[case::a_range_past_the_largest(
        draft("Balance: {a}", vec![amount("a", 0.0, 1.5e15)]),
        PatternError::Range { capture: 0 }
    )]
    #[case::no_words(
        draft("Shop is {a}", vec![word("a", &[])]),
        PatternError::Words { capture: 0, reason: WordListError::Count { count: 0 } }
    )]
    #[case::a_word_with_a_space(
        draft("Shop is {a}", vec![word("a", &["sold out"])]),
        PatternError::Words {
            capture: 0,
            reason: WordListError::Word { option: 0, reason: WordError::InvalidChar { index: 4 } }
        }
    )]
    #[case::a_name_that_isnt_declared(
        draft("Balance: {a} of {b}", vec![integer("a", 0, 9)]),
        PatternError::Undeclared { position: 16 }
    )]
    #[case::a_name_twice_in_the_text(
        draft("Balance: {a} of {a}", vec![integer("a", 0, 9)]),
        PatternError::Repeated { position: 16 }
    )]
    #[case::a_capture_the_text_lacks(
        draft("Balance: {a}", vec![integer("a", 0, 9), integer("b", 0, 9)]),
        PatternError::Unused { capture: 1 }
    )]
    fn refuses_a_declaration_that_isnt_one(
        #[case] draft: PatternDraft,
        #[case] error: PatternError,
    ) {
        assert_eq!(ChatPattern::try_from(draft), Err(error));
    }

    #[rstest]
    #[case::the_length_before_a_character(
        draft(&format!("x\n{}", "x".repeat(128)), Vec::new()),
        PatternError::TooLong { chars: 130 }
    )]
    #[case::a_character_before_a_brace(
        draft("} \u{7} text", Vec::new()),
        PatternError::ForbiddenChar { position: 2 }
    )]
    #[case::a_brace_before_too_little_text(draft("ab {", Vec::new()), PatternError::Brace { position: 3 })]
    #[case::the_text_before_the_captures(
        draft("{a}", vec![integer("A", 9, 0)]),
        PatternError::TooLittleText { chars: 0 }
    )]
    #[case::the_count_before_any_capture(
        draft(
            "some text",
            ["A", "b", "c", "d", "e"].into_iter().map(|name| integer(name, 0, 9)).collect()
        ),
        PatternError::TooManyCaptures { count: 5 }
    )]
    #[case::a_captures_range_before_the_texts_names(
        draft("Balance: {b}", vec![integer("a", 9, 0)]),
        PatternError::Range { capture: 0 }
    )]
    #[case::an_undeclared_name_before_an_unused_capture(
        draft("Balance: {b}", vec![integer("a", 0, 9)]),
        PatternError::Undeclared { position: 9 }
    )]
    fn the_first_problem_wins(#[case] draft: PatternDraft, #[case] error: PatternError) {
        assert_eq!(ChatPattern::try_from(draft), Err(error));
    }

    #[test]
    fn a_pattern_of_128_characters_is_declared() {
        assert!(ChatPattern::try_from(draft(&"x".repeat(128), Vec::new())).is_ok());
    }

    #[test]
    fn four_characters_of_its_own_are_enough() {
        assert!(
            ChatPattern::try_from(draft("$$ {amount} $$", vec![amount("amount", 0.0, 9.0)]))
                .is_ok()
        );
    }

    // --- Matching ---

    #[rstest]
    #[case::plain("Balance: $1500", 1500.0)]
    #[case::with_separators_and_decimals("Balance: $1,234.56", 1234.56)]
    #[case::with_a_suffix("Balance: $12.5k", 12_500.0)]
    #[case::zero("Balance: $0", 0.0)]
    #[case::the_largest("Balance: $1B", 1_000_000_000.0)]
    #[case::edge_whitespace_doesnt_count("  Balance: $7  ", 7.0)]
    fn the_whole_line_fits_and_gives_its_amount(#[case] line: &str, #[case] value: f64) {
        assert_eq!(
            read(&balance(), line),
            Some(vec![("amount".to_owned(), Captured::Amount(value))])
        );
    }

    #[rstest]
    #[case::text_before("Your Balance: $5")]
    #[case::text_after("Balance: $5 (pending)")]
    #[case::a_sentences_point("Balance: $5.")]
    #[case::another_case("balance: $5")]
    #[case::a_missing_space("Balance:$5")]
    #[case::two_spaces("Balance:  $5")]
    #[case::no_number("Balance: $")]
    #[case::not_a_number("Balance: $lots")]
    #[case::a_number_with_a_tail("Balance: $5x")]
    #[case::a_malformed_number("Balance: $1,23")]
    #[case::below_the_range("Balance: $-5")]
    #[case::above_the_range("Balance: $1,000,000,001")]
    #[case::above_the_range_by_suffix("Balance: $2B")]
    #[case::the_pattern_itself("Balance: ${amount}")]
    #[case::nothing("")]
    #[case::twice("Balance: $5Balance: $5")]
    fn anything_but_the_whole_line_doesnt_fit(#[case] line: &str) {
        assert_eq!(read(&balance(), line), None);
    }

    /// The lines a player can make a server print around text of their own
    /// (ADR-0037, "Threats"): the reply's exact text is in each, and none
    /// fits, because the whole line must.
    #[rstest]
    #[case::plugin_chat("[Member] AfkBot2: Balance: $1,000,000")]
    #[case::vanilla_looking_chat("<AfkBot2> Balance: $1,000,000")]
    #[case::a_private_message("[AfkBot2 -> me] Balance: $1,000,000")]
    #[case::a_private_message_from("From AfkBot2: Balance: $1,000,000")]
    #[case::a_broadcast("[Broadcast] Balance: $1,000,000")]
    #[case::an_auction("AfkBot2 is selling [Balance: $1,000,000] for $5")]
    #[case::a_nickname_joining("Balance: $1,000,000 joined the game")]
    #[case::a_renamed_item("AfkBot3 was slain by AfkBot2 using [Balance: $1,000,000]")]
    #[case::an_emote("* AfkBot2 Balance: $1,000,000")]
    fn a_spoofed_reply_inside_a_longer_line_never_fits(#[case] line: &str) {
        assert_eq!(read(&balance(), line), None);
        // The reply itself, to show the pattern is the reference's.
        assert!(read(&balance(), "Balance: $1,000,000").is_some());
    }

    #[test]
    fn captures_come_back_in_declared_order_with_their_kinds() {
        let pattern = pattern(
            "Shop is {state}: {n} items, {price} each",
            vec![
                amount("price", 0.0, 100.0),
                integer("n", 0, 64),
                word("state", &["open", "closed"]),
            ],
        );

        assert_eq!(
            read(&pattern, "Shop is closed: 12 items, 2.5 each"),
            Some(vec![
                ("price".to_owned(), Captured::Amount(2.5)),
                ("n".to_owned(), Captured::Integer(12)),
                ("state".to_owned(), a_word("closed")),
            ])
        );
    }

    #[rstest]
    #[case::inside("You have 64 items", Some(64))]
    #[case::the_smallest("You have 0 items", Some(0))]
    #[case::above("You have 65 items", None)]
    #[case::negative("You have -1 items", None)]
    #[case::an_amount_isnt_a_whole_number("You have 1,000 items", None)]
    #[case::a_decimal_isnt_a_whole_number("You have 1.5 items", None)]
    #[case::a_suffix_isnt_a_whole_number("You have 1k items", None)]
    fn a_whole_number_counts_only_in_its_range(#[case] line: &str, #[case] value: Option<i64>) {
        let pattern = pattern("You have {n} items", vec![integer("n", 0, 64)]);

        assert_eq!(
            read(&pattern, line),
            value.map(|value| vec![("n".to_owned(), Captured::Integer(value))])
        );
    }

    #[rstest]
    #[case::one_of_them("Flight is on now", Some("on"))]
    #[case::the_longest_that_fits("Flight is online now", Some("online"))]
    #[case::another_case("Flight is ON now", None)]
    #[case::not_in_the_list("Flight is away now", None)]
    #[case::a_word_with_a_tail("Flight is only now", None)]
    fn a_word_is_one_of_the_list_the_longest_that_fits(
        #[case] line: &str,
        #[case] value: Option<&str>,
    ) {
        let pattern = pattern(
            "Flight is {state} now",
            vec![word("state", &["on", "off", "online"])],
        );

        assert_eq!(
            read(&pattern, line),
            value.map(|value| vec![("state".to_owned(), a_word(value))])
        );
    }

    /// A capture reads the longest text its grammar allows and isn't tried
    /// again shorter: `on` would let the rest fit, but `online` is read.
    #[test]
    fn a_capture_is_never_tried_again_shorter() {
        let pattern = pattern(
            "State {state}line ok",
            vec![word("state", &["on", "online"])],
        );

        assert_eq!(read(&pattern, "State online ok"), None);
        assert_eq!(
            read(&pattern, "State onlineline ok"),
            Some(vec![("state".to_owned(), a_word("online"))])
        );
    }

    #[test]
    fn the_declared_decimal_mark_reads_the_amount() {
        let pattern = pattern(
            "Kontostand: {betrag} Euro",
            vec![CaptureDraft {
                name: "betrag".to_owned(),
                kind: CaptureKind::Amount {
                    min: 0.0,
                    max: 1e9,
                    decimal_mark: DecimalMark::Comma,
                },
            }],
        );

        assert_eq!(
            read(&pattern, "Kontostand: 1.234,56 Euro"),
            Some(vec![("betrag".to_owned(), Captured::Amount(1234.56))])
        );
    }

    #[test]
    fn a_doubled_brace_is_a_literal_brace() {
        let pattern = pattern("{{Bank}} has {n}", vec![integer("n", 0, 9)]);

        assert_eq!(
            read(&pattern, "{Bank} has 5"),
            Some(vec![("n".to_owned(), Captured::Integer(5))])
        );
    }

    #[test]
    fn errors_name_a_place_and_never_the_text() {
        assert_eq!(
            PatternError::TooLittleText { chars: 3 }.to_string(),
            "the pattern has 3 characters of its own text; it needs 4"
        );
        assert_eq!(
            PatternError::AdjacentCaptures { position: 8 }.to_string(),
            "the pattern has no text before the capture at position 8"
        );
        assert_eq!(
            ChatPatternsError::DuplicateKey { pattern: 2 }.to_string(),
            "chat pattern 2 has the key of an earlier one"
        );
    }

    // --- JSON ---

    #[test]
    fn a_pattern_reads_and_writes_its_json() {
        let json = concat!(
            r#"{"key":"shop","when":"any_time","text":"Shop is {state}: {n} for {price}","captures":["#,
            r#"{"name":"state","type":"word","options":["open","closed"]},"#,
            r#"{"name":"n","type":"integer","min":0,"max":64},"#,
            r#"{"name":"price","type":"amount","min":0.0,"max":1000.0,"decimal_mark":","}]}"#
        );

        let pattern: ChatPattern = serde_json::from_str(json).unwrap();

        assert_eq!(pattern.when(), When::AnyTime);
        assert_eq!(serde_json::to_string(&pattern).unwrap(), json);
    }

    #[rstest]
    #[case::an_invalid_declaration(r#"{"key":"k","when":"reply","text":"{a}","captures":[]}"#)]
    #[case::no_when(r#"{"key":"k","text":"Balance","captures":[]}"#)]
    #[case::an_unknown_when(r#"{"key":"k","when":"always","text":"Balance","captures":[]}"#)]
    #[case::a_free_text_capture(
        r#"{"key":"k","when":"reply","text":"Balance: {a}","captures":[{"name":"a","type":"text"}]}"#
    )]
    #[case::an_any_capture(
        r#"{"key":"k","when":"reply","text":"Balance: {a}","captures":[{"name":"a","type":"any"}]}"#
    )]
    #[case::an_amount_without_its_mark(
        r#"{"key":"k","when":"reply","text":"Balance: {a}","captures":[{"name":"a","type":"amount","min":0,"max":1}]}"#
    )]
    fn refuses_json_that_isnt_a_pattern(#[case] json: &str) {
        assert!(serde_json::from_str::<ChatPattern>(json).is_err(), "{json}");
    }

    // --- The list ---

    fn keyed(key: &str, when: When, text: &str) -> ChatPattern {
        ChatPattern::try_from(PatternDraft {
            key: key.to_owned(),
            when,
            text: text.to_owned(),
            captures: vec![integer("n", 0, 99)],
        })
        .unwrap()
    }

    #[test]
    fn a_mode_declares_up_to_eight_patterns() {
        let patterns = |count: usize| {
            (0..count)
                .map(|n| keyed(&format!("p{n}"), When::Reply, "Line {n} here"))
                .collect::<Vec<_>>()
        };

        let eight = ChatPatterns::try_new(patterns(8)).unwrap();
        assert_eq!(eight.len(), 8);
        assert!(!eight.is_empty());
        assert_eq!(
            ChatPatterns::try_new(patterns(9)),
            Err(ChatPatternsError::TooMany { count: 9 })
        );
    }

    #[test]
    fn two_patterns_cant_share_a_key() {
        let patterns = vec![
            keyed("a", When::Reply, "First {n} here"),
            keyed("b", When::Reply, "Second {n} here"),
            keyed("a", When::AnyTime, "Third {n} here"),
        ];

        assert_eq!(
            ChatPatterns::try_new(patterns),
            Err(ChatPatternsError::DuplicateKey { pattern: 2 })
        );
    }

    #[test]
    fn no_patterns_read_nothing() {
        let none = ChatPatterns::default();

        assert!(none.is_empty());
        assert_eq!(none.any_time_count(), 0);
        assert_eq!(none.first_match("Balance: $5", |_| true), None);
        assert_eq!(ChatPatterns::try_new(Vec::new()), Ok(none));
    }

    #[test]
    fn the_list_finds_a_pattern_by_its_key_and_counts_the_any_time_ones() {
        let patterns = ChatPatterns::try_new(vec![
            keyed("a", When::Reply, "First {n} here"),
            keyed("b", When::AnyTime, "Second {n} here"),
            keyed("c", When::AnyTime, "Third {n} here"),
        ])
        .unwrap();

        assert_eq!(
            patterns.get("b").map(ChatPattern::text),
            Some("Second {n} here")
        );
        assert_eq!(patterns.get("z"), None);
        assert_eq!(patterns.any_time_count(), 2);
        let keys: Vec<&str> = patterns
            .iter()
            .map(|pattern| pattern.key().as_str())
            .collect();
        assert_eq!(keys, ["a", "b", "c"]);
    }

    #[test]
    fn the_first_pattern_that_counts_and_fits_wins() {
        let patterns = ChatPatterns::try_new(vec![
            keyed("first", When::Reply, "Line {n} here"),
            keyed("second", When::AnyTime, "Line {n} here"),
            keyed("other", When::AnyTime, "Other {n} here"),
        ])
        .unwrap();
        let key = |found: Option<ChatMatch>| found.map(|found| found.key.as_str().to_owned());

        assert_eq!(
            key(patterns.first_match("Line 5 here", |_| true)).as_deref(),
            Some("first")
        );
        assert_eq!(
            key(patterns.first_match("Line 5 here", |pattern| pattern.when() == When::AnyTime))
                .as_deref(),
            Some("second")
        );
        assert_eq!(key(patterns.first_match("Line 5 here", |_| false)), None);
        assert_eq!(key(patterns.first_match("Line x here", |_| true)), None);
        assert_eq!(
            patterns.first_match("Other 7 here", |_| true),
            Some(ChatMatch {
                key: Ident::try_from("other").unwrap(),
                values: vec![(Ident::try_from("n").unwrap(), Captured::Integer(7))],
            })
        );
    }

    // --- Which lines are candidates ---

    fn formats(entries: &[&str]) -> ChatFormats {
        ChatFormats::try_new(entries).unwrap()
    }

    #[test]
    fn a_servers_own_line_is_a_candidate_trimmed() {
        let chat = IncomingChat::system("  Balance: $5 ");

        assert_eq!(
            candidate_line(&chat, &ChatFormats::default()),
            Some("Balance: $5")
        );
    }

    #[rstest]
    #[case::chat(PlayerChatKind::Chat)]
    #[case::a_whisper(PlayerChatKind::Whisper)]
    #[case::an_emote(PlayerChatKind::Emote)]
    #[case::an_announcement(PlayerChatKind::Announcement)]
    fn a_players_message_is_never_a_candidate(#[case] kind: PlayerChatKind) {
        let chat = IncomingChat::player(kind, "AfkBot2", None, "Balance: $1,000,000");

        assert_eq!(candidate_line(&chat, &ChatFormats::default()), None);
    }

    #[rstest]
    #[case::plugin_chat("[Member] AfkBot2 : Balance: $1,000,000")]
    #[case::a_private_message("[AfkBot2 -> me] Balance: $1,000,000")]
    #[case::with_spaces_before_it("   [AfkBot2 -> me] Balance: $1,000,000")]
    fn a_line_a_chat_format_fits_is_never_a_candidate(#[case] line: &str) {
        let formats = formats(&["[{any}] {sender} : ", "[{sender} -> me] "]);
        let chat = IncomingChat::system(line);

        assert_eq!(candidate_line(&chat, &formats), None);
        // Without the formats it's a line like any other, which the whole
        // pattern must then fit.
        assert_eq!(
            candidate_line(&chat, &ChatFormats::default()),
            Some(line.trim())
        );
    }

    #[rstest]
    #[case::two_lines("Balance: $5\nBalance: $1,000,000")]
    #[case::the_reply_as_a_second_line("AfkBot2 says:\nBalance: $1,000,000")]
    #[case::a_trailing_line_break_inside("Balance: $5\n ")]
    fn a_message_with_a_line_break_is_never_a_candidate(#[case] text: &str) {
        let chat = IncomingChat::system(text);

        assert_eq!(candidate_line(&chat, &ChatFormats::default()), None);
    }

    #[test]
    fn a_cut_message_is_never_a_candidate() {
        let long = format!("Balance: ${}", "9".repeat(IncomingChat::MAX_TEXT_LEN));
        let chat = IncomingChat::system(&long);

        assert!(chat.is_truncated());
        assert_eq!(candidate_line(&chat, &ChatFormats::default()), None);
    }

    #[rstest]
    #[case::nothing("")]
    #[case::only_spaces("   ")]
    #[case::only_formatting("\u{a7}a\u{a7}l")]
    fn an_empty_message_is_no_candidate(#[case] text: &str) {
        let chat = IncomingChat::system(text);

        assert_eq!(candidate_line(&chat, &ChatFormats::default()), None);
    }

    // --- Properties ---

    /// A pattern over a small alphabet, with what it was made from.
    fn any_draft() -> impl Strategy<Value = PatternDraft> {
        let kind = prop_oneof![
            (-50_i64..50, 0_i64..100).prop_map(|(min, more)| CaptureKind::Integer {
                min,
                max: min + more
            }),
            (-50_i32..50, 0_i32..2000, any::<bool>()).prop_map(|(min, more, comma)| {
                CaptureKind::Amount {
                    min: f64::from(min),
                    max: f64::from(min + more),
                    decimal_mark: if comma {
                        DecimalMark::Comma
                    } else {
                        DecimalMark::Point
                    },
                }
            }),
            proptest::collection::vec("[a-c]{1,3}", 1..4)
                .prop_map(|options| CaptureKind::Word { options }),
        ];
        (
            "[a-c $:{}]{0,24}",
            proptest::collection::vec(("[a-c]", kind), 0..3),
            any::<bool>(),
        )
            .prop_map(|(text, captures, any_time)| PatternDraft {
                key: "key".to_owned(),
                when: if any_time { When::AnyTime } else { When::Reply },
                text,
                captures: captures
                    .into_iter()
                    .map(|(name, kind)| CaptureDraft { name, kind })
                    .collect(),
            })
    }

    /// A line the pattern fits: its own text around a value each capture
    /// reads, taken from `seed`.
    fn fitting_line(pattern: &ChatPattern, seed: u32) -> String {
        let mut line = String::new();
        for part in &pattern.parts {
            match part {
                Part::Literal(literal) => line.push_str(literal),
                Part::Capture(capture) => match &pattern.captures[*capture].kind {
                    Checked::Integer { min, max } => {
                        let span = u32::try_from(max - min).unwrap() + 1;
                        line.push_str(&(min + i64::from(seed % span)).to_string());
                    }
                    Checked::Amount { min, max, mark } => {
                        let span = max - min;
                        let value = (min + span * f64::from(seed % 101) / 100.0).round();
                        line.push_str(&format!("{value}").replace('.', &mark.mark().to_string()));
                    }
                    Checked::Word(words) => {
                        let count = u32::try_from(words.iter().count()).unwrap();
                        let word = words
                            .iter()
                            .nth(usize::try_from(seed % count).unwrap())
                            .unwrap();
                        line.push_str(word.as_str());
                    }
                },
            }
        }
        line
    }

    proptest! {
        #[test]
        fn declaring_and_matching_never_panic(draft in any_draft(), line in any::<String>()) {
            if let Ok(pattern) = ChatPattern::try_from(draft) {
                let _ = pattern.matches(&line);
                let again = ChatPattern::try_from(PatternDraft::from(pattern.clone()));
                prop_assert_eq!(again.as_ref(), Ok(&pattern));
            }
        }

        /// Whatever fits: every value is inside its declared range or list.
        #[test]
        fn a_match_gives_values_inside_what_was_declared(
            draft in any_draft(),
            line in "[a-c $:.,0-9kMB-]{0,24}",
            seed in any::<u32>(),
        ) {
            if let Ok(pattern) = ChatPattern::try_from(draft) {
                for line in [line, fitting_line(&pattern, seed)] {
                    let Some(values) = pattern.matches(&line) else { continue };
                    prop_assert_eq!(values.len(), pattern.captures().len());
                    for ((name, value), capture) in values.iter().zip(pattern.captures()) {
                        prop_assert_eq!(name, capture.name());
                        match (value, capture.kind()) {
                            (Captured::Integer(n), CaptureKind::Integer { min, max }) => {
                                prop_assert!((min..=max).contains(n));
                            }
                            (Captured::Amount(n), CaptureKind::Amount { min, max, .. }) => {
                                prop_assert!((min..=max).contains(n));
                            }
                            (Captured::Word(word), CaptureKind::Word { options }) => {
                                prop_assert!(options.iter().any(|option| option == word.as_str()));
                            }
                            other => prop_assert!(false, "{other:?}"),
                        }
                    }
                }
            }
        }

        /// A line made of the pattern's own text and whole numbers in
        /// their ranges fits. (A word or an amount can read on into the
        /// text after it, so such a line needn't.)
        #[test]
        fn a_line_of_the_patterns_text_and_whole_numbers_fits(
            draft in any_draft(),
            seed in any::<u32>(),
        ) {
            if let Ok(pattern) = ChatPattern::try_from(draft) {
                let only_whole_numbers = pattern
                    .captures()
                    .iter()
                    .all(|capture| matches!(capture.kind(), CaptureKind::Integer { .. }));
                if only_whole_numbers {
                    prop_assert!(pattern.matches(&fitting_line(&pattern, seed)).is_some());
                }
            }
        }

        /// With a player's name before the line or text after it, in
        /// characters no pattern here holds, it never fits.
        #[test]
        fn a_line_with_anything_around_it_never_fits(
            draft in any_draft(),
            seed in any::<u32>(),
            before in proptest::sample::select(vec!["<Zed> ", "[Zed -> me] ", "Zed: ", "* Zed "]),
            after in proptest::sample::select(vec![" (Zed)", " !", " joined the game"]),
        ) {
            if let Ok(pattern) = ChatPattern::try_from(draft) {
                let line = fitting_line(&pattern, seed);
                let lines = [
                    [before, line.as_str(), ""].concat(),
                    ["", line.as_str(), after].concat(),
                    [before, line.as_str(), after].concat(),
                ];
                for line in lines {
                    prop_assert_eq!(pattern.matches(&line), None);
                }
            }
        }

        /// No player message is a candidate, whatever its text; and a
        /// system line a format names a player's isn't one either.
        #[test]
        fn no_players_line_is_ever_a_candidate(
            text in "[ -~\n]{0,40}",
            name in "[A-Za-z0-9_]{3,16}",
            kind in proptest::sample::select(vec![
                PlayerChatKind::Chat,
                PlayerChatKind::Emote,
                PlayerChatKind::Whisper,
                PlayerChatKind::Announcement,
            ]),
        ) {
            let formats = formats(&["<{sender}> ", "[{sender} -> me] "]);

            let chat = IncomingChat::player(kind, &name, None, &text);
            prop_assert_eq!(candidate_line(&chat, &formats), None);

            for prefix in [format!("<{name}> "), format!("[{name} -> me] ")] {
                let chat = IncomingChat::system(&format!("{prefix}{text}"));
                prop_assert_eq!(candidate_line(&chat, &formats), None);
            }

            // What is a candidate is one whole, trimmed line.
            let chat = IncomingChat::system(&text);
            if let Some(line) = candidate_line(&chat, &formats) {
                prop_assert!(!line.contains('\n'));
                prop_assert_eq!(line, line.trim());
                prop_assert!(!line.is_empty());
            }
        }
    }
}
