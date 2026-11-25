//! [`MessageTemplate`]: a message a scripted mode declares, the only text
//! its script can send (ADR-0036, D8).

use serde::{Deserialize, Serialize};

use super::menu::SignLineError;
use super::placeholder::{self, Parsed, Segment};
use super::word::{WordList, WordListError, is_forbidden_in_declaration};
use super::{Ident, IdentError};
use crate::chat::{ChatMessage, ChatMessageError};
use crate::text;

/// What a declared message's value may be, as written in a mode's JSON:
/// tagged with its `"type"`, e.g. `{"type":"integer","min":1,"max":64}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValueKind {
    /// A whole number from `min` to `max`.
    Integer {
        /// The smallest value.
        min: i64,
        /// The largest value.
        max: i64,
    },
    /// A decimal number from `min` to `max`, sent with exactly `places`
    /// digits after the point.
    Decimal {
        /// The smallest value.
        min: f64,
        /// The largest value.
        max: f64,
        /// How many digits follow the point, 1 to
        /// [`MessageTemplate::MAX_PLACES`].
        places: u8,
    },
    /// One of a closed list of words.
    Word {
        /// The words, as [`WordList`] takes them.
        options: Vec<String>,
    },
}

/// A declared message's value as written, before
/// [`MessageTemplate::try_from`] has checked it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValueDraft {
    /// The value's name, as the text writes it in braces.
    pub name: String,
    /// What it may be.
    #[serde(flatten)]
    pub kind: ValueKind,
}

/// A declared message as written, before [`MessageTemplate::try_from`] has
/// checked it. Its JSON is `{"key":…,"text":…,"values":[…]}`; every field
/// is required, and unknown fields are ignored (ADR-0010).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDraft {
    /// The key a script names the message by.
    pub key: String,
    /// The text, with a `{name}` where each value goes.
    pub text: String,
    /// The values the text holds.
    pub values: Vec<ValueDraft>,
}

/// Why a [`MessageDraft`] isn't a message a mode may declare.
///
/// A `position` is a character's place in the trimmed text, counted in
/// characters; `value` is a value's place in the list, from 0. The text is
/// never included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The key isn't an [`Ident`].
    #[error("the message's key: {0}")]
    Key(IdentError),
    /// The text is empty or only whitespace.
    #[error("the message is empty")]
    Empty,
    /// The text holds a control character (a line break included), `§`, a
    /// text-direction mark or an invisible character.
    #[error("the message holds a character it may not, at position {position}")]
    ForbiddenChar {
        /// The character's place.
        position: usize,
    },
    /// The text holds a brace that isn't part of a `{name}`, `{{` or `}}`.
    #[error(
        "the message holds a brace that isn't part of a value or doubled, at position {position}"
    )]
    Brace {
        /// The brace's place.
        position: usize,
    },
    /// A command's name holds a brace: the name is always written out.
    #[error("the command's name holds a brace, at position {position}")]
    BraceInCommandName {
        /// The brace's place.
        position: usize,
    },
    /// The text starts with a value, which could turn it into a command.
    #[error("the message starts with a value")]
    StartsWithValue,
    /// There are more than [`MessageTemplate::MAX_VALUES`] values.
    #[error("the message has {count} values; the limit is {max}", max = MessageTemplate::MAX_VALUES)]
    TooManyValues {
        /// How many it has.
        count: usize,
    },
    /// A value's name isn't an [`Ident`].
    #[error("value {value}'s name: {reason}")]
    ValueName {
        /// The value's place.
        value: usize,
        /// Why it isn't a name.
        reason: IdentError,
    },
    /// Two values have the same name.
    #[error("value {value} has the name of an earlier one")]
    DuplicateValue {
        /// The second one's place.
        value: usize,
    },
    /// A whole number's or a decimal number's range is empty, isn't a
    /// number, or reaches past [`MessageTemplate::MAX_DECIMAL`].
    #[error("value {value}'s range isn't one")]
    Range {
        /// The value's place.
        value: usize,
    },
    /// A decimal number's places aren't 1 to
    /// [`MessageTemplate::MAX_PLACES`], or an end of its range is finer
    /// than they are.
    #[error("value {value}'s decimal places don't fit its range")]
    Places {
        /// The value's place.
        value: usize,
    },
    /// A value's words aren't a [`WordList`].
    #[error("value {value}'s words: {reason}")]
    Words {
        /// The value's place.
        value: usize,
        /// Why they aren't a list.
        reason: WordListError,
    },
    /// The text names a value that isn't declared.
    #[error("the message names a value it doesn't declare, at position {position}")]
    Undeclared {
        /// The `{`'s place.
        position: usize,
    },
    /// The text names a value a second time.
    #[error("the message names a value a second time, at position {position}")]
    Repeated {
        /// The second `{`'s place.
        position: usize,
    },
    /// A declared value isn't in the text.
    #[error("value {value} isn't in the message")]
    Unused {
        /// The value's place.
        value: usize,
    },
    /// The longest text the message can become is longer than
    /// [`ChatMessage::MAX_LEN`] UTF-16 code units.
    #[error("the message can be {units} UTF-16 code units long; the limit is {max}", max = ChatMessage::MAX_LEN)]
    TooLong {
        /// Its longest length in UTF-16 code units.
        units: usize,
    },
}

/// A value a script gives for a declared message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Given<'a> {
    /// A whole number. It also fills a decimal value.
    Integer(i64),
    /// A decimal number.
    Decimal(f64),
    /// Text, which must be one of the value's words.
    Text(&'a str),
}

/// Why values don't make a message of a [`MessageTemplate`], or a sign's
/// lines of a [`SignTemplate`](super::menu::SignTemplate).
///
/// `value` is a declared value's place, from 0. Neither a given name nor a
/// given text is ever included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// A declared value wasn't given.
    #[error("value {value} is missing")]
    Missing {
        /// The value's place.
        value: usize,
    },
    /// A given name isn't a declared value's, or was given twice.
    #[error("a value was given that the message doesn't declare, or given twice")]
    Undeclared,
    /// A value was given as another kind than declared: text for a number,
    /// a number for a word, a decimal number for a whole one.
    #[error("value {value} isn't of its declared kind")]
    WrongKind {
        /// The value's place.
        value: usize,
    },
    /// A number is outside its declared range, or isn't a number.
    #[error("value {value} is outside its range")]
    OutOfRange {
        /// The value's place.
        value: usize,
    },
    /// A text isn't one of the value's words.
    #[error("value {value} isn't one of its words")]
    NotAWord {
        /// The value's place.
        value: usize,
    },
    /// The finished text isn't a message a bot may send. A declaration's
    /// checks rule that out; this second check is the net behind them
    /// (ADR-0036, D8).
    #[error("the finished text isn't a message: {0}")]
    Message(ChatMessageError),
    /// A sign's finished line isn't one a sign takes: the net behind a
    /// sign's declaration, as [`RenderError::Message`] is a message's
    /// (ADR-0036, M16).
    #[error("line {line} of the sign: {reason}")]
    SignLine {
        /// The line's place, from 0.
        line: usize,
        /// Why it isn't a sign's line.
        reason: SignLineError,
    },
}

