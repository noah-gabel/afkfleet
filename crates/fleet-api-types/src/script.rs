//! A scripted mode's body and a bot's script as the API carries them
//! (Plan.md P15.21, P15.23, P15.34; ADR-0036, D13, D14, D15; ADR-0037).
//!
//! - **In a request** ([`ModeScriptRequest`]: a mode's `script`, beside an
//!   empty `steps`, in `POST /modes` and `PUT /modes/{id}`): the script's
//!   text, the leash, the tool guard, the blocks a held left button mines,
//!   the messages it may send and the server's chat lines it reads, as a
//!   mode's JSON has them; a declared value and a pattern's capture are flat
//!   objects with a `type` and the fields that type takes, as a step's
//!   action is. [`ModeScriptRequest::script`] checks every rule fleet-core
//!   has, naming the first one broken at its field, with the column of a
//!   place in a text (a field message names the rule, never the value).
//!   Whether the text is a program is the server's to say at save: a 422
//!   at `script.source` with Rhai's message, the line and the column.
//! - **In an answer** ([`ModeScript`]: a mode's `script`): the same, with
//!   the API level the mode was validated at and whether it reads chat at
//!   any time (ADR-0037, C5). Below the right to edit the mode, a declared
//!   command is its `/` and name alone, without its values, as a chat
//!   step's (S15). The script's text keeps its line breaks and tabs
//!   ([`SourceText`]).
//! - **In the live events** (`bot_script`, `bot_script_log`): a bot's
//!   script's status ([`ScriptStatus`]) and its log's lines
//!   ([`ScriptLogLine`]), each text the agent's, cleaned and cut at
//!   [`SCRIPT_REPORT_MAX_CHARS`] characters on the server and again where
//!   it's read.

use core::fmt;

use fleet_core::script::{BlockGuardError, Ident, Identifier, Word};
use fleet_core::script::{
    CaptureDraft, CaptureKind, ChatPatternsError, DecimalMark, Hook, LogKind, MessageDraft,
    PatternDraft, PatternError, ProblemKind, ReportText, Script, ScriptApiLevel, ScriptDraft,
    ScriptError, ScriptLogLine as CoreLogLine, ScriptSource, ScriptSourceError,
    ScriptState as CoreState, TemplateError, ValueDraft, ValueKind, When, clean_source,
};
use fleet_script::CompileError;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{PartialSchema as _, ToSchema};

use crate::error::FieldError;
use crate::int::{SafeInt, integer_schema};
use crate::open::{Open, OpenEnum};
use crate::text::BoundedText;

/// The most bytes of a script's text: 16 KiB (`ScriptSource::MAX_BYTES`).
pub const SCRIPT_SOURCE_MAX_BYTES: usize = ScriptSource::MAX_BYTES;
/// The most characters of a key or a name a mode declares
/// (`Ident::MAX_LEN`).
pub const SCRIPT_KEY_MAX_CHARS: usize = Ident::MAX_LEN;
/// The most characters of a block's registry name (`Identifier::MAX_LEN`).
pub const SCRIPT_BLOCK_MAX_CHARS: usize = Identifier::MAX_LEN;
/// The most characters of a word of a closed list (`Word::MAX_LEN`).
pub const SCRIPT_WORD_MAX_CHARS: usize = Word::MAX_LEN;
/// The most characters of a declared message or pattern as written: a
/// message's longest text is 256 characters, and a `{name}` stands for its
/// value; a pattern's is 128.
pub const SCRIPT_TEXT_MAX_CHARS: usize = 1024;
/// The most characters of a text a bot's script reports
/// (`ReportText::MAX_CHARS`).
pub const SCRIPT_REPORT_MAX_CHARS: usize = ReportText::MAX_CHARS;

// --- A request -------------------------------------------------------------------------

/// A scripted mode's body in a request, beside empty `steps`: the script,
/// what it sets and what it declares. The server sets the API level
/// itself, checks each rule at its field and compiles the script without
/// running it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModeScriptRequest {
    /// The script's text: at most 16 KiB of UTF-8, no control character
    /// but a line break and a tab, no text-direction mark or invisible
    /// character.
    #[schema(schema_with = source_schema)]
    pub source: String,
    /// How far the bot may walk from where it was put, in blocks: 1 to 64.
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub leash: u8,
    /// How many uses the tool guard keeps on a tool: 0 (off) to 100.
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub tool_guard: u8,
    /// The blocks a held left button mines, at most 16 registry names;
    /// `[]` for any block.
    #[schema(schema_with = blocks_schema)]
    pub block_guard: Vec<String>,
    /// The messages the script may send, at most 16.
    pub messages: Vec<ModeMessageRequest>,
    /// The server's chat lines the script reads, at most 8.
    pub patterns: Vec<ModePatternRequest>,
}

/// A message a scripted mode declares, in a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModeMessageRequest {
    /// The key a script sends it by: 1 to 32 letters, digits and `_`.
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub key: String,
    /// The text, with a `{name}` where each value goes; a command's name
    /// is literal.
    #[schema(schema_with = BoundedText::<SCRIPT_TEXT_MAX_CHARS>::schema)]
    pub text: String,
    /// The values the text holds, at most 8.
    pub values: Vec<ModeValueRequest>,
}

/// What a declared value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeValueKind {
    /// A whole number from `min` to `max`.
    Integer,
    /// A decimal number from `min` to `max`, written with `places` digits
    /// after the point.
    Decimal,
    /// One of `options`.
    Word,
}

impl OpenEnum for ModeValueKind {
    const ALL: &'static [Self] = &[Self::Integer, Self::Decimal, Self::Word];

    fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Word => "word",
        }
    }
}

/// A declared value in a request: its `type` and the fields that type
/// takes, no others (`integer`: `min`, `max`; `decimal`: `min`, `max`,
/// `places`; `word`: `options`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModeValueRequest {
    /// The value's name, as the text writes it in braces.
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub name: String,
    /// What it is.
    #[serde(rename = "type")]
    pub kind: ModeValueKind,
    /// `integer`, `decimal`: the smallest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub min: Option<RangeEnd>,
    /// `integer`, `decimal`: the largest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub max: Option<RangeEnd>,
    /// `decimal`: the digits after the point, 1 to 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub places: Option<u8>,
    /// `word`: the words, at most 32, each 1 to 32 characters without
    /// whitespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = words_schema, nullable = false)]
    pub options: Option<Vec<String>>,
}

/// A line of the server's own chat a scripted mode declares it reads, in a
/// request (ADR-0037, C3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModePatternRequest {
    /// The key a script knows it by.
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub key: String,
    /// When it counts: only in a window after the script's own command, or
    /// at any time, the riskier kind (ADR-0037, C4, C5).
    pub when: PatternWhen,
    /// The whole line, with a `{name}` where each capture is.
    #[schema(schema_with = BoundedText::<SCRIPT_TEXT_MAX_CHARS>::schema)]
    pub text: String,
    /// The captures the text holds, at most 4.
    pub captures: Vec<ModeCaptureRequest>,
}

