//! A scripted mode's body, and what an agent reports about a bot's script
//! (Plan.md P15.20; ADR-0036, D13; ADR-0037).
//!
//! - **A script** crosses as structured messages that mirror fleet-core's
//!   [`ScriptDraft`]. Reading builds the draft and checks it with
//!   [`Script::try_from`], and the result must write back to exactly what was
//!   read, as a chat message must: an agent runs what the server validated,
//!   never a repaired version of it. Whether the source is a program is the
//!   agent's own engine's to say, per bot (ADR-0036, D13).
//! - **A script's status and log** come from an agent, which the server
//!   trusts in nothing: every text is cleaned as untrusted text and cut at
//!   [`ReportText::MAX_CHARS`] characters, never refused for its length, and
//!   a place is a line and a column that both count from 1.

use fleet_core::script::{
    CaptureDraft, CaptureKind, DecimalMark, Hook, LogKind, MessageDraft, PatternDraft, ProblemKind,
    ReportText, Script, ScriptDraft, ScriptLogLine, ScriptPlace, ScriptProblem, ScriptState,
    ValueDraft, ValueKind, When,
};

use super::{enum_from_wire, required};
use crate::generated as wire;
use crate::{ConvertError, Problem};

/// Writes a script.
pub(crate) fn script_to_wire(script: Script) -> wire::Script {
    let draft = ScriptDraft::from(script);
    wire::Script {
        api: u32::from(draft.api),
        source: draft.source,
        leash: u32::from(draft.leash),
        tool_guard: u32::from(draft.tool_guard),
        block_guard: draft.block_guard,
        messages: draft.messages.into_iter().map(message_to_wire).collect(),
        patterns: draft.patterns.into_iter().map(pattern_to_wire).collect(),
    }
}

/// Reads a script: its draft, checked by fleet-core, in exactly the form
/// [`Script`] keeps it.
pub(crate) fn script_from_wire(script: wire::Script) -> Result<Script, ConvertError> {
    let draft = ScriptDraft {
        api: small("Script.api", script.api)?,
        source: script.source,
        leash: small("Script.leash", script.leash)?,
        tool_guard: small("Script.tool_guard", script.tool_guard)?,
        block_guard: script.block_guard,
        messages: script
            .messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| message_from_wire(message).map_err(|e| e.at(index)))
            .collect::<Result<_, _>>()?,
        patterns: script
            .patterns
            .into_iter()
            .enumerate()
            .map(|(index, pattern)| pattern_from_wire(pattern).map_err(|e| e.at(index)))
            .collect::<Result<_, _>>()?,
    };
    let checked = Script::try_from(draft.clone())
        .map_err(|e| ConvertError::new("ModeDefinition.script", e.into()))?;
    if ScriptDraft::from(checked.clone()) == draft {
        Ok(checked)
    } else {
        Err(ConvertError::new(
            "ModeDefinition.script",
            Problem::NotExact,
        ))
    }
}

/// A number that fits a `u8`, as a script's small numbers do.
fn small(field: &'static str, number: u32) -> Result<u8, ConvertError> {
    u8::try_from(number).map_err(|_| ConvertError::new(field, Problem::OutOfRange))
}

fn message_to_wire(message: MessageDraft) -> wire::DeclaredMessage {
    use wire::declared_value::Kind;
    wire::DeclaredMessage {
        key: message.key,
        text: message.text,
        values: message
            .values
            .into_iter()
            .map(|value| wire::DeclaredValue {
                name: value.name,
                kind: Some(match value.kind {
                    ValueKind::Integer { min, max } => {
                        Kind::Integer(wire::IntegerRange { min, max })
                    }
                    ValueKind::Decimal { min, max, places } => Kind::Decimal(wire::DecimalRange {
                        min,
                        max,
                        places: u32::from(places),
                    }),
                    ValueKind::Word { options } => Kind::Word(wire::Words { options }),
                }),
            })
            .collect(),
    }
}

fn message_from_wire(message: wire::DeclaredMessage) -> Result<MessageDraft, ConvertError> {
    use wire::declared_value::Kind;
    let values = message
        .values
        .into_iter()
        .map(|value| {
            let kind = match required("DeclaredValue.kind", value.kind)? {
                Kind::Integer(range) => ValueKind::Integer {
                    min: range.min,
                    max: range.max,
                },
                Kind::Decimal(range) => ValueKind::Decimal {
                    min: range.min,
                    max: range.max,
                    places: small("DecimalRange.places", range.places)?,
                },
                Kind::Word(words) => ValueKind::Word {
                    options: words.options,
                },
            };
            Ok(ValueDraft {
                name: value.name,
                kind,
            })
        })
        .collect::<Result<_, ConvertError>>()?;
    Ok(MessageDraft {
        key: message.key,
        text: message.text,
        values,
    })
}

