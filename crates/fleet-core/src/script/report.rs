//! What an agent reports about a bot's script (Plan.md P15.20, P15.23;
//! ADR-0036, D3, D13; ADR-0037, C7): where the script is in its life
//! ([`ScriptState`]), the problem that stopped or refused it with its place
//! ([`ScriptProblem`]), and the lines of its log ([`ScriptLogLine`]).
//!
//! **Every text here is untrusted where it's read.** It's the script's own
//! (a line it logged), the engine's about the script (Rhai's message), or
//! the agent's wording of what the runtime did with its chat, and it
//! crosses from an agent to the server, which trusts nothing an agent says
//! (ADR-0019). So a text is a [`ReportText`]: cleaned as untrusted text,
//! one line, at most [`ReportText::MAX_CHARS`] characters, whatever was
//! sent. It's shown as plain text, never as markup.

use core::num::NonZeroU32;

use super::Hook;
use crate::text::sanitize_untrusted;

/// A text a script's report carries: cleaned as untrusted text (no
/// formatting codes, control, bidirectional or invisible characters; a line
/// break becomes ` | `) and cut at [`ReportText::MAX_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReportText(String);

impl ReportText {
    /// The most characters of a report's text: a script's log line and the
    /// engine's message are bounded there by the agent too (ADR-0040, E7,
    /// E9).
    pub const MAX_CHARS: usize = 200;

    /// `raw`, cleaned and cut. Cleaning twice gives the same text.
    #[must_use]
    pub fn clean(raw: &str) -> Self {
        Self(sanitize_untrusted(raw, Self::MAX_CHARS).text)
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a bot's script is in its life, as its agent reports it. A script
/// that isn't [`Running`](Self::Running) runs nothing: the bot stays online
/// and idle until its next Start or a change of its mode (ADR-0036, D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptState {
    /// Its hooks run.
    Running,
    /// It failed or hit a limit.
    Stopped(ScriptProblem),
    /// The agent's own engine refused it, so it never ran: the per-bot
    /// refusal of ADR-0036's D13, with the error and its line.
    Refused(ScriptProblem),
    /// The agent's runtime failed while one of its hooks ran.
    Crashed,
}

/// What stopped or refused a script, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptProblem {
    /// Its kind, from a closed list.
    pub kind: ProblemKind,
    /// The hook that ran, for a script that stopped; a limit has no place,
    /// so the hook names it.
    pub hook: Option<Hook>,
    /// Its place in the script, when the engine gave one.
    pub place: Option<ScriptPlace>,
    /// The engine's message about the script.
    pub message: ReportText,
}

/// A problem's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProblemKind {
    /// The script isn't a program the engine takes.
    Compile,
    /// A hook failed: an error of the script's own.
    Error,
    /// The script used the API in a way it may not, such as a key the mode
    /// doesn't declare.
    Misuse,
    /// A limit: operations, time, the stack, the size of a value.
    Limit,
    /// The script's state broke its rules.
    State,
}

impl ProblemKind {
    /// Every kind, in a fixed order.
    pub const ALL: [Self; 5] = [
        Self::Compile,
        Self::Error,
        Self::Misuse,
        Self::Limit,
        Self::State,
    ];

    /// Its stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compile => "compile",
            Self::Error => "error",
            Self::Misuse => "misuse",
            Self::Limit => "limit",
            Self::State => "state",
        }
    }
}

/// A place in a script: its line and column, each counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScriptPlace {
    /// The line.
    pub line: NonZeroU32,
    /// The column.
    pub column: NonZeroU32,
}

impl ScriptPlace {
    /// The place at `line` and `column`; `None` unless both count from 1.
    #[must_use]
    pub fn new(line: u32, column: u32) -> Option<Self> {
        Some(Self {
            line: NonZeroU32::new(line)?,
            column: NonZeroU32::new(column)?,
        })
    }
}

/// One line of a script's log, as its agent reports it (ADR-0037, C7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptLogLine {
    /// What the line is about.
    pub kind: LogKind,
    /// The line.
    pub text: ReportText,
}

/// What a line of a script's log is about. [`Line`](Self::Line) is the
/// script's own; every other kind is the runtime's about the script's chat,
/// its screens or its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogKind {
    /// A line the script wrote.
    Line,
    /// A declared message was handed to the session.
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
    /// that wasn't sent, or whose fate wasn't `ok` (ADR-0052, G6; Plan.md
    /// P16.16).
    Click,
}

impl LogKind {
    /// Every kind, in a fixed order.
    pub const ALL: [Self; 10] = [
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

    /// Its stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain("hello there", "hello there")]
    #[case::formatting_codes("§cred§r text", "red text")]
    #[case::a_line_break("one\ntwo", "one | two")]
    #[case::control_characters("a\u{7}b\u{1b}[31mc", "ab[31mc")]
    #[case::a_bidirectional_override("abc\u{202e}def", "abcdef")]
    #[case::an_invisible_character("pay\u{200b}me", "payme")]
    fn a_reports_text_is_cleaned_as_untrusted_text(#[case] raw: &str, #[case] clean: &str) {
        assert_eq!(ReportText::clean(raw).as_str(), clean);
    }

    #[test]
    fn a_reports_text_is_cut_at_200_characters() {
        let long = "é".repeat(500);

        let text = ReportText::clean(&long);

        assert_eq!(text.as_str().chars().count(), ReportText::MAX_CHARS);
    }

    #[test]
    fn a_place_counts_from_1() {
        assert_eq!(ScriptPlace::new(0, 1), None);
        assert_eq!(ScriptPlace::new(1, 0), None);
        let place = ScriptPlace::new(3, 7).unwrap();
        assert_eq!((place.line.get(), place.column.get()), (3, 7));
    }

    #[test]
    fn every_kind_has_its_own_stable_name() {
        let problems: HashSet<&str> = ProblemKind::ALL.iter().map(|kind| kind.as_str()).collect();
        let lines: HashSet<&str> = LogKind::ALL.iter().map(|kind| kind.as_str()).collect();

        assert_eq!(problems.len(), ProblemKind::ALL.len());
        assert_eq!(lines.len(), LogKind::ALL.len());
        assert_eq!(ProblemKind::Compile.as_str(), "compile");
        assert_eq!(LogKind::WindowOpened.as_str(), "window_opened");
    }

    proptest! {
        #[test]
        fn whatever_is_sent_a_text_is_one_bounded_clean_line(raw in any::<String>()) {
            let text = ReportText::clean(&raw);

            prop_assert!(text.as_str().chars().count() <= ReportText::MAX_CHARS);
            prop_assert!(!text.as_str().chars().any(char::is_control));
            prop_assert_eq!(ReportText::clean(text.as_str()), text);
        }
    }
}