/// When a chat pattern counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatternWhen {
    /// Only inside a window the script opened after its own command.
    Reply,
    /// Whenever the bot is online.
    AnyTime,
}

impl OpenEnum for PatternWhen {
    const ALL: &'static [Self] = &[Self::Reply, Self::AnyTime];

    fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::AnyTime => "any_time",
        }
    }
}

/// What a pattern's capture reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeCaptureKind {
    /// A whole number from `min` to `max`.
    Integer,
    /// An amount, as servers write one, from `min` to `max`.
    Amount,
    /// One of `options`.
    Word,
}

impl OpenEnum for ModeCaptureKind {
    const ALL: &'static [Self] = &[Self::Integer, Self::Amount, Self::Word];

    fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Amount => "amount",
            Self::Word => "word",
        }
    }
}

/// Which character an amount writes before its decimals; the other is its
/// thousands separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
pub enum ModeDecimalMark {
    /// `1,234.56`.
    #[serde(rename = ".")]
    Point,
    /// `1.234,56`.
    #[serde(rename = ",")]
    Comma,
}

impl OpenEnum for ModeDecimalMark {
    const ALL: &'static [Self] = &[Self::Point, Self::Comma];

    fn as_str(self) -> &'static str {
        match self {
            Self::Point => ".",
            Self::Comma => ",",
        }
    }
}

/// A pattern's capture in a request: its `type` and the fields that type
/// takes, no others (`integer`: `min`, `max`; `amount`: `min`, `max`,
/// `decimal_mark`; `word`: `options`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModeCaptureRequest {
    /// The capture's name, as the text writes it in braces.
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub name: String,
    /// What it reads.
    #[serde(rename = "type")]
    pub kind: ModeCaptureKind,
    /// `integer`, `amount`: the smallest value that counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub min: Option<RangeEnd>,
    /// `integer`, `amount`: the largest value that counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub max: Option<RangeEnd>,
    /// `amount`: the character before its decimals, `.` or `,`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub decimal_mark: Option<ModeDecimalMark>,
    /// `word`: the words, at most 32.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = words_schema, nullable = false)]
    pub options: Option<Vec<String>>,
}

/// An end of a range: a JSON number, which remembers whether it was written
/// as a whole number. A whole number is within ±(2^53 − 1), which
/// JavaScript reads exactly; an `integer` takes only those, a `decimal`
/// and an `amount` either. In TypeScript it's a `number`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RangeEnd {
    /// Written without a point or an exponent.
    Whole(SafeInt),
    /// Written with one.
    Decimal(f64),
}

impl RangeEnd {
    /// The number, as a decimal.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        match self {
            // A whole number within ±2^53 is exactly a double.
            Self::Whole(whole) => whole_as_f64(whole.get()),
            Self::Decimal(decimal) => decimal,
        }
    }

    /// The number as a whole number, if it was written as one.
    #[must_use]
    pub const fn whole(self) -> Option<i64> {
        match self {
            Self::Whole(whole) => Some(whole.get()),
            Self::Decimal(_) => None,
        }
    }

    /// A number without a range or a format: TypeScript's `number`.
    #[must_use]
    pub fn schema() -> RefOr<Schema> {
        ObjectBuilder::new().schema_type(Type::Number).into()
    }
}

/// `whole` as a double: exact within ±2^53, which a [`SafeInt`] is.
fn whole_as_f64(whole: i64) -> f64 {
    // `i32` halves are exact as doubles, and so is their sum here.
    let high = i32::try_from(whole >> 32).unwrap_or(0);
    let low = u32::try_from(whole & 0xFFFF_FFFF).unwrap_or(0);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

impl From<i64> for RangeEnd {
    /// `value` written as a whole number, when it's within ±(2^53 − 1),
    /// and as a decimal otherwise.
    fn from(value: i64) -> Self {
        SafeInt::try_new(value).map_or_else(|_| Self::Decimal(whole_as_f64(value)), Self::Whole)
    }
}

impl Serialize for RangeEnd {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Whole(whole) => serializer.serialize_i64(whole.get()),
            Self::Decimal(decimal) => serializer.serialize_f64(*decimal),
        }
    }
}

impl<'de> Deserialize<'de> for RangeEnd {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RangeEndVisitor)
    }
}

/// Reads a [`RangeEnd`] from any JSON number.
struct RangeEndVisitor;

impl Visitor<'_> for RangeEndVisitor {
    type Value = RangeEnd;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a number")
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<RangeEnd, E> {
        SafeInt::try_new(value)
            .map(RangeEnd::Whole)
            .map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<RangeEnd, E> {
        let value = i64::try_from(value).map_err(E::custom)?;
        self.visit_i64(value)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<RangeEnd, E> {
        if value.is_finite() {
            Ok(RangeEnd::Decimal(value))
        } else {
            Err(E::custom("a range's end is a finite number"))
        }
    }
}

/// A rule a scripted mode's body broke: where, as garde writes a path
/// under the request's `script`; the rule, as fleet-core words it, never
/// the value; and the column, from 1, of a place in a one-line text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFieldError {
    /// The field: `script.leash`, `script.messages[2].values[0].min`.
    pub path: String,
    /// The rule.
    pub message: String,
    /// The line, from 1, in the script's text.
    pub line: Option<u32>,
    /// The column, from 1.
    pub column: Option<u32>,
}

impl ScriptFieldError {
    fn at(path: impl Into<String>, message: impl fmt::Display) -> Self {
        Self {
            path: path.into(),
            message: message.to_string(),
            line: None,
            column: None,
        }
    }

    /// The same, at the character `position` of a one-line text, from 0.
    fn column_of(self, position: usize) -> Self {
        Self {
            line: Some(1),
            column: u32::try_from(position.saturating_add(1)).ok(),
            ..self
        }
    }

    /// The same, at `line` and `column` of the script's text.
    fn place(self, line: usize, column: usize) -> Self {
        Self {
            line: u32::try_from(line).ok(),
            column: u32::try_from(column).ok(),
            ..self
        }
    }
}

impl From<ScriptFieldError> for FieldError {
    /// The rule at its field, with its place when it has one: a 422's
    /// field error, and what the app's own check of a script's text says.
    fn from(error: ScriptFieldError) -> Self {
        Self::new(&error.path, &error.message).at_place(error.line, error.column)
    }
}

/// A script whose text isn't a program, as the caller's own mistake: a field
/// error at `script.source` with the engine's message, its line and its
/// column (ADR-0036, D14). The server's 422 at save and the app's check
/// while a script is typed both say it through this, so they say the same.
#[must_use]
pub fn not_a_program(error: &CompileError) -> FieldError {
    let (line, column) = error
        .at
        .map_or((None, None), |at| (Some(at.line), Some(at.column)));
    FieldError::new("script.source", &error.kind.to_string()).at_place(line, column)
}

