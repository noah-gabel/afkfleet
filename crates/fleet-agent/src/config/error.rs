//! Why the agent's config is invalid: its kinds of problems, and its
//! names for fleet-startup's generic error types (ADR-0015).

use fleet_core::disconnect::ConflictTextsError;
use fleet_core::mode::UnknownPresetError;
use fleet_core::resilience::{CircuitPolicyError, RetryPolicyError};
use fleet_core::value::{AgentNameError, McUsernameError, ServerAddressError};
use fleet_startup::config::{LogFilterError, UnknownLogFormatError};

/// Why [`load`](super::load) failed.
pub type ConfigError = fleet_startup::config::ConfigError<ProblemKind>;

/// Every problem the agent's validation found, sorted by key.
pub type Problems = fleet_startup::config::Problems<ProblemKind>;

/// One problem, at one key.
pub type Problem = fleet_startup::config::Problem<ProblemKind>;

/// What's wrong with a key. No message echoes the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProblemKind {
    /// A required key is missing.
    #[error("missing")]
    Missing,
    /// A required text is empty.
    #[error("must not be empty")]
    Empty,
    /// A number outside its range, with garde's message (`lower than 5`).
    #[error("{message}")]
    OutOfRange {
        /// garde's message, which names the bound, never the value.
        message: String,
    },
    /// A path that must be absolute isn't.
    #[error("must be an absolute path")]
    NotAbsolute,
    /// An invalid agent name.
    #[error(transparent)]
    AgentName(AgentNameError),
    /// An invalid Minecraft name.
    #[error(transparent)]
    Username(McUsernameError),
    /// An invalid server address.
    #[error(transparent)]
    Server(ServerAddressError),
    /// An unknown mode name.
    #[error(transparent)]
    Mode(UnknownPresetError),
    /// Invalid conflict texts.
    #[error(transparent)]
    ConflictTexts(ConflictTextsError),
    /// Retry delays that don't fit together.
    #[error(transparent)]
    Retry(RetryPolicyError),
    /// Circuit-breaker settings that don't fit together.
    #[error(transparent)]
    Circuit(CircuitPolicyError),
    /// An unknown log format.
    #[error(transparent)]
    LogFormat(UnknownLogFormatError),
    /// An invalid log filter.
    #[error(transparent)]
    LogFilter(LogFilterError),
    /// Neither `[standalone]` nor `[control_plane]` is there.
    #[error("the config needs [standalone] or [control_plane]")]
    NoMode,
    /// Both `[standalone]` and `[control_plane]` are there.
    #[error("[standalone] and [control_plane] can't be used together")]
    BothModes,
    /// `[standalone]` lists no bots.
    #[error("[standalone] needs at least one bot")]
    NoBots,
    /// `[standalone]` lists more bots than `[runtime] max_bots`.
    #[error("{count} bots, more than runtime.max_bots ({max})")]
    TooManyBots {
        /// How many bots are listed.
        count: usize,
        /// The limit.
        max: usize,
    },
    /// Two bots use the same account.
    #[error("clashes with standalone.bots[{with}] (names compare ignoring case)")]
    Clash {
        /// The position of the earlier entry with the same account.
        with: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_startup::config::KeyPath;

    #[test]
    fn problems_are_listed_one_per_line_and_sorted() {
        let mut problems = Problems::default();
        problems.push(
            KeyPath::root().key("runtime").key("max_bots"),
            ProblemKind::Missing,
        );
        problems.push(KeyPath::root(), ProblemKind::NoMode);
        problems.push(KeyPath::root().key("name"), ProblemKind::Empty);
        let problems = problems.sorted();

        assert_eq!(problems.len(), 3);
        assert!(!problems.is_empty());
        assert!(problems.any_under(&KeyPath::root().key("runtime")));
        assert!(!problems.any_under(&KeyPath::root().key("retry")));
        assert_eq!(
            ConfigError::Invalid(problems).to_string(),
            "the config is invalid:\n  \
             - the config needs [standalone] or [control_plane]\n  \
             - name: must not be empty\n  \
             - runtime.max_bots: missing"
        );
    }
}
