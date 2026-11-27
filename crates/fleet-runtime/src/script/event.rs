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
use fleet_core::script::{
    Captured, ChatMatch, Ident, LogKind, ProblemKind, Refusal, ReportText, ScriptLogLine,
    ScriptPlace, ScriptProblem, ScriptState,
};
use fleet_script::{Asked, CompileError, Refused, ScriptError, ScriptErrorKind, SourcePosition};

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

    /// The status as the agent reports it to the server (Plan.md P15.20):
    /// fleet-core's report, every text in it cleaned and bounded. A
    /// refusal is the per-bot refusal of ADR-0036's D13.
    #[must_use]
    pub fn report(&self) -> ScriptState {
        match self {
            Self::Running => ScriptState::Running,
            Self::Stopped(error) => ScriptState::Stopped(stopped(error)),
            Self::Refused(error) => ScriptState::Refused(refused(error)),
            Self::Crashed => ScriptState::Crashed,
        }
    }
}

/// A place in fleet-core's terms; `None` for one that doesn't count from 1.
fn place(at: Option<SourcePosition>) -> Option<ScriptPlace> {
    at.and_then(|at| ScriptPlace::new(at.line, at.column))
}

/// A stopped script's problem: its kind, its hook, its place and the
/// engine's message.
fn stopped(error: &ScriptError) -> ScriptProblem {
    let kind = match error.kind {
        ScriptErrorKind::Limit(_) => ProblemKind::Limit,
        ScriptErrorKind::StateNotAMap
        | ScriptErrorKind::StateNotPlain
        | ScriptErrorKind::StateInUse => ProblemKind::State,
        ScriptErrorKind::Misuse(_) => ProblemKind::Misuse,
        ScriptErrorKind::Failed { .. } => ProblemKind::Error,
    };
    ScriptProblem {
        kind,
        hook: Some(error.hook),
        place: place(error.at),
        message: ReportText::clean(&error.kind.to_string()),
    }
}