/// The field message of a field a type needs and a request left out.
pub const SCRIPT_FIELD_NEEDED: &str = "is needed for this type";
/// The field message of a field a type doesn't take.
pub const SCRIPT_FIELD_NOT_TAKEN: &str = "isn't taken by this type";
/// The field message of an integer's end written as a decimal.
pub const SCRIPT_WHOLE_NUMBER: &str = "must be a whole number";

impl ModeScriptRequest {
    /// The scripted mode's body this request makes, at the newest API level
    /// this server knows, or the first rule it breaks: a declared value's or
    /// capture's own fields first, in order, then fleet-core's rules for the
    /// body, in their order. The server reads a script through this alone,
    /// so what passes here is what's checked as a program and stored.
    ///
    /// # Errors
    /// The first [`ScriptFieldError`].
    pub fn script(&self) -> Result<Script, ScriptFieldError> {
        let messages = self
            .messages
            .iter()
            .enumerate()
            .map(|(m, message)| {
                let values = message
                    .values
                    .iter()
                    .enumerate()
                    .map(|(v, value)| {
                        value_draft(&format!("script.messages[{m}].values[{v}]"), value)
                    })
                    .collect::<Result<_, _>>()?;
                Ok(MessageDraft {
                    key: message.key.clone(),
                    text: message.text.clone(),
                    values,
                })
            })
            .collect::<Result<_, ScriptFieldError>>()?;
        let patterns = self
            .patterns
            .iter()
            .enumerate()
            .map(|(p, pattern)| {
                let captures = pattern
                    .captures
                    .iter()
                    .enumerate()
                    .map(|(c, capture)| {
                        capture_draft(&format!("script.patterns[{p}].captures[{c}]"), capture)
                    })
                    .collect::<Result<_, _>>()?;
                Ok(PatternDraft {
                    key: pattern.key.clone(),
                    when: match pattern.when {
                        PatternWhen::Reply => When::Reply,
                        PatternWhen::AnyTime => When::AnyTime,
                    },
                    text: pattern.text.clone(),
                    captures,
                })
            })
            .collect::<Result<_, ScriptFieldError>>()?;
        Script::try_from(ScriptDraft {
            // Level 1 until the server stamps the level a script needs, with
            // the wire at level 2 (Plan.md P16.15): a mode saved now never
            // keeps an agent from Phase 15 from running it.
            api: ScriptApiLevel::V1.get(),
            source: self.source.clone(),
            leash: self.leash,
            tool_guard: self.tool_guard,
            block_guard: self.block_guard.clone(),
            messages,
            patterns,
            signs: Vec::new(),
        })
        .map_err(script_rule)
    }
}

/// The fields a declared value's or capture's `type` takes, each with
/// whether the request has it, in order.
type Fields<'a> = [(&'a str, bool); 4];

/// The first of `fields` that `takes` and the request left out, or that it
/// doesn't take and the request has.
fn misplaced(path: &str, fields: Fields<'_>, takes: &[&str]) -> Result<(), ScriptFieldError> {
    for (name, present) in fields {
        let wanted = takes.contains(&name);
        if wanted && !present {
            return Err(ScriptFieldError::at(
                format!("{path}.{name}"),
                SCRIPT_FIELD_NEEDED,
            ));
        }
        if !wanted && present {
            return Err(ScriptFieldError::at(
                format!("{path}.{name}"),
                SCRIPT_FIELD_NOT_TAKEN,
            ));
        }
    }
    Ok(())
}

/// A range's two ends as whole numbers, or the first written otherwise.
fn whole_range(
    path: &str,
    min: Option<RangeEnd>,
    max: Option<RangeEnd>,
) -> Result<(i64, i64), ScriptFieldError> {
    let whole = |name: &str, end: Option<RangeEnd>| {
        end.and_then(RangeEnd::whole)
            .ok_or_else(|| ScriptFieldError::at(format!("{path}.{name}"), SCRIPT_WHOLE_NUMBER))
    };
    Ok((whole("min", min)?, whole("max", max)?))
}

/// A range's two ends as decimals; [`misplaced`] made sure both are there.
fn decimal_range(min: Option<RangeEnd>, max: Option<RangeEnd>) -> (f64, f64) {
    let end = |end: Option<RangeEnd>| end.map_or(f64::NAN, RangeEnd::as_f64);
    (end(min), end(max))
}

fn value_draft(path: &str, value: &ModeValueRequest) -> Result<ValueDraft, ScriptFieldError> {
    let fields = [
        ("min", value.min.is_some()),
        ("max", value.max.is_some()),
        ("places", value.places.is_some()),
        ("options", value.options.is_some()),
    ];
    let kind = match value.kind {
        ModeValueKind::Integer => {
            misplaced(path, fields, &["min", "max"])?;
            let (min, max) = whole_range(path, value.min, value.max)?;
            ValueKind::Integer { min, max }
        }
        ModeValueKind::Decimal => {
            misplaced(path, fields, &["min", "max", "places"])?;
            let (min, max) = decimal_range(value.min, value.max);
            ValueKind::Decimal {
                min,
                max,
                places: value.places.unwrap_or(0),
            }
        }
        ModeValueKind::Word => {
            misplaced(path, fields, &["options"])?;
            ValueKind::Word {
                options: value.options.clone().unwrap_or_default(),
            }
        }
    };
    Ok(ValueDraft {
        name: value.name.clone(),
        kind,
    })
}

fn capture_draft(
    path: &str,
    capture: &ModeCaptureRequest,
) -> Result<CaptureDraft, ScriptFieldError> {
    let fields = [
        ("min", capture.min.is_some()),
        ("max", capture.max.is_some()),
        ("decimal_mark", capture.decimal_mark.is_some()),
        ("options", capture.options.is_some()),
    ];
    let kind = match capture.kind {
        ModeCaptureKind::Integer => {
            misplaced(path, fields, &["min", "max"])?;
            let (min, max) = whole_range(path, capture.min, capture.max)?;
            CaptureKind::Integer { min, max }
        }
        ModeCaptureKind::Amount => {
            misplaced(path, fields, &["min", "max", "decimal_mark"])?;
            let (min, max) = decimal_range(capture.min, capture.max);
            CaptureKind::Amount {
                min,
                max,
                decimal_mark: match capture.decimal_mark {
                    Some(ModeDecimalMark::Comma) => DecimalMark::Comma,
                    Some(ModeDecimalMark::Point) | None => DecimalMark::Point,
                },
            }
        }
        ModeCaptureKind::Word => {
            misplaced(path, fields, &["options"])?;
            CaptureKind::Word {
                options: capture.options.clone().unwrap_or_default(),
            }
        }
    };
    Ok(CaptureDraft {
        name: capture.name.clone(),
        kind,
    })
}

