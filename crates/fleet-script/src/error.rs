//! What's wrong with a script when it's saved ([`CompileError`]), and why a
//! running one stopped ([`ScriptError`]).

use core::fmt;

use fleet_core::script::Hook;

/// A place in a script's text: a line and a column, both counted from 1,
/// the column in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourcePosition {
    /// The line, from 1.
    pub line: u32,
    /// The column in that line, from 1.
    pub column: u32,
}

impl fmt::Display for SourcePosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column)
    }
}

/// What's wrong with a script that can't be saved (ADR-0036, D14).
///
/// The names it holds are the script's own identifiers, written by its
/// author. A name, like Rhai's message in [`Syntax`](Self::Syntax), is one
/// line of at most [`CompileError::MAX_MESSAGE_CHARS`] characters: a longer
/// one is cut there.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileErrorKind {
    /// The text isn't a Rhai program: Rhai's own message.
    #[error("{message}")]
    Syntax {
        /// What Rhai says is wrong.
        message: String,
    },
    /// A name that's neither a variable of the script nor a game object.
    #[error("`{name}` isn't defined")]
    UndefinedVariable {
        /// The name.
        name: String,
    },
    /// A variable or a parameter named like a game object or `state`.
    #[error("`{name}` is a game object's name and can't be a variable or a parameter")]
    ReservedName {
        /// The name.
        name: String,
    },
    /// `import`: a script can't load another one.
    #[error("`import` isn't part of the language here")]
    Import,
    /// `eval`: a script can't run text as a program.
    #[error("`eval` isn't part of the language here")]
    Eval,
    /// An anonymous function, `|x| …`: a script's functions all have
    /// names.
    #[error(
        "an anonymous function isn't part of the language here; give it a name and pass `Fn(\"name\")`"
    )]
    AnonymousFunction,
    /// A function whose name starts with `on_` and isn't a hook's.
    #[error("`{name}` isn't a hook; a function whose name starts with `on_` must be one")]
    UnknownHook {
        /// The function's name.
        name: String,
    },
    /// A hook with other parameters than it's called with.
    #[error("`{hook}` takes {expected} parameters, not {found}", expected = hook.parameters().len())]
    HookParameters {
        /// The hook.
        hook: Hook,
        /// How many parameters the script gave it.
        found: usize,
    },
    /// Something outside a function: a script is functions only, and
    /// nothing outside one would ever run.
    #[error("a script holds only functions; nothing outside a function ever runs")]
    TopLevelStatement,
    /// More functions than a script may define, closures included.
    #[error("the script defines more than {max} functions", max = fleet_core::script::limits::MAX_FUNCTIONS)]
    TooManyFunctions,
    /// An expression nested deeper than the limit.
    #[error("an expression is nested more than {max} deep", max = fleet_core::script::limits::MAX_FUNCTION_EXPRESSION_DEPTH)]
    ExpressionTooDeep,
    /// A raw string, `#"…"#`: Rhai 1.27.0 loses a line inside one, so the
    /// places after it would be wrong (ADR-0040's amendment, A6).
    #[error(
        "a raw string (`#\"…\"#`) isn't part of the language here; write the text in quotes or backticks"
    )]
    RawString,
    /// A game object or a hook of a newer API level than the mode's: a
    /// level-1 script naming `menu`, say (ADR-0036, D13). At level 1 the
    /// names of level 2 are a script's own to use, so only one used as
    /// the object or the hook is this.
    #[error("`{name}` needs script API level {level}; this mode's is lower")]
    NeedsLevel {
        /// The name.
        name: String,
        /// The level it came with.
        level: u8,
    },
}

/// A script that can't be saved: what's wrong, and where (ADR-0036, D14).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct CompileError {
    /// What's wrong.
    pub kind: CompileErrorKind,
    /// Where, when the place is known.
    pub at: Option<SourcePosition>,
}

impl CompileError {
    /// The longest message of Rhai's own, and the longest name, that an
    /// error carries, in characters.
    pub const MAX_MESSAGE_CHARS: usize = 200;
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.at {
            Some(at) => write!(f, "{at}: {}", self.kind),
            None => write!(f, "{}", self.kind),
        }
    }
}

