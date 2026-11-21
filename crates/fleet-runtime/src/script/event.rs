//! [`ScriptEvent`]: what a bot's script tells the app: its status, its error
//! with the line, and its log (Plan.md P15.18; ADR-0036, D3; ADR-0037, C7).
//!
//! **Everything here that is text is the script's own or a declaration's:**
//! a log line the script wrote, Rhai's message about the script, a declared
//! message as it was sent, a pattern's key. It's plain text from end to end,
//! bounded where fleet-script bounds it (a line at 200 characters, an
//! error's message at 200), and it's logged through `tracing` only at
//! `debug`, like chat.

use core::fmt;

use fleet_core::chat::ChatMessage;
use fleet_core::script::{Captured, ChatMatch, Ident};
use fleet_script::{CompileError, Refused, ScriptError, ScriptErrorKind};

use crate::chat::{ChatError, ChatFailure};

/// Something a bot's script reports, in a
/// [`FleetEvent`](crate::FleetEvent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptEvent {
    /// Where the script is in its life. It comes when a script starts, when
    /// it stops, and once at every join, so a subscriber that missed one
    /// hears it again.
    Status(ScriptStatus),
    /// A line of the script's log.
    Log(ScriptLog),
}

/// Where a bot's script is in its life.
///
/// A script that isn't [`Running`](Self::Running) runs nothing: the bot
/// stays online and idle, and the script starts again at the bot's next
/// Start or when its mode is changed or saved (ADR-0036, D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptStatus {
    /// Its hooks run.
    Running,
    /// It failed or hit a limit: why, in which hook, and at which line when
    /// the place is known.
    Stopped(ScriptError),
    /// It isn't a program this agent's engine takes, so it never ran.
    Refused(CompileError),
    /// The runtime itself failed while one of its hooks ran. That's a bug
    /// of ours, not the script's; the script is stopped all the same, so
    /// the bot doesn't crash again at its next join.
    Crashed,
}

impl ScriptStatus {
    /// A short name for a metric's label: why a script isn't running.
    /// `None` for one that runs.
    #[must_use]
    pub const fn stop_label(&self) -> Option<&'static str> {
        match self {
            Self::Running => None,
            Self::Stopped(error) => Some(match error.kind {
                ScriptErrorKind::Limit(_) => "limit",
                ScriptErrorKind::StateNotAMap
                | ScriptErrorKind::StateNotPlain
                | ScriptErrorKind::StateInUse => "state",
                ScriptErrorKind::Misuse(_) => "misuse",
                ScriptErrorKind::Failed { .. } => "error",
            }),
            Self::Refused(_) => Some("refused"),
            Self::Crashed => Some("crashed"),
        }
    }
}

/// Why a declared message a script asked for wasn't sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotSent {
    /// The bot's chat queue refused it at once: it was full, or the bot's
    /// chat rate was used up.
    Refused(ChatError),
    /// It was queued, and the session didn't send it.
    Failed(ChatFailure),
}

impl fmt::Display for NotSent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(error) => error.fmt(f),
            Self::Failed(failure) => failure.fmt(f),
        }
    }
}

/// A value a chat pattern read, as the log shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedValue {
    /// The capture's name.
    pub name: Ident,
    /// The value: a number as written out, or the pattern's own word.
    pub value: String,
}

/// One line of a script's log (ADR-0037, C7).
///
/// [`Line`](Self::Line) is what the script wrote itself, at most 20 a
/// minute. Every other line is written by the runtime about the script's
/// chat. What a script was refused is written at most 20 times a minute
/// too; the others need no limit of their own, since the bot's chat rate
/// and a pattern's one match a second bound them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptLog {
    /// A line the script wrote with `log.info`.
    Line(String),
    /// A declared message was handed to the session.
    Sent {
        /// The declared message's key.
        key: Ident,
        /// The finished text.
        message: ChatMessage,
    },
    /// A declared message wasn't sent.
    NotSent {
        /// The declared message's key.
        key: Ident,
        /// Why not.
        reason: NotSent,
    },
    /// A reply's window opened: its command was handed to the session.
    WindowOpened {
        /// The reply pattern's key.
        pattern: Ident,
    },
    /// A window ran out without its reply.
    WindowExpired {
        /// The reply pattern's key.
        pattern: Ident,
    },
    /// A window never opened: its command wasn't sent.
    WindowDropped {
        /// The reply pattern's key.
        pattern: Ident,
    },
    /// A pattern matched a line of the server's, and `on_chat` runs.
    Matched {
        /// The pattern's key.
        pattern: Ident,
        /// What it read, in declared order.
        values: Vec<LoggedValue>,
    },
    /// `chat.send` or `chat.expect` was refused before anything was queued:
    /// for which reason, and how often in one hook.
    Refused(Refused),
    /// `on_stop` failed. The script ended all the same.
    StopFailed(ScriptError),
}