/// Where a fleet-core rule for a scripted mode's body was broken.
fn script_rule(error: ScriptError) -> ScriptFieldError {
    match error {
        // The server sets the level itself; a body never names it.
        ScriptError::Api(reason) => ScriptFieldError::at("script", reason),
        ScriptError::Source(reason) => match reason {
            ScriptSourceError::TooLong { .. } => ScriptFieldError::at("script.source", reason),
            ScriptSourceError::ForbiddenChar { line, column } => {
                ScriptFieldError::at("script.source", reason).place(line, column)
            }
        },
        ScriptError::Leash(reason) => ScriptFieldError::at("script.leash", reason),
        ScriptError::ToolGuard(reason) => ScriptFieldError::at("script.tool_guard", reason),
        ScriptError::BlockGuard(reason) => match reason {
            BlockGuardError::TooMany { .. } => ScriptFieldError::at("script.block_guard", reason),
            BlockGuardError::Block { block, .. } | BlockGuardError::Duplicate { block } => {
                ScriptFieldError::at(format!("script.block_guard[{block}]"), reason)
            }
        },
        ScriptError::DeclarationsTooLarge { .. } => ScriptFieldError::at("script", error),
        ScriptError::TooManyMessages { .. } => ScriptFieldError::at("script.messages", error),
        ScriptError::Message { message, reason } => message_rule(message, reason),
        ScriptError::DuplicateMessage { message } => {
            ScriptFieldError::at(format!("script.messages[{message}].key"), error)
        }
        ScriptError::Pattern { pattern, reason } => pattern_rule(pattern, reason),
        // A body declares no signs until the wire and the server take them
        // (Plan.md P16.15); their places are named all the same.
        ScriptError::TooManySigns { .. } | ScriptError::SignsNeedLevel2 => {
            ScriptFieldError::at("script.signs", error)
        }
        ScriptError::Sign { sign, reason } => {
            ScriptFieldError::at(format!("script.signs[{sign}]"), reason)
        }
        ScriptError::DuplicateSign { sign } => {
            ScriptFieldError::at(format!("script.signs[{sign}].key"), error)
        }
        ScriptError::Patterns(reason) => match reason {
            ChatPatternsError::TooMany { .. } => ScriptFieldError::at("script.patterns", reason),
            ChatPatternsError::DuplicateKey { pattern } => {
                ScriptFieldError::at(format!("script.patterns[{pattern}].key"), reason)
            }
        },
    }
}

/// Where a declared message broke a rule.
fn message_rule(message: usize, reason: TemplateError) -> ScriptFieldError {
    let base = format!("script.messages[{message}]");
    let at = |field: &str| ScriptFieldError::at(format!("{base}{field}"), reason);
    let value = |value: usize, field: &str| {
        ScriptFieldError::at(format!("{base}.values[{value}]{field}"), reason)
    };
    match reason {
        TemplateError::Key(_) => at(".key"),
        TemplateError::TooManyValues { .. } => at(".values"),
        TemplateError::ValueName { value: v, .. }
        | TemplateError::DuplicateValue { value: v }
        | TemplateError::Unused { value: v } => value(v, ".name"),
        TemplateError::Range { value: v } => value(v, ""),
        TemplateError::Places { value: v } => value(v, ".places"),
        TemplateError::Words { value: v, .. } => value(v, ".options"),
        TemplateError::ForbiddenChar { position }
        | TemplateError::Brace { position }
        | TemplateError::BraceInCommandName { position }
        | TemplateError::Undeclared { position }
        | TemplateError::Repeated { position } => at(".text").column_of(position),
        TemplateError::Empty | TemplateError::StartsWithValue | TemplateError::TooLong { .. } => {
            at(".text")
        }
    }
}

/// Where a declared chat pattern broke a rule.
fn pattern_rule(pattern: usize, reason: PatternError) -> ScriptFieldError {
    let base = format!("script.patterns[{pattern}]");
    let at = |field: &str| ScriptFieldError::at(format!("{base}{field}"), reason);
    let capture = |capture: usize, field: &str| {
        ScriptFieldError::at(format!("{base}.captures[{capture}]{field}"), reason)
    };
    match reason {
        PatternError::Key(_) => at(".key"),
        PatternError::TooManyCaptures { .. } => at(".captures"),
        PatternError::CaptureName { capture: c, .. }
        | PatternError::DuplicateCapture { capture: c }
        | PatternError::Unused { capture: c } => capture(c, ".name"),
        PatternError::Range { capture: c } => capture(c, ""),
        PatternError::Words { capture: c, .. } => capture(c, ".options"),
        PatternError::ForbiddenChar { position }
        | PatternError::Brace { position }
        | PatternError::AdjacentCaptures { position }
        | PatternError::Undeclared { position }
        | PatternError::Repeated { position } => at(".text").column_of(position),
        PatternError::Empty | PatternError::TooLong { .. } | PatternError::TooLittleText { .. } => {
            at(".text")
        }
    }
}

// --- An answer -------------------------------------------------------------------------

/// A script's text in an answer: cleaned where it's read as a script's text
/// is checked (no control character but a line break and a tab, no
/// text-direction mark or invisible character) and cut at 16 KiB, its line
/// breaks and tabs kept. It's a plain JSON string on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceText(String);

impl SourceText {
    /// `text`, cleaned.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(clean_source(text))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A string of at most 16,384 characters, as the field's schema.
    #[must_use]
    pub fn schema() -> RefOr<Schema> {
        source_schema()
    }
}

impl Serialize for SourceText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SourceText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::new(&text))
    }
}

/// A scripted mode's body as its viewer may see it: whole for one who may
/// edit the mode, each declared command its `/` and name alone for
/// everyone else (S15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeScript {
    /// The script API level the mode was validated at; an agent must run it
    /// or a newer one (ADR-0036, D13).
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub api: u8,
    /// The script's text.
    #[ts(type = "string")]
    #[schema(schema_with = SourceText::schema)]
    pub source: SourceText,
    /// The leash's radius, in blocks.
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub leash: u8,
    /// How many uses the tool guard keeps; 0 is off.
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub tool_guard: u8,
    /// The blocks a held left button mines; none for any block.
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = blocks_schema)]
    pub block_guard: Vec<BoundedText<SCRIPT_BLOCK_MAX_CHARS>>,
    /// The messages the script may send.
    pub messages: Vec<ModeMessage>,
    /// The server's chat lines the script reads.
    pub patterns: Vec<ModePattern>,
    /// Whether one of its patterns counts at any time, not only as a reply
    /// to its own command: the riskier kind, which the app marks (ADR-0037,
    /// C5).
    pub any_time_chat: bool,
}

/// A declared message as its viewer may see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeMessage {
    /// Its key.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub key: BoundedText<SCRIPT_KEY_MAX_CHARS>,
    /// Its text; below the right to edit the mode, a command is its `/` and
    /// name alone (S15).
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_TEXT_MAX_CHARS>::schema)]
    pub text: BoundedText<SCRIPT_TEXT_MAX_CHARS>,
    /// Its values; none when its arguments are hidden.
    pub values: Vec<ModeValue>,
    /// Whether a command's arguments and values were left out for this
    /// viewer (S15).
    pub arguments_hidden: bool,
}