/// A limit a running script hit (ADR-0036, D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Limit {
    /// The operations one hook may run.
    OperationsPerHook,
    /// The operations a script may run in one second, over all its hooks.
    OperationsPerSecond,
    /// How long one hook may run.
    HookTime,
    /// How long a script's hooks may run in one second, together.
    TimePerSecond,
    /// How deep a script's own functions call each other.
    CallDepth,
    /// How much of its thread's stack a hook may take: calls and
    /// expressions nested so deep that, in this build, they'd come near
    /// the stack's end before the call limit is reached.
    Stack,
    /// The longest text, in bytes.
    TextBytes,
    /// The most items an array holds.
    ArrayItems,
    /// The most entries a map holds.
    MapEntries,
    /// The most variables alive at once.
    Variables,
    /// The most entries the state holds.
    StateEntries,
    /// The most bytes the state holds.
    StateBytes,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OperationsPerHook => "the hook ran more operations than one hook may",
            Self::OperationsPerSecond => "the script ran more operations than it may in one second",
            Self::HookTime => "the hook ran longer than one hook may",
            Self::TimePerSecond => "the script's hooks ran longer than they may in one second",
            Self::CallDepth => "the script's functions called each other too deeply",
            Self::Stack => "the script's calls and expressions nested deeper than a hook may",
            Self::TextBytes => "a text grew longer than a text may be",
            Self::ArrayItems => "an array grew larger than an array may be",
            Self::MapEntries => "a map grew larger than a map may be",
            Self::Variables => "more variables were alive at once than may be",
            Self::StateEntries => "the state holds more entries than it may",
            Self::StateBytes => "the state holds more bytes than it may",
        })
    }
}

/// A mistake in how a script used the API that no value could excuse: it
/// stops the script with its line, as a misspelled method does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Misuse {
    /// `chat.send` with a key the mode doesn't declare.
    UnknownMessage,
    /// `chat.expect` with a key that is no declared pattern's.
    UnknownPattern,
    /// `chat.expect` for a pattern declared to come at any time.
    NotAReplyPattern,
    /// A hold of a key that only taps: drop, drop a stack, swap.
    TapOnly,
    /// `hold_until_used` on anything but the right button.
    RightButtonOnly,
    /// `sign.write` with a key the mode doesn't declare.
    UnknownSign,
    /// `clock.local` with a name that is no time zone of the IANA
    /// database compiled in (Plan.md P16.21).
    UnknownTimeZone,
    /// A decimal mark that is neither `"."` nor `","`.
    UnknownDecimalMark,
    /// A member of API level 2 (`bot.name`, `number`) in a level-1 script
    /// (ADR-0036, D13).
    NeedsLevel2,
}

impl fmt::Display for Misuse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownMessage => "the mode declares no message with this key",
            Self::UnknownPattern => "the mode declares no chat pattern with this key",
            Self::NotAReplyPattern => {
                "this chat pattern may come at any time; only a reply is expected"
            }
            Self::TapOnly => "this key can only be tapped",
            Self::RightButtonOnly => "only the right button is held until the item is used",
            Self::UnknownSign => "the mode declares no sign with this key",
            Self::UnknownTimeZone => "there is no time zone of this name",
            Self::UnknownDecimalMark => r#"a decimal mark is "." or ",""#,
            Self::NeedsLevel2 => "this needs script API level 2; this mode's is 1",
        })
    }
}

/// Why a script stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptErrorKind {
    /// It hit a limit.
    #[error("{0}")]
    Limit(Limit),
    /// It replaced its state with something that isn't a map.
    #[error("`state` was replaced by something that isn't a map")]
    StateNotAMap,
    /// Its state holds something that isn't plain data: a function, a
    /// closure or a game object.
    #[error("`state` holds something that isn't a number, a text, true, false, an array or a map")]
    StateNotPlain,
    /// It used its state inside a function that a call on the state itself
    /// was running, such as a function handed to a method of a list in
    /// the state.
    #[error("`state` was used inside a function that a call on `state` itself was running")]
    StateInUse,
    /// It used the API in a way no value could excuse.
    #[error("{0}")]
    Misuse(Misuse),
    /// It failed where it ran: Rhai's own message, one line of at most
    /// [`ScriptError::MAX_MESSAGE_CHARS`] characters.
    #[error("{message}")]
    Failed {
        /// What Rhai says went wrong.
        message: String,
    },
}