/// A refused script's problem: no hook ran.
fn refused(error: &CompileError) -> ScriptProblem {
    ScriptProblem {
        kind: ProblemKind::Compile,
        hook: None,
        place: place(error.at),
        message: ReportText::clean(&error.kind.to_string()),
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
    /// Something the script did in a screen or a sign editor didn't go as
    /// asked: the session didn't send it after all, or an action's fate
    /// came back other than `ok` (ADR-0036, M4; Plan.md P16.13). Every one
    /// is written: the click limit bounds them.
    Menu {
        /// What it was: a click and the like, a close or the inventory, a
        /// sign's answer.
        asked: Asked,
        /// The slot a click clicked; `None` for anything else.
        slot: Option<u16>,
        /// Why it wasn't sent, or what came instead of what was predicted.
        reason: Refusal,
        /// Whether it went out: a fate is of something sent.
        sent: bool,
    },
}

/// What `reason` means, as a part of a sentence: the reference's line,
/// lowercase and without its full stop.
fn meaning(reason: Refusal) -> String {
    let meaning = fleet_script::reason_meaning(reason);
    let meaning = meaning.strip_suffix('.').unwrap_or(meaning);
    let mut chars = meaning.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

impl ScriptLog {
    /// The line as the agent reports it to the server (Plan.md P15.23):
    /// its kind, and the agent's wording, cleaned and bounded.
    ///
    /// **A declared message is named by its key alone:** its text and its
    /// values are a command's arguments, which only a viewer with Manage
    /// reads (ADR-0020, S6, S9), and a script's log reaches every viewer of
    /// the bot. A match's values are what the server printed, which the
    /// bot's chat shows them already.
    #[must_use]
    pub fn report(&self) -> ScriptLogLine {
        let (kind, text) = match self {
            Self::Line(line) => (LogKind::Line, line.clone()),
            Self::Sent { key, message: _ } => (LogKind::Sent, format!("sent `{key}`")),
            Self::NotSent { key, reason } => {
                (LogKind::NotSent, format!("`{key}` wasn't sent: {reason}"))
            }
            Self::WindowOpened { pattern } => {
                (LogKind::WindowOpened, format!("waiting for `{pattern}`"))
            }
            Self::WindowExpired { pattern } => (
                LogKind::WindowExpired,
                format!("`{pattern}` didn't come in time"),
            ),
            Self::WindowDropped { pattern } => (
                LogKind::WindowDropped,
                format!("not waiting for `{pattern}`: its command wasn't sent"),
            ),
            Self::Matched { pattern, values } => {
                let values: Vec<String> = values
                    .iter()
                    .map(|value| format!("{} = {}", value.name, value.value))
                    .collect();
                let text = if values.is_empty() {
                    format!("`{pattern}` matched")
                } else {
                    format!("`{pattern}` matched: {}", values.join(", "))
                };
                (LogKind::Matched, text)
            }
            Self::Refused(refused) => (
                // A refusal in a screen or a sign editor is a click line
                // (Plan.md P16.16); `refused` is chat's.
                match refused.asked {
                    Asked::Click | Asked::Screen | Asked::Sign => LogKind::Click,
                    _ => LogKind::Refused,
                },
                format!(
                    "{} refused ({}), {} time{}",
                    refused.asked.as_str(),
                    refused.reason.as_str(),
                    refused.times,
                    if refused.times == 1 { "" } else { "s" }
                ),
            ),
            Self::StopFailed(error) => (LogKind::StopFailed, format!("on_stop failed: {error}")),
            // A click line (Plan.md P16.16): what was clicked and its fate,
            // in words a person reads, with the fate's name a script reads.
            Self::Menu {
                asked,
                slot,
                reason,
                sent,
            } => {
                let what = match (asked, slot) {
                    (Asked::Click, Some(slot)) => format!("a click on slot {slot}"),
                    (Asked::Click, None) => "a screen's button, trade or name".to_owned(),
                    (Asked::Sign, _) => "the sign's answer".to_owned(),
                    _ => "closing the screen or opening the inventory".to_owned(),
                };
                let text = if *sent {
                    match reason {
                        Refusal::Unclear => {
                            format!("{what} may have worked: the script checks the next screen")
                        }
                        Refusal::Rejected => format!("{what} was put back by the server"),
                        Refusal::Replaced => format!(
                            "{what} wasn't answered: its screen closed or another opened first"
                        ),
                        Refusal::Unanswered => format!(
                            "{what} got no answer from the server, which may or may not have done it"
                        ),
                        _ => format!("{what} came back"),
                    }
                } else {
                    format!("{what} wasn't sent: {}", meaning(*reason))
                };
                (LogKind::Click, format!("{text} ({reason})"))
            }
        };
        ScriptLogLine {
            kind,
            text: ReportText::clean(&text),
        }
    }

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

    // --- What the agent reports (Plan.md P15.20, P15.23) ---

    #[test]
    fn a_status_is_reported_with_its_kind_hook_place_and_the_engines_message() {
        let stop = ScriptStatus::Stopped(ScriptError {
            kind: ScriptErrorKind::Failed {
                message: "Function not found: jmp ()".to_owned(),
            },
            hook: Hook::Timer,
            at: Some(SourcePosition { line: 4, column: 7 }),
        });
        let limit = stopped(ScriptErrorKind::Limit(Limit::HookTime));
        let refusal = ScriptStatus::Refused(CompileError {
            kind: fleet_script::CompileErrorKind::UndefinedVariable {
                name: "kees".to_owned(),
            },
            at: Some(SourcePosition { line: 2, column: 3 }),
        });

        assert_eq!(ScriptStatus::Running.report(), ScriptState::Running);
        assert_eq!(ScriptStatus::Crashed.report(), ScriptState::Crashed);
        assert_eq!(
            stop.report(),
            ScriptState::Stopped(ScriptProblem {
                kind: ProblemKind::Error,
                hook: Some(Hook::Timer),
                place: ScriptPlace::new(4, 7),
                message: ReportText::clean("Function not found: jmp ()"),
            })
        );
        let ScriptState::Stopped(problem) = limit.report() else {
            panic!("a limit stops the script");
        };
        assert_eq!((problem.kind, problem.place), (ProblemKind::Limit, None));
        assert_eq!(
            refusal.report(),
            ScriptState::Refused(ScriptProblem {
                kind: ProblemKind::Compile,
                hook: None,
                place: ScriptPlace::new(2, 3),
                message: ReportText::clean("`kees` isn't defined"),
            })
        );
    }

    #[test]
    fn every_way_a_script_stops_is_reported_as_its_kind() {
        let kinds = [
            (ScriptErrorKind::Limit(Limit::HookTime), ProblemKind::Limit),
            (ScriptErrorKind::StateNotAMap, ProblemKind::State),
            (ScriptErrorKind::StateNotPlain, ProblemKind::State),
            (ScriptErrorKind::StateInUse, ProblemKind::State),
            (
                ScriptErrorKind::Misuse(Misuse::TapOnly),
                ProblemKind::Misuse,
            ),
        ];
        for (kind, expected) in kinds {
            let ScriptState::Stopped(problem) = stopped(kind).report() else {
                panic!("a stopped script");
            };
            assert_eq!(problem.kind, expected);
        }
    }

    #[test]
    fn a_sent_message_is_reported_by_its_key_never_its_text_or_values() {
        let sent = ScriptLog::Sent {
            key: Ident::try_from("pay").unwrap(),
            message: ChatMessage::try_from("/pay AfkMain 1250.50").unwrap(),
        };

        let line = sent.report();

        assert_eq!(line.kind, LogKind::Sent);
        assert_eq!(line.text.as_str(), "sent `pay`");
    }

    #[test]
    fn every_line_is_reported_as_its_kind_and_cleaned() {
        let key = Ident::try_from("balance").unwrap();
        let found = ScriptLog::matched(&ChatMatch {
            key: key.clone(),
            values: vec![(Ident::try_from("amount").unwrap(), Captured::Amount(1250.5))],
        });
        let lines = [
            (
                ScriptLog::Line("§cdone\ntoday".to_owned()),
                LogKind::Line,
                "done | today",
            ),
            (
                ScriptLog::NotSent {
                    key: key.clone(),
                    reason: NotSent::Refused(ChatError::RateLimited),
                },
                LogKind::NotSent,
                "`balance` wasn't sent: the bot's chat rate limit is reached",
            ),
            (
                ScriptLog::WindowOpened {
                    pattern: key.clone(),
                },
                LogKind::WindowOpened,
                "waiting for `balance`",
            ),
            (
                ScriptLog::WindowExpired {
                    pattern: key.clone(),
                },
                LogKind::WindowExpired,
                "`balance` didn't come in time",
            ),
            (
                ScriptLog::WindowDropped {
                    pattern: key.clone(),
                },
                LogKind::WindowDropped,
                "not waiting for `balance`: its command wasn't sent",
            ),
            (
                found,
                LogKind::Matched,
                "`balance` matched: amount = 1250.5",
            ),
            (
                ScriptLog::Refused(Refused {
                    asked: fleet_script::Asked::Send,
                    reason: fleet_core::script::Refusal::TooSoon,
                    times: 3,
                }),
                LogKind::Refused,
                "send refused (too_soon), 3 times",
            ),
        ];

        for (log, kind, text) in lines {
            let line = log.report();
            assert_eq!((line.kind, line.text.as_str()), (kind, text));
        }
        let failed = ScriptLog::StopFailed(ScriptError {
            kind: ScriptErrorKind::Misuse(Misuse::UnknownMessage),
            hook: Hook::Stop,
            at: None,
        });
        assert_eq!(failed.report().kind, LogKind::StopFailed);
        assert!(
            failed
                .report()
                .text
                .as_str()
                .starts_with("on_stop failed: ")
        );
    }

    /// A screen's line as the agent reports it.
    fn menu_line(asked: Asked, slot: Option<u16>, reason: Refusal, sent: bool) -> ScriptLogLine {
        ScriptLog::Menu {
            asked,
            slot,
            reason,
            sent,
        }
        .report()
    }

    #[rstest::rstest]
    #[case::unclear(
        Refusal::Unclear,
        "a click on slot 13 may have worked: the script checks the next screen (unclear)"
    )]
    #[case::rejected(
        Refusal::Rejected,
        "a click on slot 13 was put back by the server (rejected)"
    )]
    #[case::replaced(
        Refusal::Replaced,
        "a click on slot 13 wasn't answered: its screen closed or another opened first (replaced)"
    )]
    #[case::unanswered(
        Refusal::Unanswered,
        "a click on slot 13 got no answer from the server, which may or may not have done it (unanswered)"
    )]
    fn a_clicks_fate_is_a_click_line_in_words_with_its_name(
        #[case] fate: Refusal,
        #[case] text: &str,
    ) {
        let line = menu_line(Asked::Click, Some(13), fate, true);

        assert_eq!((line.kind, line.text.as_str()), (LogKind::Click, text));
    }

    #[test]
    fn what_wasnt_sent_is_a_click_line_with_the_reasons_meaning() {
        let cases = [
            (
                menu_line(Asked::Click, Some(0), Refusal::SlotChanged, false),
                "a click on slot 0 wasn't sent: a slot the click touches holds something else than the hook or the slot saw (slot_changed)",
            ),
            (
                menu_line(Asked::Click, None, Refusal::NoMenu, false),
                "a screen's button, trade or name wasn't sent: no screen is open (no_menu)",
            ),
            (
                menu_line(Asked::Sign, None, Refusal::NoSign, false),
                "the sign's answer wasn't sent: no sign editor waits for an answer (no_sign)",
            ),
            (
                menu_line(Asked::Screen, None, Refusal::NotInWorld, false),
                "closing the screen or opening the inventory wasn't sent: the bot is between worlds, or its session is ending (not_in_world)",
            ),
        ];

        for (line, text) in cases {
            assert_eq!((line.kind, line.text.as_str()), (LogKind::Click, text));
        }
    }

    #[test]
    fn a_hooks_refusal_in_a_screen_is_a_click_line_and_chats_stays_a_refusal() {
        let refused = |asked| {
            ScriptLog::Refused(Refused {
                asked,
                reason: Refusal::TooSoon,
                times: 2,
            })
            .report()
            .kind
        };

        for asked in [Asked::Click, Asked::Screen, Asked::Sign] {
            assert_eq!(refused(asked), LogKind::Click, "{asked:?}");
        }
        for asked in [Asked::Send, Asked::Expect] {
            assert_eq!(refused(asked), LogKind::Refused, "{asked:?}");
        }
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