/// A declared value as its viewer may see it: its `type` and the fields
/// that type takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeValue {
    /// Its name.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub name: BoundedText<SCRIPT_KEY_MAX_CHARS>,
    /// What it is.
    #[serde(rename = "type")]
    #[ts(as = "ModeValueKind")]
    #[schema(value_type = ModeValueKind)]
    pub kind: Open<ModeValueKind>,
    /// `integer`, `decimal`: the smallest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub min: Option<RangeEnd>,
    /// `integer`, `decimal`: the largest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub max: Option<RangeEnd>,
    /// `decimal`: the digits after the point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub places: Option<u8>,
    /// `word`: the words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<Vec<String>>", optional)]
    #[schema(schema_with = words_schema, nullable = false)]
    pub options: Option<Vec<BoundedText<SCRIPT_WORD_MAX_CHARS>>>,
}

/// A declared chat pattern as its viewer may see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModePattern {
    /// Its key.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub key: BoundedText<SCRIPT_KEY_MAX_CHARS>,
    /// When it counts.
    #[ts(as = "PatternWhen")]
    #[schema(value_type = PatternWhen)]
    pub when: Open<PatternWhen>,
    /// The line.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_TEXT_MAX_CHARS>::schema)]
    pub text: BoundedText<SCRIPT_TEXT_MAX_CHARS>,
    /// Its captures.
    pub captures: Vec<ModeCapture>,
}

/// A pattern's capture as its viewer may see it: its `type` and the fields
/// that type takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeCapture {
    /// Its name.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_KEY_MAX_CHARS>::schema)]
    pub name: BoundedText<SCRIPT_KEY_MAX_CHARS>,
    /// What it reads.
    #[serde(rename = "type")]
    #[ts(as = "ModeCaptureKind")]
    #[schema(value_type = ModeCaptureKind)]
    pub kind: Open<ModeCaptureKind>,
    /// `integer`, `amount`: the smallest value that counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub min: Option<RangeEnd>,
    /// `integer`, `amount`: the largest value that counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<f64>", optional)]
    #[schema(schema_with = RangeEnd::schema, nullable = false)]
    pub max: Option<RangeEnd>,
    /// `amount`: the character before its decimals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<ModeDecimalMark>", optional)]
    #[schema(value_type = Option<ModeDecimalMark>, nullable = false)]
    pub decimal_mark: Option<Open<ModeDecimalMark>>,
    /// `word`: the words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<Vec<String>>", optional)]
    #[schema(schema_with = words_schema, nullable = false)]
    pub options: Option<Vec<BoundedText<SCRIPT_WORD_MAX_CHARS>>>,
}

impl ModeScript {
    /// `script` as its viewer sees it: whole for one who may edit the mode
    /// (`editable`); otherwise each declared command its `/` and name
    /// alone, without its values (S15).
    #[must_use]
    pub fn of(script: &Script, editable: bool) -> Self {
        let draft = ScriptDraft::from(script.clone());
        let any_time_chat = draft
            .patterns
            .iter()
            .any(|pattern| pattern.when == When::AnyTime);
        Self {
            api: draft.api,
            source: SourceText::new(&draft.source),
            leash: draft.leash,
            tool_guard: draft.tool_guard,
            block_guard: draft
                .block_guard
                .iter()
                .map(|block| BoundedText::new(block))
                .collect(),
            messages: draft
                .messages
                .iter()
                .map(|message| shown_message(message, editable))
                .collect(),
            patterns: draft.patterns.iter().map(shown_pattern).collect(),
            any_time_chat,
        }
    }
}

/// `message` as a viewer sees it: a command's `/` and name alone below the
/// right to edit the mode.
fn shown_message(message: &MessageDraft, editable: bool) -> ModeMessage {
    let text = message.text.trim();
    let command = text
        .strip_prefix('/')
        .map(|rest| rest.split(char::is_whitespace).next().unwrap_or_default());
    match command {
        Some(name) if !editable && name.len().saturating_add(1) < text.len() => ModeMessage {
            key: BoundedText::new(&message.key),
            text: BoundedText::new(&format!("/{name}")),
            values: Vec::new(),
            arguments_hidden: true,
        },
        _ => ModeMessage {
            key: BoundedText::new(&message.key),
            text: BoundedText::new(&message.text),
            values: message.values.iter().map(shown_value).collect(),
            arguments_hidden: false,
        },
    }
}

fn words(options: &[String]) -> Vec<BoundedText<SCRIPT_WORD_MAX_CHARS>> {
    options.iter().map(|word| BoundedText::new(word)).collect()
}

fn shown_value(value: &ValueDraft) -> ModeValue {
    let mut shown = ModeValue {
        name: BoundedText::new(&value.name),
        kind: ModeValueKind::Integer.into(),
        min: None,
        max: None,
        places: None,
        options: None,
    };
    match &value.kind {
        ValueKind::Integer { min, max } => {
            shown.min = Some(RangeEnd::from(*min));
            shown.max = Some(RangeEnd::from(*max));
        }
        ValueKind::Decimal { min, max, places } => {
            shown.kind = ModeValueKind::Decimal.into();
            shown.min = Some(RangeEnd::Decimal(*min));
            shown.max = Some(RangeEnd::Decimal(*max));
            shown.places = Some(*places);
        }
        ValueKind::Word { options } => {
            shown.kind = ModeValueKind::Word.into();
            shown.options = Some(words(options));
        }
    }
    shown
}

fn shown_pattern(pattern: &PatternDraft) -> ModePattern {
    ModePattern {
        key: BoundedText::new(&pattern.key),
        when: match pattern.when {
            When::Reply => PatternWhen::Reply,
            When::AnyTime => PatternWhen::AnyTime,
        }
        .into(),
        text: BoundedText::new(&pattern.text),
        captures: pattern.captures.iter().map(shown_capture).collect(),
    }
}

fn shown_capture(capture: &CaptureDraft) -> ModeCapture {
    let mut shown = ModeCapture {
        name: BoundedText::new(&capture.name),
        kind: ModeCaptureKind::Integer.into(),
        min: None,
        max: None,
        decimal_mark: None,
        options: None,
    };
    match &capture.kind {
        CaptureKind::Integer { min, max } => {
            shown.min = Some(RangeEnd::from(*min));
            shown.max = Some(RangeEnd::from(*max));
        }
        CaptureKind::Amount {
            min,
            max,
            decimal_mark,
        } => {
            shown.kind = ModeCaptureKind::Amount.into();
            shown.min = Some(RangeEnd::Decimal(*min));
            shown.max = Some(RangeEnd::Decimal(*max));
            shown.decimal_mark = Some(
                match decimal_mark {
                    DecimalMark::Point => ModeDecimalMark::Point,
                    DecimalMark::Comma => ModeDecimalMark::Comma,
                }
                .into(),
            );
        }
        CaptureKind::Word { options } => {
            shown.kind = ModeCaptureKind::Word.into();
            shown.options = Some(words(options));
        }
    }
    shown
}

// --- The live events -------------------------------------------------------------------