/// A script that stopped: why, in which hook, and where when Rhai says
/// (ADR-0036, D3). A limit has no place, so the hook names it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ScriptError {
    /// Why it stopped.
    pub kind: ScriptErrorKind,
    /// The hook that was running.
    pub hook: Hook,
    /// Where it stopped, when the place is known.
    pub at: Option<SourcePosition>,
}

impl ScriptError {
    /// The longest message of Rhai's own that an error carries, in
    /// characters.
    pub const MAX_MESSAGE_CHARS: usize = 200;
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.at {
            Some(at) => write!(f, "{} at {at}: {}", self.hook, self.kind),
            None => write!(f, "{}: {}", self.hook, self.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compile_error_reads_with_its_place_first() {
        let error = CompileError {
            kind: CompileErrorKind::UndefinedVariable {
                name: "kees".to_owned(),
            },
            at: Some(SourcePosition { line: 2, column: 3 }),
        };

        assert_eq!(error.to_string(), "line 2, column 3: `kees` isn't defined");
    }

    #[test]
    fn a_compile_error_without_a_place_reads_alone() {
        let error = CompileError {
            kind: CompileErrorKind::TopLevelStatement,
            at: None,
        };

        assert_eq!(
            error.to_string(),
            "a script holds only functions; nothing outside a function ever runs"
        );
    }

    #[test]
    fn a_hook_with_the_wrong_parameters_says_how_many_it_takes() {
        let kind = CompileErrorKind::HookParameters {
            hook: Hook::Join,
            found: 0,
        };

        assert_eq!(kind.to_string(), "`on_join` takes 1 parameters, not 0");
    }

    #[test]
    fn the_limits_in_messages_are_fleet_cores() {
        assert_eq!(
            CompileErrorKind::TooManyFunctions.to_string(),
            "the script defines more than 64 functions"
        );
        assert_eq!(
            CompileErrorKind::ExpressionTooDeep.to_string(),
            "an expression is nested more than 32 deep"
        );
    }

    #[test]
    fn a_script_error_names_its_hook_and_its_place() {
        let placed = ScriptError {
            kind: ScriptErrorKind::Misuse(Misuse::UnknownMessage),
            hook: Hook::Timer,
            at: Some(SourcePosition { line: 4, column: 9 }),
        };
        let limit = ScriptError {
            kind: ScriptErrorKind::Limit(Limit::OperationsPerHook),
            hook: Hook::Join,
            at: None,
        };

        assert_eq!(
            placed.to_string(),
            "on_timer at line 4, column 9: the mode declares no message with this key"
        );
        assert_eq!(
            limit.to_string(),
            "on_join: the hook ran more operations than one hook may"
        );
    }

    #[test]
    fn every_limit_and_misuse_has_its_own_text() {
        let limits = [
            Limit::OperationsPerHook,
            Limit::OperationsPerSecond,
            Limit::CallDepth,
            Limit::Stack,
            Limit::TextBytes,
            Limit::ArrayItems,
            Limit::MapEntries,
            Limit::Variables,
            Limit::StateEntries,
            Limit::StateBytes,
        ];
        let misuses = [
            Misuse::UnknownMessage,
            Misuse::UnknownPattern,
            Misuse::NotAReplyPattern,
            Misuse::TapOnly,
            Misuse::RightButtonOnly,
        ];
        let mut texts: Vec<String> = limits
            .iter()
            .map(ToString::to_string)
            .chain(misuses.iter().map(ToString::to_string))
            .collect();
        let count = texts.len();
        texts.sort();
        texts.dedup();

        assert_eq!(texts.len(), count);
        assert!(texts.iter().all(|text| !text.is_empty()));
    }
}