fn pattern_to_wire(pattern: PatternDraft) -> wire::ChatPattern {
    use wire::pattern_capture::Kind;
    let when = match pattern.when {
        When::Reply => wire::PatternWhen::Reply,
        When::AnyTime => wire::PatternWhen::AnyTime,
    };
    wire::ChatPattern {
        key: pattern.key,
        when: when.into(),
        text: pattern.text,
        captures: pattern
            .captures
            .into_iter()
            .map(|capture| wire::PatternCapture {
                name: capture.name,
                kind: Some(match capture.kind {
                    CaptureKind::Integer { min, max } => {
                        Kind::Integer(wire::IntegerRange { min, max })
                    }
                    CaptureKind::Amount {
                        min,
                        max,
                        decimal_mark,
                    } => Kind::Amount(wire::AmountRange {
                        min,
                        max,
                        decimal_mark: match decimal_mark {
                            DecimalMark::Point => wire::DecimalMark::Point,
                            DecimalMark::Comma => wire::DecimalMark::Comma,
                        }
                        .into(),
                    }),
                    CaptureKind::Word { options } => Kind::Word(wire::Words { options }),
                }),
            })
            .collect(),
    }
}

fn pattern_from_wire(pattern: wire::ChatPattern) -> Result<PatternDraft, ConvertError> {
    use wire::pattern_capture::Kind;
    const WHEN: &str = "ChatPattern.when";
    const MARK: &str = "AmountRange.decimal_mark";
    let when = match enum_from_wire(WHEN, pattern.when)? {
        wire::PatternWhen::Reply => When::Reply,
        wire::PatternWhen::AnyTime => When::AnyTime,
        wire::PatternWhen::Unspecified => return Err(ConvertError::new(WHEN, Problem::Missing)),
    };
    let captures = pattern
        .captures
        .into_iter()
        .map(|capture| {
            let kind = match required("PatternCapture.kind", capture.kind)? {
                Kind::Integer(range) => CaptureKind::Integer {
                    min: range.min,
                    max: range.max,
                },
                Kind::Amount(range) => CaptureKind::Amount {
                    min: range.min,
                    max: range.max,
                    decimal_mark: match enum_from_wire(MARK, range.decimal_mark)? {
                        wire::DecimalMark::Point => DecimalMark::Point,
                        wire::DecimalMark::Comma => DecimalMark::Comma,
                        wire::DecimalMark::Unspecified => {
                            return Err(ConvertError::new(MARK, Problem::Missing));
                        }
                    },
                },
                Kind::Word(words) => CaptureKind::Word {
                    options: words.options,
                },
            };
            Ok(CaptureDraft {
                name: capture.name,
                kind,
            })
        })
        .collect::<Result<_, ConvertError>>()?;
    Ok(PatternDraft {
        key: pattern.key,
        when,
        text: pattern.text,
        captures,
    })
}

/// Writes a script's status.
pub(crate) fn status_to_wire(state: ScriptState) -> wire::ScriptStatus {
    use wire::script_status::Status;
    let status = match state {
        ScriptState::Running => Status::Running(wire::Unit {}),
        ScriptState::Stopped(problem) => Status::Stopped(problem_to_wire(&problem)),
        ScriptState::Refused(problem) => Status::Refused(problem_to_wire(&problem)),
        ScriptState::Crashed => Status::Crashed(wire::Unit {}),
    };
    wire::ScriptStatus {
        status: Some(status),
    }
}

/// Reads a script's status, its texts cleaned.
pub(crate) fn status_from_wire(status: wire::ScriptStatus) -> Result<ScriptState, ConvertError> {
    use wire::script_status::Status;
    Ok(match required("ScriptStatus.status", status.status)? {
        Status::Running(wire::Unit {}) => ScriptState::Running,
        Status::Stopped(problem) => ScriptState::Stopped(problem_from_wire(&problem)?),
        Status::Refused(problem) => ScriptState::Refused(problem_from_wire(&problem)?),
        Status::Crashed(wire::Unit {}) => ScriptState::Crashed,
    })
}

fn problem_to_wire(problem: &ScriptProblem) -> wire::ScriptProblem {
    let kind = match problem.kind {
        ProblemKind::Compile => wire::ScriptProblemKind::Compile,
        ProblemKind::Error => wire::ScriptProblemKind::Error,
        ProblemKind::Misuse => wire::ScriptProblemKind::Misuse,
        ProblemKind::Limit => wire::ScriptProblemKind::Limit,
        ProblemKind::State => wire::ScriptProblemKind::State,
    };
    let hook = match problem.hook {
        None => wire::ScriptHook::Unspecified,
        Some(Hook::Join) => wire::ScriptHook::Join,
        Some(Hook::WorldChange) => wire::ScriptHook::WorldChange,
        Some(Hook::Death) => wire::ScriptHook::Death,
        Some(Hook::Respawn) => wire::ScriptHook::Respawn,
        Some(Hook::Timer) => wire::ScriptHook::Timer,
        Some(Hook::Chat) => wire::ScriptHook::Chat,
        Some(Hook::Stop) => wire::ScriptHook::Stop,
    };
    wire::ScriptProblem {
        kind: kind.into(),
        hook: hook.into(),
        line: problem.place.map(|place| place.line.get()),
        column: problem.place.map(|place| place.column.get()),
        message: problem.message.as_str().to_owned(),
    }
}