impl ScriptLog {
    /// A pattern's match as a log line.
    #[must_use]
    pub fn matched(found: &ChatMatch) -> Self {
        Self::Matched {
            pattern: found.key.clone(),
            values: found
                .values
                .iter()
                .map(|(name, value)| LoggedValue {
                    name: name.clone(),
                    value: match value {
                        Captured::Integer(number) => number.to_string(),
                        Captured::Amount(number) => number.to_string(),
                        Captured::Word(word) => word.as_str().to_owned(),
                    },
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::script::{Hook, Word};
    use fleet_script::{Limit, Misuse, SourcePosition};

    use super::*;

    fn stopped(kind: ScriptErrorKind) -> ScriptStatus {
        ScriptStatus::Stopped(ScriptError {
            kind,
            hook: Hook::Timer,
            at: None,
        })
    }

    #[test]
    fn every_way_a_script_isnt_running_has_its_label() {
        let refused = ScriptStatus::Refused(CompileError {
            kind: fleet_script::CompileErrorKind::Import,
            at: Some(SourcePosition { line: 1, column: 1 }),
        });

        assert_eq!(ScriptStatus::Running.stop_label(), None);
        assert_eq!(
            stopped(ScriptErrorKind::Limit(Limit::HookTime)).stop_label(),
            Some("limit")
        );
        assert_eq!(
            stopped(ScriptErrorKind::StateNotAMap).stop_label(),
            Some("state")
        );
        assert_eq!(
            stopped(ScriptErrorKind::StateNotPlain).stop_label(),
            Some("state")
        );
        assert_eq!(
            stopped(ScriptErrorKind::StateInUse).stop_label(),
            Some("state")
        );
        assert_eq!(
            stopped(ScriptErrorKind::Misuse(Misuse::TapOnly)).stop_label(),
            Some("misuse")
        );
        assert_eq!(
            stopped(ScriptErrorKind::Failed {
                message: "Function not found".to_owned()
            })
            .stop_label(),
            Some("error")
        );
        assert_eq!(refused.stop_label(), Some("refused"));
        assert_eq!(ScriptStatus::Crashed.stop_label(), Some("crashed"));
    }

    #[test]
    fn a_match_is_logged_with_each_value_written_out() {
        let name = |text: &str| Ident::try_from(text).unwrap();
        let found = ChatMatch {
            key: name("balance"),
            values: vec![
                (name("amount"), Captured::Amount(1250.5)),
                (name("count"), Captured::Integer(-3)),
                (
                    name("state"),
                    Captured::Word(Word::try_from("open").unwrap()),
                ),
            ],
        };

        let ScriptLog::Matched { pattern, values } = ScriptLog::matched(&found) else {
            panic!("a match is logged as one");
        };

        assert_eq!(pattern.as_str(), "balance");
        let shown: Vec<(&str, &str)> = values
            .iter()
            .map(|value| (value.name.as_str(), value.value.as_str()))
            .collect();
        assert_eq!(
            shown,
            [("amount", "1250.5"), ("count", "-3"), ("state", "open")]
        );
    }

    #[test]
    fn why_a_message_wasnt_sent_reads_as_the_queues_or_the_sessions_own_text() {
        assert_eq!(
            NotSent::Refused(ChatError::RateLimited).to_string(),
            "the bot's chat rate limit is reached"
        );
        assert_eq!(
            NotSent::Failed(ChatFailure::ChatUnavailable).to_string(),
            "the server requires signed chat, and the session can't sign it"
        );
    }
}