/// Where a bot's script is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptStateKind {
    /// Its hooks run.
    Running,
    /// It failed or hit a limit; it runs nothing until the bot's next Start
    /// or a change of its mode.
    Stopped,
    /// The agent's engine refused it, so it never ran.
    Refused,
    /// The agent's runtime failed while one of its hooks ran.
    Crashed,
}

impl OpenEnum for ScriptStateKind {
    const ALL: &'static [Self] = &[Self::Running, Self::Stopped, Self::Refused, Self::Crashed];

    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Refused => "refused",
            Self::Crashed => "crashed",
        }
    }
}

/// What stopped or refused a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptProblemKind {
    /// The script isn't a program the engine takes.
    Compile,
    /// A hook failed.
    Error,
    /// The script used the API in a way it may not.
    Misuse,
    /// A limit: operations, time, the stack, the size of a value.
    Limit,
    /// The script's state broke its rules.
    State,
}

impl OpenEnum for ScriptProblemKind {
    const ALL: &'static [Self] = &[
        Self::Compile,
        Self::Error,
        Self::Misuse,
        Self::Limit,
        Self::State,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Compile => "compile",
            Self::Error => "error",
            Self::Misuse => "misuse",
            Self::Limit => "limit",
            Self::State => "state",
        }
    }
}

/// A script's hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptHookName {
    /// `on_join`.
    OnJoin,
    /// `on_world_change`.
    OnWorldChange,
    /// `on_death`.
    OnDeath,
    /// `on_respawn`.
    OnRespawn,
    /// `on_timer`.
    OnTimer,
    /// `on_chat`.
    OnChat,
    /// `on_stop`.
    OnStop,
    /// `on_menu_open` (API level 2).
    OnMenuOpen,
    /// `on_menu_update` (API level 2).
    OnMenuUpdate,
    /// `on_menu_close` (API level 2).
    OnMenuClose,
    /// `on_click` (API level 2).
    OnClick,
    /// `on_sign_open` (API level 2).
    OnSignOpen,
}

impl OpenEnum for ScriptHookName {
    const ALL: &'static [Self] = &[
        Self::OnJoin,
        Self::OnWorldChange,
        Self::OnDeath,
        Self::OnRespawn,
        Self::OnTimer,
        Self::OnChat,
        Self::OnStop,
        Self::OnMenuOpen,
        Self::OnMenuUpdate,
        Self::OnMenuClose,
        Self::OnClick,
        Self::OnSignOpen,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::OnJoin => "on_join",
            Self::OnWorldChange => "on_world_change",
            Self::OnDeath => "on_death",
            Self::OnRespawn => "on_respawn",
            Self::OnTimer => "on_timer",
            Self::OnChat => "on_chat",
            Self::OnStop => "on_stop",
            Self::OnMenuOpen => "on_menu_open",
            Self::OnMenuUpdate => "on_menu_update",
            Self::OnMenuClose => "on_menu_close",
            Self::OnClick => "on_click",
            Self::OnSignOpen => "on_sign_open",
        }
    }
}

impl From<Hook> for ScriptHookName {
    fn from(hook: Hook) -> Self {
        match hook {
            Hook::Join => Self::OnJoin,
            Hook::WorldChange => Self::OnWorldChange,
            Hook::Death => Self::OnDeath,
            Hook::Respawn => Self::OnRespawn,
            Hook::Timer => Self::OnTimer,
            Hook::Chat => Self::OnChat,
            Hook::Stop => Self::OnStop,
            Hook::MenuOpen => Self::OnMenuOpen,
            Hook::MenuUpdate => Self::OnMenuUpdate,
            Hook::MenuClose => Self::OnMenuClose,
            Hook::Click => Self::OnClick,
            Hook::SignOpen => Self::OnSignOpen,
        }
    }
}

/// A bot's script's status, as `bot_script` carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ScriptStatus {
    /// Where it is.
    #[ts(as = "ScriptStateKind")]
    #[schema(value_type = ScriptStateKind)]
    pub state: Open<ScriptStateKind>,
    /// `stopped`, `refused`: what, where, and the engine's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub problem: Option<ScriptProblem>,
}

/// What stopped or refused a bot's script.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ScriptProblem {
    /// Its kind.
    #[ts(as = "ScriptProblemKind")]
    #[schema(value_type = ScriptProblemKind)]
    pub kind: Open<ScriptProblemKind>,
    /// The hook that ran, for a script that stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<ScriptHookName>", optional)]
    #[schema(value_type = Option<ScriptHookName>, nullable = false)]
    pub hook: Option<Open<ScriptHookName>>,
    /// Its line in the script, from 1, when the engine gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<1, { u32::MAX }>, nullable = false)]
    pub line: Option<u32>,
    /// Its column, from 1, with `line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<1, { u32::MAX }>, nullable = false)]
    pub column: Option<u32>,
    /// The engine's message about the script.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_REPORT_MAX_CHARS>::schema)]
    pub message: BoundedText<SCRIPT_REPORT_MAX_CHARS>,
}

impl From<&CoreState> for ScriptStatus {
    fn from(state: &CoreState) -> Self {
        let problem = |problem: &fleet_core::script::ScriptProblem| ScriptProblem {
            kind: match problem.kind {
                ProblemKind::Compile => ScriptProblemKind::Compile,
                ProblemKind::Error => ScriptProblemKind::Error,
                ProblemKind::Misuse => ScriptProblemKind::Misuse,
                ProblemKind::Limit => ScriptProblemKind::Limit,
                ProblemKind::State => ScriptProblemKind::State,
            }
            .into(),
            hook: problem.hook.map(|hook| ScriptHookName::from(hook).into()),
            line: problem.place.map(|place| place.line.get()),
            column: problem.place.map(|place| place.column.get()),
            message: BoundedText::new(problem.message.as_str()),
        };
        match state {
            CoreState::Running => Self {
                state: ScriptStateKind::Running.into(),
                problem: None,
            },
            CoreState::Stopped(stopped) => Self {
                state: ScriptStateKind::Stopped.into(),
                problem: Some(problem(stopped)),
            },
            CoreState::Refused(refused) => Self {
                state: ScriptStateKind::Refused.into(),
                problem: Some(problem(refused)),
            },
            CoreState::Crashed => Self {
                state: ScriptStateKind::Crashed.into(),
                problem: None,
            },
        }
    }
}

/// What a line of a bot's script's log is about: `line` is the script's
/// own, every other kind the runtime's about its chat or its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptLogKind {
    /// A line the script wrote.
    Line,
    /// A declared message was handed to the server.
    Sent,
    /// A declared message wasn't sent.
    NotSent,
    /// A reply's window opened.
    WindowOpened,
    /// A reply's window ran out.
    WindowExpired,
    /// A reply's window never opened: its command wasn't sent.
    WindowDropped,
    /// A chat pattern matched.
    Matched,
    /// `chat.send` or `chat.expect` was refused.
    Refused,
    /// `on_stop` failed.
    StopFailed,
    /// Something the script did in a screen or a sign editor didn't go as
    /// asked: a click, a button, a trade, a name, a close or a sign's answer
    /// that wasn't sent, or whose fate wasn't `ok` (API level 2).
    Click,
}