fn problem_from_wire(problem: &wire::ScriptProblem) -> Result<ScriptProblem, ConvertError> {
    const KIND: &str = "ScriptProblem.kind";
    const HOOK: &str = "ScriptProblem.hook";
    const PLACE: &str = "ScriptProblem.line";
    let kind = match enum_from_wire(KIND, problem.kind)? {
        wire::ScriptProblemKind::Compile => ProblemKind::Compile,
        wire::ScriptProblemKind::Error => ProblemKind::Error,
        wire::ScriptProblemKind::Misuse => ProblemKind::Misuse,
        wire::ScriptProblemKind::Limit => ProblemKind::Limit,
        wire::ScriptProblemKind::State => ProblemKind::State,
        wire::ScriptProblemKind::Unspecified => {
            return Err(ConvertError::new(KIND, Problem::Missing));
        }
    };
    // 0 is no hook: a refusal, which no hook ran.
    let hook = match problem.hook {
        0 => None,
        number => Some(enum_from_wire(HOOK, number)?),
    };
    let hook = match hook {
        None | Some(wire::ScriptHook::Unspecified) => None,
        Some(wire::ScriptHook::Join) => Some(Hook::Join),
        Some(wire::ScriptHook::WorldChange) => Some(Hook::WorldChange),
        Some(wire::ScriptHook::Death) => Some(Hook::Death),
        Some(wire::ScriptHook::Respawn) => Some(Hook::Respawn),
        Some(wire::ScriptHook::Timer) => Some(Hook::Timer),
        Some(wire::ScriptHook::Chat) => Some(Hook::Chat),
        Some(wire::ScriptHook::Stop) => Some(Hook::Stop),
    };
    // A line and a column, both from 1, or neither.
    let place = match (problem.line, problem.column) {
        (None, None) => None,
        (Some(line), Some(column)) => {
            Some(ScriptPlace::new(line, column).ok_or(ConvertError::new(PLACE, Problem::Missing))?)
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(ConvertError::new(PLACE, Problem::Missing));
        }
    };
    Ok(ScriptProblem {
        kind,
        hook,
        place,
        message: ReportText::clean(&problem.message),
    })
}

/// Writes a line of a script's log.
pub(crate) fn log_to_wire(line: &ScriptLogLine) -> wire::ScriptLogLine {
    let kind = match line.kind {
        LogKind::Line => wire::ScriptLogKind::Line,
        LogKind::Sent => wire::ScriptLogKind::Sent,
        LogKind::NotSent => wire::ScriptLogKind::NotSent,
        LogKind::WindowOpened => wire::ScriptLogKind::WindowOpened,
        LogKind::WindowExpired => wire::ScriptLogKind::WindowExpired,
        LogKind::WindowDropped => wire::ScriptLogKind::WindowDropped,
        LogKind::Matched => wire::ScriptLogKind::Matched,
        LogKind::Refused => wire::ScriptLogKind::Refused,
        LogKind::StopFailed => wire::ScriptLogKind::StopFailed,
    };
    wire::ScriptLogLine {
        kind: kind.into(),
        text: line.text.as_str().to_owned(),
    }
}

/// Reads a line of a script's log, its text cleaned.
pub(crate) fn log_from_wire(line: &wire::ScriptLogLine) -> Result<ScriptLogLine, ConvertError> {
    const KIND: &str = "ScriptLogLine.kind";
    let kind = match enum_from_wire(KIND, line.kind)? {
        wire::ScriptLogKind::Line => LogKind::Line,
        wire::ScriptLogKind::Sent => LogKind::Sent,
        wire::ScriptLogKind::NotSent => LogKind::NotSent,
        wire::ScriptLogKind::WindowOpened => LogKind::WindowOpened,
        wire::ScriptLogKind::WindowExpired => LogKind::WindowExpired,
        wire::ScriptLogKind::WindowDropped => LogKind::WindowDropped,
        wire::ScriptLogKind::Matched => LogKind::Matched,
        wire::ScriptLogKind::Refused => LogKind::Refused,
        wire::ScriptLogKind::StopFailed => LogKind::StopFailed,
        wire::ScriptLogKind::Unspecified => return Err(ConvertError::new(KIND, Problem::Missing)),
    };
    Ok(ScriptLogLine {
        kind,
        text: ReportText::clean(&line.text),
    })
}