/// One value of a [`MessageTemplate`].
#[derive(Debug, Clone, PartialEq)]
pub struct MessageValue {
    name: Ident,
    kind: Checked,
}

impl MessageValue {
    /// The value's name.
    #[must_use]
    pub const fn name(&self) -> &Ident {
        &self.name
    }

    /// What it may be, as declared.
    #[must_use]
    pub fn kind(&self) -> ValueKind {
        match &self.kind {
            Checked::Integer { min, max } => ValueKind::Integer {
                min: *min,
                max: *max,
            },
            Checked::Decimal { min, max, places } => ValueKind::Decimal {
                min: *min,
                max: *max,
                places: *places,
            },
            Checked::Word(words) => ValueKind::Word {
                options: words.iter().map(|word| word.as_str().to_owned()).collect(),
            },
        }
    }
}

/// A value's kind, checked.
#[derive(Debug, Clone, PartialEq)]
enum Checked {
    Integer { min: i64, max: i64 },
    Decimal { min: f64, max: f64, places: u8 },
    Word(WordList),
}

/// A part of a template's text: a declared message's, or a line of a
/// declared sign's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::script) enum Part {
    /// This text, exactly.
    Literal(String),
    /// The value at this place in the list.
    Value(usize),
}

/// A message a scripted mode declares: fixed text, or text with typed
/// values (ADR-0036, D8).
///
/// A script sends a message by its key and gives its values; it can build
/// no text of its own. What that guarantees, whatever a script does and
/// whatever it read:
/// - **The words are the declaration's.** A value is a whole number in a
///   range, a decimal number in a range with a fixed number of places, or
///   one of a closed list of [`Word`](super::Word)s.
/// - **A command's name is written out,** never a value, and a message that
///   isn't a command can't become one: the text never starts with a value.
///   So [`command_name`](Self::command_name) is what the allowlist judges,
///   as for a step mode's chat.
/// - **It fits a chat message** with every value at its longest, checked
///   when it's declared; and [`render`](Self::render) puts the finished
///   text through [`ChatMessage`]'s own validation again.
///
/// The text is trimmed and holds no line break, no `§`, no text-direction
/// mark and no invisible character, so a declaration reads as it's shown.
/// It reads and writes the JSON of a [`MessageDraft`], validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "MessageDraft", into = "MessageDraft")]
pub struct MessageTemplate {
    key: Ident,
    text: String,
    parts: Vec<Part>,
    values: Vec<MessageValue>,
}

impl MessageTemplate {
    /// The most values a message holds.
    pub const MAX_VALUES: usize = 8;
    /// The most digits after a decimal number's point.
    pub const MAX_PLACES: u8 = 4;
    /// The largest end a decimal number's range may have, in either
    /// direction: 10^11, which with four places is still exact in an `f64`.
    pub const MAX_DECIMAL: f64 = 100_000_000_000.0;

    /// The key a script names the message by.
    #[must_use]
    pub const fn key(&self) -> &Ident {
        &self.key
    }

    /// The text as declared, trimmed, with its values in braces.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The values, in the order they were declared.
    #[must_use]
    pub fn values(&self) -> &[MessageValue] {
        &self.values
    }

    /// Whether the message is a command, i.e. starts with `/`.
    #[must_use]
    pub fn is_command(&self) -> bool {
        self.text.starts_with('/')
    }

    /// The command's name for allowlist checks, as
    /// [`ChatMessage::command_name`] gives it for a finished message; `None`
    /// if the message isn't a command. No value changes it.
    #[must_use]
    pub fn command_name(&self) -> Option<&str> {
        // The name holds no brace (`BraceInCommandName`), so it's the same
        // as written and as sent.
        let command = self.text.strip_prefix('/')?;
        Some(
            command
                .split_once(char::is_whitespace)
                .map_or(command, |(name, _)| name),
        )
    }

    /// The longest the finished text can be, in UTF-16 code units.
    #[must_use]
    pub fn max_len(&self) -> usize {
        longest(&self.parts, &self.values)
    }