impl OpenEnum for ScriptLogKind {
    const ALL: &'static [Self] = &[
        Self::Line,
        Self::Sent,
        Self::NotSent,
        Self::WindowOpened,
        Self::WindowExpired,
        Self::WindowDropped,
        Self::Matched,
        Self::Refused,
        Self::StopFailed,
        Self::Click,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Sent => "sent",
            Self::NotSent => "not_sent",
            Self::WindowOpened => "window_opened",
            Self::WindowExpired => "window_expired",
            Self::WindowDropped => "window_dropped",
            Self::Matched => "matched",
            Self::Refused => "refused",
            Self::StopFailed => "stop_failed",
            Self::Click => "click",
        }
    }
}

/// A line of a bot's script's log, as `bot_script_log` carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ScriptLogLine {
    /// What it's about.
    #[ts(as = "ScriptLogKind")]
    #[schema(value_type = ScriptLogKind)]
    pub kind: Open<ScriptLogKind>,
    /// The line, as plain text.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCRIPT_REPORT_MAX_CHARS>::schema)]
    pub text: BoundedText<SCRIPT_REPORT_MAX_CHARS>,
}

impl From<&CoreLogLine> for ScriptLogLine {
    fn from(line: &CoreLogLine) -> Self {
        let kind = match line.kind {
            LogKind::Line => ScriptLogKind::Line,
            LogKind::Sent => ScriptLogKind::Sent,
            LogKind::NotSent => ScriptLogKind::NotSent,
            LogKind::WindowOpened => ScriptLogKind::WindowOpened,
            LogKind::WindowExpired => ScriptLogKind::WindowExpired,
            LogKind::WindowDropped => ScriptLogKind::WindowDropped,
            LogKind::Matched => ScriptLogKind::Matched,
            LogKind::Refused => ScriptLogKind::Refused,
            LogKind::StopFailed => ScriptLogKind::StopFailed,
            LogKind::Click => ScriptLogKind::Click,
        };
        Self {
            kind: kind.into(),
            text: BoundedText::new(line.text.as_str()),
        }
    }
}

// --- Schemas ---------------------------------------------------------------------------

/// A script's text: a string of at most 16,384 characters.
fn source_schema() -> RefOr<Schema> {
    BoundedText::<SCRIPT_SOURCE_MAX_BYTES>::schema()
}

/// Blocks: registry names of at most [`SCRIPT_BLOCK_MAX_CHARS`] characters.
fn blocks_schema() -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(BoundedText::<SCRIPT_BLOCK_MAX_CHARS>::schema())
        .into()
}

/// Words: strings of at most [`SCRIPT_WORD_MAX_CHARS`] characters.
fn words_schema() -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(BoundedText::<SCRIPT_WORD_MAX_CHARS>::schema())
        .into()
}

#[cfg(test)]
mod tests {
    use fleet_core::script::{LeashRadiusError, ScriptPlace};
    use serde_json::{Value, json};

    use super::*;

    /// The balance example's body as an app sends it.
    fn balance() -> Value {
        json!({
            "source": "fn on_join(first) {\n\tchat.send(\"balance\");\n}\n",
            "leash": 8,
            "tool_guard": 2,
            "block_guard": ["cobblestone"],
            "messages": [
                { "key": "balance", "text": "/balance", "values": [] },
                { "key": "pay", "text": "/pay AfkMain {amount}", "values": [
                    { "name": "amount", "type": "decimal", "min": 1, "max": 1_000_000, "places": 2 }
                ] },
                { "key": "count", "text": "/give AfkMain {n}", "values": [
                    { "name": "n", "type": "integer", "min": 1, "max": 64 }
                ] }
            ],
            "patterns": [
                { "key": "balance", "when": "reply", "text": "Balance: ${amount}", "captures": [
                    { "name": "amount", "type": "amount", "min": 0, "max": 1e9, "decimal_mark": "." }
                ] },
                { "key": "restart", "when": "any_time", "text": "Restarting in {m} minutes", "captures": [
                    { "name": "m", "type": "integer", "min": 0, "max": 60 }
                ] }
            ]
        })
    }

    fn request(body: Value) -> ModeScriptRequest {
        serde_json::from_value(body).unwrap()
    }

    fn with(change: impl FnOnce(&mut Value)) -> Result<Script, ScriptFieldError> {
        let mut body = balance();
        change(&mut body);
        request(body).script()
    }

    fn rule(path: &str, message: &str) -> ScriptFieldError {
        ScriptFieldError::at(path, message)
    }

    #[test]
    fn a_body_reads_as_fleet_cores_script_at_level_1_until_the_server_stamps_its_own() {
        let script = request(balance()).script().unwrap();

        assert_eq!(script.api(), ScriptApiLevel::V1);
        assert_eq!(script.leash().blocks(), 8);
        assert_eq!(
            script
                .block_guard()
                .blocks()
                .first()
                .map(Identifier::as_str),
            Some("minecraft:cobblestone")
        );
        assert_eq!(script.messages().len(), 3);
        assert_eq!(script.patterns().len(), 2);
        assert_eq!(script.source().as_str(), balance()["source"]);
    }

    #[test]
    fn a_value_or_capture_takes_its_types_fields_and_no_others() {
        assert_eq!(
            with(|body| body["messages"][1]["values"][0]["places"] = Value::Null).unwrap_err(),
            rule("script.messages[1].values[0].places", SCRIPT_FIELD_NEEDED)
        );
        assert_eq!(
            with(|body| body["messages"][2]["values"][0]["options"] = json!(["a"])).unwrap_err(),
            rule(
                "script.messages[2].values[0].options",
                SCRIPT_FIELD_NOT_TAKEN
            )
        );
        assert_eq!(
            with(|body| body["patterns"][1]["captures"][0]["max"] = json!(60.5)).unwrap_err(),
            rule("script.patterns[1].captures[0].max", SCRIPT_WHOLE_NUMBER)
        );
        assert_eq!(
            with(|body| body["patterns"][0]["captures"][0]["decimal_mark"] = Value::Null)
                .unwrap_err(),
            rule(
                "script.patterns[0].captures[0].decimal_mark",
                SCRIPT_FIELD_NEEDED
            )
        );
    }

    #[test]
    fn a_rule_of_fleet_cores_is_named_at_its_field_with_its_column() {
        assert_eq!(
            with(|body| body["leash"] = json!(0)).unwrap_err(),
            rule("script.leash", &LeashRadiusError { blocks: 0 }.to_string())
        );
        let forbidden =
            with(|body| body["source"] = json!("fn on_join(first) {\n  x\u{202e}\n}")).unwrap_err();
        assert_eq!(
            (forbidden.path.as_str(), forbidden.line, forbidden.column),
            ("script.source", Some(2), Some(4))
        );
        let brace =
            with(|body| body["patterns"][0]["text"] = json!("Balance: } ${amount}")).unwrap_err();
        assert_eq!(
            (brace.path.as_str(), brace.line, brace.column),
            ("script.patterns[0].text", Some(1), Some(10))
        );
        assert_eq!(
            with(|body| body["messages"][1]["key"] = json!("balance"))
                .unwrap_err()
                .path,
            "script.messages[1].key"
        );
        assert_eq!(
            with(|body| body["block_guard"] = json!(["stone", "minecraft:stone"]))
                .unwrap_err()
                .path,
            "script.block_guard[1]"
        );
        assert_eq!(
            with(|body| body["messages"][2]["values"][0]["max"] = json!(-5))
                .unwrap_err()
                .path,
            "script.messages[2].values[0]"
        );
    }

    #[test]
    fn a_body_has_no_level_or_other_unknown_field() {
        let mut body = balance();
        body["api"] = json!(1);

        assert!(serde_json::from_value::<ModeScriptRequest>(body).is_err());
    }

    #[test]
    fn a_range_end_remembers_whether_it_was_whole() {
        let whole: RangeEnd = serde_json::from_value(json!(64)).unwrap();
        let decimal: RangeEnd = serde_json::from_value(json!(64.5)).unwrap();

        assert_eq!(whole.whole(), Some(64));
        assert_eq!(decimal.whole(), None);
        assert!((decimal.as_f64() - 64.5).abs() < f64::EPSILON);
        assert!(serde_json::from_value::<RangeEnd>(json!(9_007_199_254_740_992_u64)).is_err());
        assert_eq!(serde_json::to_value(RangeEnd::from(-3)).unwrap(), json!(-3));
        assert!(
            (RangeEnd::from(-(1_i64 << 40)).as_f64() + 1_099_511_627_776.0).abs() < f64::EPSILON
        );
    }

    #[test]
    fn an_answer_shows_the_body_whole_to_whoever_may_edit_it() {
        let script = request(balance()).script().unwrap();

        let shown = ModeScript::of(&script, true);

        assert_eq!(shown.api, 1);
        assert!(shown.any_time_chat);
        assert_eq!(shown.source.as_str(), balance()["source"]);
        assert_eq!(
            shown.block_guard,
            [BoundedText::new("minecraft:cobblestone")]
        );
        let pay = shown.messages.get(1).unwrap();
        assert_eq!(pay.text.as_str(), "/pay AfkMain {amount}");
        assert!(!pay.arguments_hidden);
        assert_eq!(pay.values.first().unwrap().places, Some(2));
        let count = shown.messages.get(2).unwrap().values.first().unwrap();
        assert_eq!(
            (
                count.min.and_then(RangeEnd::whole),
                count.max.and_then(RangeEnd::whole)
            ),
            (Some(1), Some(64))
        );
    }

    #[test]
    fn below_the_right_to_edit_a_declared_command_is_its_name_alone() {
        let script = request(balance()).script().unwrap();

        let shown = ModeScript::of(&script, false);

        let texts: Vec<(&str, bool, usize)> = shown
            .messages
            .iter()
            .map(|message| {
                (
                    message.text.as_str(),
                    message.arguments_hidden,
                    message.values.len(),
                )
            })
            .collect();
        assert_eq!(
            texts,
            [
                ("/balance", false, 0),
                ("/pay", true, 0),
                ("/give", true, 0)
            ]
        );
    }

    #[test]
    fn a_source_read_from_an_answer_keeps_its_lines_and_loses_what_a_script_may_not_hold() {
        let read: SourceText = serde_json::from_value(json!("a\r\n\tb\u{202e}\u{7}c")).unwrap();

        assert_eq!(read.as_str(), "a\n\tbc");
    }

    #[test]
    fn a_report_reads_as_its_dto() {
        let refused = CoreState::Refused(fleet_core::script::ScriptProblem {
            kind: ProblemKind::Compile,
            hook: None,
            place: ScriptPlace::new(2, 3),
            message: ReportText::clean("`kees` isn't defined"),
        });

        assert_eq!(
            serde_json::to_value(ScriptStatus::from(&refused)).unwrap(),
            json!({ "state": "refused", "problem": {
                "kind": "compile", "line": 2, "column": 3, "message": "`kees` isn't defined"
            } })
        );
        assert_eq!(
            serde_json::to_value(ScriptStatus::from(&CoreState::Running)).unwrap(),
            json!({ "state": "running" })
        );
        let line = CoreLogLine {
            kind: LogKind::WindowOpened,
            text: ReportText::clean("waiting for `balance`"),
        };
        assert_eq!(
            serde_json::to_value(ScriptLogLine::from(&line)).unwrap(),
            json!({ "kind": "window_opened", "text": "waiting for `balance`" })
        );
    }

    #[test]
    fn every_open_name_is_its_serde_name() {
        fn check<E: OpenEnum + Serialize>() {
            for value in E::ALL {
                assert_eq!(serde_json::to_value(value).unwrap(), json!(value.as_str()));
            }
        }
        check::<ModeValueKind>();
        check::<ModeCaptureKind>();
        check::<ModeDecimalMark>();
        check::<PatternWhen>();
        check::<ScriptStateKind>();
        check::<ScriptProblemKind>();
        check::<ScriptHookName>();
        check::<ScriptLogKind>();
    }

    #[test]
    fn a_rule_becomes_its_field_error_with_its_place() {
        let error =
            with(|body| body["source"] = json!("fn on_join(first) {\n\u{200b}}")).unwrap_err();

        let field = FieldError::from(error.clone());

        assert_eq!(field.path.as_str(), "script.source");
        assert_eq!(field.message.as_str(), error.message);
        assert_eq!((field.line, field.column), (Some(2), Some(1)));
        let leash = FieldError::from(rule("script.leash", "is too far"));
        assert_eq!(
            (leash.path.as_str(), leash.line, leash.column),
            ("script.leash", None, None)
        );
    }

    #[test]
    fn a_script_that_isnt_a_program_is_a_field_error_at_its_source_with_its_place() {
        let script = request(json!({
            "source": "fn on_join(first) {\n\tkees.forward.hold();\n}\n",
            "leash": 8, "tool_guard": 2, "block_guard": [], "messages": [], "patterns": []
        }))
        .script()
        .unwrap();
        let error = fleet_script::compile(&script).unwrap_err();

        let field = not_a_program(&error);

        assert_eq!(field.path.as_str(), "script.source");
        assert_eq!(field.message.as_str(), error.kind.to_string());
        assert!(
            field.message.as_str().contains("kees"),
            "{}",
            field.message.as_str()
        );
        assert_eq!((field.line, field.column), (Some(2), Some(2)));
    }

    #[test]
    fn a_compile_error_without_a_place_has_none() {
        let error = CompileError {
            kind: fleet_script::CompileErrorKind::TopLevelStatement,
            at: None,
        };

        let field = not_a_program(&error);

        assert_eq!((field.line, field.column), (None, None));
    }
}