    /// Makes the message from `given`, each a declared value's name and
    /// what the script gave for it, in any order.
    ///
    /// A whole number is written in digits. A decimal number is rounded to
    /// its places and written with exactly that many; it's the number as
    /// written that must be in the range, so nothing outside it is ever
    /// sent. A zero is never written with a minus.
    ///
    /// # Errors
    /// The first, in this order: [`RenderError::Undeclared`] for a given
    /// name that isn't a declared value's or comes twice; then, for each
    /// declared value in order, [`RenderError::Missing`],
    /// [`RenderError::WrongKind`], [`RenderError::OutOfRange`] or
    /// [`RenderError::NotAWord`]; then [`RenderError::Message`].
    pub fn render(&self, given: &[(&str, Given<'_>)]) -> Result<ChatMessage, RenderError> {
        let written = write_values(&self.values, given)?;
        ChatMessage::try_from(assemble(&self.parts, &written)).map_err(RenderError::Message)
    }
}

/// Writes each of `values` from what a script gave, in their order: a
/// render's checks of what was given, the same for every template.
///
/// # Errors
/// As [`MessageTemplate::render`] names them, but for the finished text.
pub(in crate::script) fn write_values(
    values: &[MessageValue],
    given: &[(&str, Given<'_>)],
) -> Result<Vec<String>, RenderError> {
    let declared = |name: &str| values.iter().any(|value| value.name.as_str() == name);
    let twice = |at: usize, name: &str| given.iter().take(at).any(|(earlier, _)| *earlier == name);
    if given
        .iter()
        .enumerate()
        .any(|(at, (name, _))| !declared(name) || twice(at, name))
    {
        return Err(RenderError::Undeclared);
    }
    values
        .iter()
        .enumerate()
        .map(|(value, declared)| {
            let given = given
                .iter()
                .find(|(name, _)| *name == declared.name.as_str())
                .map(|(_, given)| *given)
                .ok_or(RenderError::Missing { value })?;
            declared.kind.write(value, given)
        })
        .collect()
}

/// The text `parts` make with each value as `written` writes it.
pub(in crate::script) fn assemble(parts: &[Part], written: &[String]) -> String {
    let mut text = String::new();
    for part in parts {
        match part {
            Part::Literal(literal) => text.push_str(literal),
            // Every part's place is one of the values': a template builds
            // its parts and its values together.
            Part::Value(value) => {
                text.push_str(written.get(*value).map_or("", String::as_str));
            }
        }
    }
    text
}

/// Why a text's names don't fit the values declared with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::script) enum Unresolved {
    /// A name that isn't declared, with its `{`'s position.
    Undeclared(usize),
    /// A name used a second time, with its `{`'s position.
    Repeated(usize),
}

/// Turns a parsed text into parts, each name the place of a value of
/// `values`. `used` marks the values used so far, over every text of one
/// declaration: a value is used once in all of them.
pub(in crate::script) fn resolve(
    parsed: Parsed,
    values: &[MessageValue],
    used: &mut [bool],
) -> Result<Vec<Part>, Unresolved> {
    let mut positions = parsed.positions.iter().copied();
    let mut parts = Vec::with_capacity(parsed.segments.len());
    for segment in parsed.segments {
        match segment {
            Segment::Literal(literal) => parts.push(Part::Literal(literal)),
            Segment::Named(name) => {
                // `parse` gives one position for each name.
                let position = positions.next().unwrap_or_default();
                let value = values
                    .iter()
                    .position(|value| value.name == name)
                    .ok_or(Unresolved::Undeclared(position))?;
                match used.get_mut(value) {
                    Some(used) if !*used => *used = true,
                    _ => return Err(Unresolved::Repeated(position)),
                }
                parts.push(Part::Value(value));
            }
        }
    }
    Ok(parts)
}

impl TryFrom<MessageDraft> for MessageTemplate {
    type Error = TemplateError;

    /// Checks a declared message.
    ///
    /// The checks run in this order, the first problem winning: the key;
    /// the text (empty, a forbidden character, a brace, a brace in a
    /// command's name, a value at the start); the values (how many, then
    /// each in order: its name, a name used before, its range, places or
    /// words); the text's names (one that isn't declared, one that comes a
    /// second time); a declared value the text doesn't hold; the longest
    /// finished text.
    fn try_from(draft: MessageDraft) -> Result<Self, TemplateError> {
        let key = Ident::try_from(draft.key).map_err(TemplateError::Key)?;
        let text = draft.text.trim();
        if text.is_empty() {
            return Err(TemplateError::Empty);
        }
        if let Some(position) = text.chars().position(is_forbidden_in_declaration) {
            return Err(TemplateError::ForbiddenChar { position });
        }
        let parsed = placeholder::parse(text).map_err(|brace| TemplateError::Brace {
            position: brace.position,
        })?;
        if text.starts_with('/')
            && let Some(position) = text
                .chars()
                .take_while(|c| !c.is_whitespace())
                .position(|c| matches!(c, '{' | '}'))
        {
            return Err(TemplateError::BraceInCommandName { position });
        }
        if matches!(parsed.segments.first(), Some(Segment::Named(_))) {
            return Err(TemplateError::StartsWithValue);
        }
        let values = check_values(draft.values)?;
        let mut used = vec![false; values.len()];
        let parts = resolve(parsed, &values, &mut used).map_err(|unresolved| match unresolved {
            Unresolved::Undeclared(position) => TemplateError::Undeclared { position },
            Unresolved::Repeated(position) => TemplateError::Repeated { position },
        })?;
        if let Some(value) = used.iter().position(|used| !used) {
            return Err(TemplateError::Unused { value });
        }
        let units = longest(&parts, &values);
        if units > ChatMessage::MAX_LEN {
            return Err(TemplateError::TooLong { units });
        }
        Ok(Self {
            key,
            text: text.to_owned(),
            parts,
            values,
        })
    }
}

/// Checks a declaration's values, in order.
pub(in crate::script) fn check_values(
    drafts: Vec<ValueDraft>,
) -> Result<Vec<MessageValue>, TemplateError> {
    let count = drafts.len();
    if count > MessageTemplate::MAX_VALUES {
        return Err(TemplateError::TooManyValues { count });
    }
    let mut values: Vec<MessageValue> = Vec::with_capacity(count);
    for (value, draft) in drafts.into_iter().enumerate() {
        let name = Ident::try_from(draft.name)
            .map_err(|reason| TemplateError::ValueName { value, reason })?;
        if values.iter().any(|earlier| earlier.name == name) {
            return Err(TemplateError::DuplicateValue { value });
        }
        let kind = Checked::try_new(value, draft.kind)?;
        values.push(MessageValue { name, kind });
    }
    Ok(values)
}

/// The longest the parts can become, in UTF-16 code units.
pub(in crate::script) fn longest(parts: &[Part], values: &[MessageValue]) -> usize {
    parts
        .iter()
        .map(|part| match part {
            Part::Literal(literal) => text::utf16_len(literal),
            Part::Value(value) => values.get(*value).map_or(0, |value| value.kind.longest()),
        })
        .fold(0, usize::saturating_add)
}

/// `number` with exactly `places` digits after the point, rounded to the
/// nearest, and never a zero with a minus.
fn written_decimal(number: f64, places: u8) -> String {
    let written = format!("{number:.places$}", places = usize::from(places));
    match written.strip_prefix('-') {
        Some(digits) if digits.chars().all(|c| matches!(c, '0' | '.')) => digits.to_owned(),
        _ => written,
    }
}

/// `written` if the number it reads as is in `min..=max`: the range is
/// judged on what's sent, after rounding.
fn in_range(written: String, min: f64, max: f64) -> Option<String> {
    let number: f64 = written.parse().ok()?;
    (min..=max).contains(&number).then_some(written)
}

impl Checked {
    /// Checks the kind of the value at place `value`.
    fn try_new(value: usize, kind: ValueKind) -> Result<Self, TemplateError> {
        match kind {
            ValueKind::Integer { min, max } => {
                if min > max {
                    return Err(TemplateError::Range { value });
                }
                Ok(Self::Integer { min, max })
            }
            ValueKind::Decimal { min, max, places } => {
                let reaches = |end: f64| end.abs() <= MessageTemplate::MAX_DECIMAL;
                // A comparison with NaN is false, so it's refused here too.
                if !(min <= max && reaches(min) && reaches(max)) {
                    return Err(TemplateError::Range { value });
                }
                let exact = |end: f64| written_decimal(end, places).parse() == Ok(end);
                if !((1..=MessageTemplate::MAX_PLACES).contains(&places)
                    && exact(min)
                    && exact(max))
                {
                    return Err(TemplateError::Places { value });
                }
                Ok(Self::Decimal { min, max, places })
            }
            ValueKind::Word { options } => WordList::try_new(&options)
                .map(Self::Word)
                .map_err(|reason| TemplateError::Words { value, reason }),
        }
    }

    /// The longest text a value of this kind is written as, in UTF-16 code
    /// units. A number's longest is at an end of its range.
    fn longest(&self) -> usize {
        match self {
            Self::Integer { min, max } => min.to_string().len().max(max.to_string().len()),
            Self::Decimal { min, max, places } => written_decimal(*min, *places)
                .len()
                .max(written_decimal(*max, *places).len()),
            Self::Word(words) => words.longest_units(),
        }
    }

    /// Writes `given` for the value at place `value`, or says why not.
    fn write(&self, value: usize, given: Given<'_>) -> Result<String, RenderError> {
        match (self, given) {
            (Self::Integer { min, max }, Given::Integer(number)) => (*min..=*max)
                .contains(&number)
                .then(|| number.to_string())
                .ok_or(RenderError::OutOfRange { value }),
            (Self::Decimal { min, max, places }, Given::Integer(number)) => {
                // Written without a conversion to a float, so every digit
                // is the number's own.
                let zeros = "0".repeat(usize::from(*places));
                in_range(format!("{number}.{zeros}"), *min, *max)
                    .ok_or(RenderError::OutOfRange { value })
            }
            (Self::Decimal { min, max, places }, Given::Decimal(number)) => {
                // Far outside, or not a number: refused before it's written,
                // so nothing long is ever formatted.
                if !(*min - 1.0..=*max + 1.0).contains(&number) {
                    return Err(RenderError::OutOfRange { value });
                }
                in_range(written_decimal(number, *places), *min, *max)
                    .ok_or(RenderError::OutOfRange { value })
            }
            (Self::Word(words), Given::Text(text)) => words
                .contains(text)
                .then(|| text.to_owned())
                .ok_or(RenderError::NotAWord { value }),
            (Self::Integer { .. }, Given::Decimal(_) | Given::Text(_))
            | (Self::Decimal { .. }, Given::Text(_))
            | (Self::Word(_), Given::Integer(_) | Given::Decimal(_)) => {
                Err(RenderError::WrongKind { value })
            }
        }
    }
}

impl From<MessageTemplate> for MessageDraft {
    fn from(template: MessageTemplate) -> Self {
        Self {
            key: template.key.into(),
            text: template.text,
            values: template
                .values
                .iter()
                .map(|value| ValueDraft {
                    name: value.name.as_str().to_owned(),
                    kind: value.kind(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::WordError;
    use proptest::prelude::*;
    use rstest::rstest;

    fn integer(name: &str, min: i64, max: i64) -> ValueDraft {
        ValueDraft {
            name: name.to_owned(),
            kind: ValueKind::Integer { min, max },
        }
    }

    fn decimal(name: &str, min: f64, max: f64, places: u8) -> ValueDraft {
        ValueDraft {
            name: name.to_owned(),
            kind: ValueKind::Decimal { min, max, places },
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

    fn draft(text: &str, values: Vec<ValueDraft>) -> MessageDraft {
        MessageDraft {
            key: "key".to_owned(),
            text: text.to_owned(),
            values,
        }
    }

    fn template(text: &str, values: Vec<ValueDraft>) -> MessageTemplate {
        MessageTemplate::try_from(draft(text, values)).unwrap()
    }

    fn refused(text: &str, values: Vec<ValueDraft>) -> TemplateError {
        MessageTemplate::try_from(draft(text, values)).unwrap_err()
    }

    /// The reference's two declarations (docs/scripting.md).
    fn sell() -> MessageTemplate {
        template("/ah sell {price}", vec![integer("price", 1, 1_000_000)])
    }

    fn pay() -> MessageTemplate {
        template(
            "/pay AfkMain {amount}",
            vec![decimal("amount", 1.0, 1_000_000.0, 2)],
        )
    }

    // --- Declaring ---

    #[test]
    fn a_fixed_message_is_its_text() {
        let home = template("  /home farm ", Vec::new());

        assert_eq!(home.key().as_str(), "key");
        assert_eq!(home.text(), "/home farm");
        assert_eq!(home.values(), []);
        assert_eq!(home.max_len(), 10);
        assert_eq!(home.render(&[]).unwrap().as_str(), "/home farm");
    }

    #[test]
    fn a_message_keeps_its_values_as_declared() {
        let message = template(
            "/warp {place} {x} {price}",
            vec![
                word("place", &["farm", "base"]),
                integer("x", -5, 5),
                decimal("price", 0.5, 10.0, 1),
            ],
        );

        let names: Vec<&str> = message
            .values()
            .iter()
            .map(|value| value.name().as_str())
            .collect();
        assert_eq!(names, ["place", "x", "price"]);
        assert_eq!(
            message.values()[0].kind(),
            ValueKind::Word {
                options: vec!["farm".to_owned(), "base".to_owned()]
            }
        );
        assert_eq!(
            message.values()[1].kind(),
            ValueKind::Integer { min: -5, max: 5 }
        );
        assert_eq!(
            message.values()[2].kind(),
            ValueKind::Decimal {
                min: 0.5,
                max: 10.0,
                places: 1
            }
        );
    }

    #[rstest]
    #[case::plain_chat("hello", false, None)]
    #[case::a_bare_command("/spawn", true, Some("spawn"))]
    #[case::a_command_with_text("/home farm", true, Some("home"))]
    #[case::namespaced("/minecraft:tp 1 2 3", true, Some("minecraft:tp"))]
    #[case::a_double_slash("//wand", true, Some("/wand"))]
    #[case::a_lone_slash("/", true, Some(""))]
    #[case::a_space_after_the_slash("/ spawn", true, Some(""))]
    #[case::a_slash_later("not /a command", false, None)]
    fn a_commands_name_is_read_as_a_chat_messages_is(
        #[case] text: &str,
        #[case] command: bool,
        #[case] name: Option<&str>,
    ) {
        let message = template(text, Vec::new());

        assert_eq!(message.is_command(), command);
        assert_eq!(message.command_name(), name);
        // The same answers as for the finished message.
        let sent = message.render(&[]).unwrap();
        assert_eq!(sent.is_command(), command);
        assert_eq!(sent.command_name(), name);
    }

    #[test]
    fn a_value_never_changes_a_commands_name() {
        let message = template(
            "/pay {to} {amount}",
            vec![word("to", &["AfkMain", "AfkBot2"]), integer("amount", 1, 9)],
        );

        let sent = message
            .render(&[
                ("to", Given::Text("AfkBot2")),
                ("amount", Given::Integer(3)),
            ])
            .unwrap();

        assert_eq!(message.command_name(), Some("pay"));
        assert_eq!(sent.command_name(), Some("pay"));
        assert_eq!(sent.as_str(), "/pay AfkBot2 3");
    }

    #[rstest]
    #[case::a_bad_key_first(
        MessageDraft { key: "Key".to_owned(), text: String::new(), values: Vec::new() },
        TemplateError::Key(IdentError::InvalidChar { index: 0 })
    )]
    #[case::empty(draft("", Vec::new()), TemplateError::Empty)]
    #[case::only_spaces(draft("   ", Vec::new()), TemplateError::Empty)]
    #[case::a_line_break(draft("a\nb", Vec::new()), TemplateError::ForbiddenChar { position: 1 })]
    #[case::a_section_sign(draft("\u{a7}cred", Vec::new()), TemplateError::ForbiddenChar { position: 0 })]
    #[case::a_direction_mark(draft("ab\u{202e}c", Vec::new()), TemplateError::ForbiddenChar { position: 2 })]
    #[case::a_zero_width_space(
        draft("/spa\u{200b}wn", Vec::new()),
        TemplateError::ForbiddenChar { position: 4 }
    )]
    #[case::positions_are_in_the_trimmed_text(
        draft("  a\tb", Vec::new()),
        TemplateError::ForbiddenChar { position: 1 }
    )]
    #[case::a_lone_brace(draft("say {", Vec::new()), TemplateError::Brace { position: 4 })]
    #[case::a_name_that_isnt_one(draft("say {Price}", Vec::new()), TemplateError::Brace { position: 4 })]
    #[case::a_value_as_the_commands_name(
        draft("/{name} x", vec![word("name", &["op"])]),
        TemplateError::BraceInCommandName { position: 1 }
    )]
    #[case::a_value_inside_the_commands_name(
        draft("/pay{to} 5", vec![word("to", &["x"])]),
        TemplateError::BraceInCommandName { position: 4 }
    )]
    #[case::a_doubled_brace_in_the_commands_name(
        draft("/a{{b}} x", Vec::new()),
        TemplateError::BraceInCommandName { position: 2 }
    )]
    #[case::a_value_first(
        draft("{greeting} all", vec![word("greeting", &["hi"])]),
        TemplateError::StartsWithValue
    )]
    #[case::nine_values(
        draft(
            "x {a} {b} {c} {d} {e} {f} {g} {h} {i}",
            ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
                .into_iter()
                .map(|name| integer(name, 0, 1))
                .collect()
        ),
        TemplateError::TooManyValues { count: 9 }
    )]
    #[case::a_values_name(
        draft("x {a}", vec![integer("a", 0, 1), integer("B", 0, 1)]),
        TemplateError::ValueName { value: 1, reason: IdentError::InvalidChar { index: 0 } }
    )]
    #[case::a_name_twice(
        draft("x {a}", vec![integer("a", 0, 1), integer("a", 0, 1)]),
        TemplateError::DuplicateValue { value: 1 }
    )]
    #[case::an_empty_range(
        draft("x {a}", vec![integer("a", 2, 1)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::an_empty_decimal_range(
        draft("x {a}", vec![decimal("a", 2.0, 1.0, 2)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::a_range_that_isnt_a_number(
        draft("x {a}", vec![decimal("a", f64::NAN, 1.0, 2)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::an_endless_range(
        draft("x {a}", vec![decimal("a", 0.0, f64::INFINITY, 2)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::a_range_past_the_largest(
        draft("x {a}", vec![decimal("a", 0.0, 100_000_000_000.5, 2)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::a_range_past_the_largest_below_zero(
        draft("x {a}", vec![decimal("a", -100_000_000_001.0, 0.0, 2)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::no_places(
        draft("x {a}", vec![decimal("a", 0.0, 1.0, 0)]),
        TemplateError::Places { value: 0 }
    )]
    #[case::five_places(
        draft("x {a}", vec![decimal("a", 0.0, 1.0, 5)]),
        TemplateError::Places { value: 0 }
    )]
    #[case::a_range_finer_than_its_places(
        draft("x {a}", vec![decimal("a", 0.005, 1.0, 2)]),
        TemplateError::Places { value: 0 }
    )]
    #[case::no_words(
        draft("x {a}", vec![word("a", &[])]),
        TemplateError::Words { value: 0, reason: WordListError::Count { count: 0 } }
    )]
    #[case::a_word_with_a_space(
        draft("x {a}", vec![word("a", &["ok", "not ok"])]),
        TemplateError::Words {
            value: 0,
            reason: WordListError::Word { option: 1, reason: WordError::InvalidChar { index: 3 } }
        }
    )]
    #[case::a_name_that_isnt_declared(
        draft("x {a} {b}", vec![integer("a", 0, 1)]),
        TemplateError::Undeclared { position: 6 }
    )]
    #[case::a_name_twice_in_the_text(
        draft("x {a} {a}", vec![integer("a", 0, 1)]),
        TemplateError::Repeated { position: 6 }
    )]
    #[case::a_value_the_text_lacks(
        draft("x {a}", vec![integer("a", 0, 1), integer("b", 0, 1)]),
        TemplateError::Unused { value: 1 }
    )]
    fn refuses_a_declaration_that_isnt_one(
        #[case] draft: MessageDraft,
        #[case] error: TemplateError,
    ) {
        assert_eq!(MessageTemplate::try_from(draft), Err(error));
    }

    #[rstest]
    #[case::the_text_before_the_values(
        draft("x {", vec![integer("A", 2, 1)]),
        TemplateError::Brace { position: 2 }
    )]
    #[case::what_trimming_removes_isnt_judged(
        draft("} \n", Vec::new()),
        TemplateError::Brace { position: 0 }
    )]
    #[case::the_count_before_any_value(
        draft(
            "x",
            ["A", "b", "c", "d", "e", "f", "g", "h", "i"]
                .into_iter()
                .map(|name| integer(name, 0, 1))
                .collect()
        ),
        TemplateError::TooManyValues { count: 9 }
    )]
    #[case::the_earlier_value(
        draft("x", vec![integer("a", 2, 1), integer("B", 0, 1)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::a_values_range_before_the_texts_names(
        draft("x {b}", vec![integer("a", 2, 1)]),
        TemplateError::Range { value: 0 }
    )]
    #[case::an_undeclared_name_before_an_unused_value(
        draft("x {b}", vec![integer("a", 0, 1)]),
        TemplateError::Undeclared { position: 2 }
    )]
    #[case::the_range_before_the_places(
        draft("x {a}", vec![decimal("a", 2.0, 1.0, 9)]),
        TemplateError::Range { value: 0 }
    )]
    fn the_first_problem_wins(#[case] draft: MessageDraft, #[case] error: TemplateError) {
        assert_eq!(MessageTemplate::try_from(draft), Err(error));
    }

    #[test]
    fn a_forbidden_character_comes_before_a_brace() {
        assert_eq!(
            refused("} \u{7} {", Vec::new()),
            TemplateError::ForbiddenChar { position: 2 }
        );
    }

    #[test]
    fn a_doubled_brace_is_a_literal_brace() {
        let message = template("say {{{n}}}", vec![integer("n", 1, 9)]);

        assert_eq!(message.text(), "say {{{n}}}");
        assert_eq!(
            message
                .render(&[("n", Given::Integer(7))])
                .unwrap()
                .as_str(),
            "say {7}"
        );
        assert_eq!(message.max_len(), 7);
    }

    // --- The longest text ---

    #[rstest]
    #[case::whole_numbers(vec![integer("a", 1, 1_000_000)], 2 + 7)]
    #[case::a_minus_counts(vec![integer("a", -1_000_000, 9)], 2 + 8)]
    #[case::the_smallest_whole_number(vec![integer("a", i64::MIN, i64::MAX)], 2 + 20)]
    #[case::decimal_places_count(vec![decimal("a", 1.0, 1_000_000.0, 2)], 2 + 10)]
    #[case::a_decimal_minus_counts(vec![decimal("a", -10.0, 9.5, 1)], 2 + 5)]
    #[case::the_longest_word(vec![word("a", &["on", "offline"])], 2 + 7)]
    #[case::an_emoji_counts_twice(vec![word("a", &["\u{1f600}"])], 2 + 2)]
    fn the_longest_text_takes_each_value_at_its_longest(
        #[case] values: Vec<ValueDraft>,
        #[case] units: usize,
    ) {
        assert_eq!(template("x {a}", values).max_len(), units);
    }

    #[test]
    fn a_message_that_fits_256_units_at_its_longest_is_declared() {
        // 249 literal characters and a value of up to 7 digits.
        let text = format!("{} {{a}}", "x".repeat(248));

        let message = template(&text, vec![integer("a", 1, 1_000_000)]);

        assert_eq!(message.max_len(), 256);
        let sent = message.render(&[("a", Given::Integer(1_000_000))]).unwrap();
        assert_eq!(crate::text::utf16_len(sent.as_str()), 256);
    }

    #[test]
    fn a_message_that_could_be_longer_than_256_units_is_refused() {
        // It would fit with a short value, and is refused for its longest.
        let text = format!("{} {{a}}", "x".repeat(249));

        assert_eq!(
            refused(&text, vec![integer("a", 1, 1_000_000)]),
            TemplateError::TooLong { units: 257 }
        );
        assert_eq!(
            refused(&"x".repeat(257), Vec::new()),
            TemplateError::TooLong { units: 257 }
        );
    }

    // --- Making the message ---

    #[rstest]
    #[case::the_smallest(1, "/ah sell 1")]
    #[case::inside(1500, "/ah sell 1500")]
    #[case::the_largest(1_000_000, "/ah sell 1000000")]
    fn a_whole_number_in_its_range_is_written_in_digits(#[case] price: i64, #[case] text: &str) {
        let sent = sell().render(&[("price", Given::Integer(price))]).unwrap();

        assert_eq!(sent.as_str(), text);
    }

    #[rstest]
    #[case::below(0)]
    #[case::above(1_000_001)]
    #[case::far_below(i64::MIN)]
    #[case::far_above(i64::MAX)]
    fn a_whole_number_outside_its_range_is_refused(#[case] price: i64) {
        assert_eq!(
            sell().render(&[("price", Given::Integer(price))]),
            Err(RenderError::OutOfRange { value: 0 })
        );
    }

    #[rstest]
    #[case::exact(234.56, "/pay AfkMain 234.56")]
    #[case::places_are_filled(5.0, "/pay AfkMain 5.00")]
    #[case::the_smallest(1.0, "/pay AfkMain 1.00")]
    #[case::the_largest(1_000_000.0, "/pay AfkMain 1000000.00")]
    #[case::rounded_down(2.344, "/pay AfkMain 2.34")]
    #[case::rounded_up(2.346, "/pay AfkMain 2.35")]
    #[case::a_difference_of_two_decimals(1234.56 - 1000.0, "/pay AfkMain 234.56")]
    #[case::just_below_the_range_but_written_inside(0.996, "/pay AfkMain 1.00")]
    #[case::just_above_the_range_but_written_inside(1_000_000.004, "/pay AfkMain 1000000.00")]
    fn a_decimal_number_is_rounded_to_its_places(#[case] amount: f64, #[case] text: &str) {
        let sent = pay().render(&[("amount", Given::Decimal(amount))]).unwrap();

        assert_eq!(sent.as_str(), text);
    }

    #[rstest]
    #[case::written_below(0.994)]
    #[case::written_above(1_000_000.006)]
    #[case::zero(0.0)]
    #[case::negative(-5.0)]
    #[case::huge(1e300)]
    #[case::not_a_number(f64::NAN)]
    #[case::endless(f64::INFINITY)]
    #[case::endless_below(f64::NEG_INFINITY)]
    fn a_decimal_number_written_outside_its_range_is_refused(#[case] amount: f64) {
        assert_eq!(
            pay().render(&[("amount", Given::Decimal(amount))]),
            Err(RenderError::OutOfRange { value: 0 })
        );
    }

    #[rstest]
    #[case::a_small_negative(-0.001, "x 0.00")]
    #[case::negative_zero(-0.0, "x 0.00")]
    #[case::a_real_negative(-0.006, "x -0.01")]
    fn a_zero_is_never_written_with_a_minus(#[case] value: f64, #[case] text: &str) {
        let message = template("x {a}", vec![decimal("a", -1.0, 1.0, 2)]);

        let sent = message.render(&[("a", Given::Decimal(value))]).unwrap();

        assert_eq!(sent.as_str(), text);
    }

    #[rstest]
    #[case::inside(7, Ok("/pay AfkMain 7.00"))]
    #[case::the_largest(1_000_000, Ok("/pay AfkMain 1000000.00"))]
    #[case::below(0, Err(RenderError::OutOfRange { value: 0 }))]
    #[case::above(1_000_001, Err(RenderError::OutOfRange { value: 0 }))]
    #[case::far_above(i64::MAX, Err(RenderError::OutOfRange { value: 0 }))]
    fn a_whole_number_fills_a_decimal_value(
        #[case] amount: i64,
        #[case] expected: Result<&str, RenderError>,
    ) {
        let sent = pay().render(&[("amount", Given::Integer(amount))]);

        assert_eq!(
            sent.as_ref().map(ChatMessage::as_str),
            expected.as_ref().copied()
        );
    }

    #[test]
    fn a_word_must_be_one_of_the_list_exactly() {
        let message = template("/home {place}", vec![word("place", &["farm", "base"])]);

        assert_eq!(
            message
                .render(&[("place", Given::Text("base"))])
                .unwrap()
                .as_str(),
            "/home base"
        );
        for other in ["Farm", "farm ", "", "farm base", "/op AfkBot1"] {
            assert_eq!(
                message.render(&[("place", Given::Text(other))]),
                Err(RenderError::NotAWord { value: 0 }),
                "{other:?}"
            );
        }
    }

    #[rstest]
    #[case::text_for_a_whole_number(sell(), "price", Given::Text("5"))]
    #[case::a_decimal_for_a_whole_number(sell(), "price", Given::Decimal(5.0))]
    #[case::text_for_a_decimal(pay(), "amount", Given::Text("5.00"))]
    #[case::a_number_for_a_word(
        template("/home {place}", vec![word("place", &["1"])]),
        "place",
        Given::Integer(1)
    )]
    #[case::a_decimal_for_a_word(
        template("/home {place}", vec![word("place", &["1"])]),
        "place",
        Given::Decimal(1.0)
    )]
    fn a_value_of_another_kind_is_refused(
        #[case] message: MessageTemplate,
        #[case] name: &str,
        #[case] given: Given<'_>,
    ) {
        assert_eq!(
            message.render(&[(name, given)]),
            Err(RenderError::WrongKind { value: 0 })
        );
    }

    #[test]
    fn every_declared_value_must_be_given_and_no_other() {
        let message = template(
            "/pay {to} {amount}",
            vec![word("to", &["AfkMain"]), integer("amount", 1, 9)],
        );
        let to = ("to", Given::Text("AfkMain"));
        let amount = ("amount", Given::Integer(3));

        assert_eq!(message.render(&[]), Err(RenderError::Missing { value: 0 }));
        assert_eq!(
            message.render(&[to]),
            Err(RenderError::Missing { value: 1 })
        );
        assert_eq!(
            message.render(&[amount]),
            Err(RenderError::Missing { value: 0 })
        );
        assert_eq!(
            message.render(&[to, amount, ("note", Given::Integer(1))]),
            Err(RenderError::Undeclared)
        );
        assert_eq!(
            message.render(&[to, amount, amount]),
            Err(RenderError::Undeclared)
        );
        // In any order.
        assert_eq!(
            message.render(&[amount, to]).unwrap().as_str(),
            "/pay AfkMain 3"
        );
    }

    #[test]
    fn a_name_that_isnt_declared_comes_before_anything_else() {
        assert_eq!(
            sell().render(&[("price", Given::Integer(0)), ("Price", Given::Integer(1))]),
            Err(RenderError::Undeclared)
        );
        assert_eq!(
            template("hello", Vec::new()).render(&[("a", Given::Integer(1))]),
            Err(RenderError::Undeclared)
        );
    }

    /// The net behind a declaration's checks: a template whose parts hold
    /// what no declaration lets in still makes no message.
    #[test]
    fn a_finished_text_that_isnt_a_message_is_refused() {
        let broken = MessageTemplate {
            key: Ident::try_from("key").unwrap(),
            text: "x".to_owned(),
            parts: vec![Part::Literal("a\nb".to_owned())],
            values: Vec::new(),
        };

        assert_eq!(
            broken.render(&[]),
            Err(RenderError::Message(ChatMessageError::ForbiddenChar {
                index: 1
            }))
        );
    }

    #[test]
    fn errors_name_a_place_and_never_the_text() {
        assert_eq!(
            TemplateError::Undeclared { position: 6 }.to_string(),
            "the message names a value it doesn't declare, at position 6"
        );
        assert_eq!(
            TemplateError::TooLong { units: 300 }.to_string(),
            "the message can be 300 UTF-16 code units long; the limit is 256"
        );
        assert_eq!(
            TemplateError::Words {
                value: 2,
                reason: WordListError::Duplicate { option: 1 }
            }
            .to_string(),
            "value 2's words: word 1 is in the list twice"
        );
        assert_eq!(
            RenderError::OutOfRange { value: 0 }.to_string(),
            "value 0 is outside its range"
        );
    }

    // --- JSON ---

    #[test]
    fn a_declared_message_reads_and_writes_its_json() {
        let json = concat!(
            r#"{"key":"pay","text":"/pay {to} {amount} {n}","values":["#,
            r#"{"name":"to","type":"word","options":["AfkMain","AfkBot2"]},"#,
            r#"{"name":"amount","type":"decimal","min":1.0,"max":1000000.0,"places":2},"#,
            r#"{"name":"n","type":"integer","min":1,"max":64}]}"#
        );

        let message: MessageTemplate = serde_json::from_str(json).unwrap();

        assert_eq!(message.key().as_str(), "pay");
        assert_eq!(message.values().len(), 3);
        assert_eq!(serde_json::to_string(&message).unwrap(), json);
    }

    #[rstest]
    #[case::an_invalid_declaration(r#"{"key":"k","text":"{a}","values":[]}"#)]
    #[case::no_key(r#"{"text":"hi","values":[]}"#)]
    #[case::no_values(r#"{"key":"k","text":"hi"}"#)]
    #[case::an_unknown_kind(r#"{"key":"k","text":"x {a}","values":[{"name":"a","type":"text"}]}"#)]
    #[case::a_kind_without_its_range(
        r#"{"key":"k","text":"x {a}","values":[{"name":"a","type":"integer","min":1}]}"#
    )]
    #[case::a_fractional_whole_number(
        r#"{"key":"k","text":"x {a}","values":[{"name":"a","type":"integer","min":1.5,"max":2}]}"#
    )]
    fn refuses_json_that_isnt_a_declared_message(#[case] json: &str) {
        assert!(
            serde_json::from_str::<MessageTemplate>(json).is_err(),
            "{json}"
        );
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{"key":"k","text":"x {a}","note":1,"values":[{"name":"a","type":"integer","min":1,"max":2,"step":1}]}"#;

        let message: MessageTemplate = serde_json::from_str(json).unwrap();

        assert_eq!(
            message
                .render(&[("a", Given::Integer(2))])
                .unwrap()
                .as_str(),
            "x 2"
        );
    }

    // --- Properties ---

    fn any_kind() -> impl Strategy<Value = ValueKind> {
        prop_oneof![
            (any::<i64>(), any::<i64>()).prop_map(|(min, max)| ValueKind::Integer { min, max }),
            (any::<f64>(), any::<f64>(), any::<u8>())
                .prop_map(|(min, max, places)| ValueKind::Decimal { min, max, places }),
            (-2000_i32..2000, 0_i32..4000, 0_u8..6).prop_map(|(min, more, places)| {
                ValueKind::Decimal {
                    min: f64::from(min) / 8.0,
                    max: f64::from(min + more) / 8.0,
                    places,
                }
            }),
            proptest::collection::vec("[a-c/ ]{0,3}", 0..4)
                .prop_map(|options| ValueKind::Word { options }),
        ]
    }

    fn any_draft() -> impl Strategy<Value = MessageDraft> {
        (
            "[a-c]{0,2}",
            "[ /{}a-c\u{a7}]{0,16}",
            proptest::collection::vec(("[a-c]{0,2}", any_kind()), 0..4),
        )
            .prop_map(|(key, text, values)| MessageDraft {
                key,
                text,
                values: values
                    .into_iter()
                    .map(|(name, kind)| ValueDraft { name, kind })
                    .collect(),
            })
    }

    /// A value for each declared one: often in its range, sometimes not.
    fn givens(message: &MessageTemplate, seed: i64) -> Vec<(String, GivenOwned)> {
        message
            .values()
            .iter()
            .map(|value| {
                let given = match value.kind() {
                    ValueKind::Integer { min, max } => GivenOwned::Integer(
                        min.saturating_add(seed.rem_euclid(max.saturating_sub(min).max(1))),
                    ),
                    ValueKind::Decimal { min, max, .. } => GivenOwned::Decimal(
                        min + (max - min)
                            * f64::from(i32::try_from(seed.rem_euclid(1000)).unwrap())
                            / 900.0,
                    ),
                    ValueKind::Word { options } => GivenOwned::Text(
                        options
                            .get(usize::try_from(seed.rem_euclid(8)).unwrap())
                            .cloned()
                            .unwrap_or_default(),
                    ),
                };
                (value.name().as_str().to_owned(), given)
            })
            .collect()
    }

    #[derive(Debug, Clone)]
    enum GivenOwned {
        Integer(i64),
        Decimal(f64),
        Text(String),
    }

    proptest! {
        #[test]
        fn declaring_never_panics(draft in any_draft()) {
            let _ = MessageTemplate::try_from(draft);
        }

        /// Whatever is declared and whatever is given: a finished message
        /// is a chat message no longer than the declared longest, with the
        /// declaration's own command name, and it reads back as declared.
        #[test]
        fn a_finished_message_keeps_what_the_declaration_promised(
            draft in any_draft(),
            seed in any::<i64>(),
        ) {
            if let Ok(message) = MessageTemplate::try_from(draft) {
                prop_assert!(message.max_len() <= ChatMessage::MAX_LEN);
                let again = MessageTemplate::try_from(MessageDraft::from(message.clone()));
                prop_assert_eq!(again.as_ref(), Ok(&message));

                let owned = givens(&message, seed);
                let given: Vec<(&str, Given<'_>)> = owned
                    .iter()
                    .map(|(name, given)| {
                        let given = match given {
                            GivenOwned::Integer(n) => Given::Integer(*n),
                            GivenOwned::Decimal(n) => Given::Decimal(*n),
                            GivenOwned::Text(text) => Given::Text(text),
                        };
                        (name.as_str(), given)
                    })
                    .collect();
                if let Ok(sent) = message.render(&given) {
                    prop_assert!(crate::text::utf16_len(sent.as_str()) <= message.max_len());
                    prop_assert_eq!(sent.command_name(), message.command_name());
                    prop_assert_eq!(sent.is_command(), message.is_command());
                }
            }
        }

        /// A decimal number as written is always inside its range.
        #[test]
        fn a_written_decimal_is_inside_its_range(
            min in -2000_i32..2000,
            more in 0_i32..4000,
            places in 1_u8..=4,
            value in any::<f64>(),
            near in -100_i32..4200,
        ) {
            let (low, high) = (f64::from(min) / 8.0, f64::from(min + more) / 8.0);
            if let Ok(message) = MessageTemplate::try_from(draft("x {a}", vec![decimal("a", low, high, places)])) {
                for value in [value, low + f64::from(near) / 8.0 - 3.0, low, high] {
                    if let Ok(sent) = message.render(&[("a", Given::Decimal(value))]) {
                        let written: f64 = sent.as_str().trim_start_matches("x ").parse().unwrap();
                        prop_assert!(low <= written && written <= high, "{written}");
                        let digits = sent.as_str().rsplit('.').next().unwrap().len();
                        prop_assert_eq!(digits, usize::from(places));
                    }
                }
            }
        }
    }
}
