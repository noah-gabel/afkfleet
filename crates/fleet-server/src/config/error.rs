//! Why the server's config is invalid: its kinds of problems, and its names
//! for fleet-startup's generic error types (ADR-0015).

use fleet_startup::config::{LogFilterError, UnknownLogFormatError};

/// Why [`load`](super::load) failed.
pub type ConfigError = fleet_startup::config::ConfigError<ProblemKind>;

/// Every problem the server's validation found, sorted by key.
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
    /// A number outside its range, with garde's message (`lower than 1`).
    #[error("{message}")]
    OutOfRange {
        /// garde's message, which names the bound, never the value.
        message: String,
    },
    /// A path that must be absolute isn't.
    #[error("must be an absolute path")]
    NotAbsolute,
    /// Not an IP address with a port.
    #[error("must be an IP address and a port, like 127.0.0.1:8080")]
    SocketAddress,
    /// Port 0, which would make the OS pick a port.
    #[error("port 0 isn't allowed: the server needs a fixed port")]
    PortZero,
    /// An unknown log format.
    #[error(transparent)]
    LogFormat(UnknownLogFormatError),
    /// An invalid log filter.
    #[error(transparent)]
    LogFilter(LogFilterError),
}

#[cfg(test)]
mod tests {
    use fleet_startup::config::KeyPath;

    use super::*;

    #[test]
    fn problems_are_listed_one_per_line_and_sorted() {
        let mut problems = Problems::default();
        problems.push(
            KeyPath::root().key("http").key("bind"),
            ProblemKind::PortZero,
        );
        problems.push(
            KeyPath::root().key("database").key("path"),
            ProblemKind::NotAbsolute,
        );
        let problems = problems.sorted();

        assert_eq!(
            ConfigError::Invalid(problems).to_string(),
            "the config is invalid:\n  \
             - database.path: must be an absolute path\n  \
             - http.bind: port 0 isn't allowed: the server needs a fixed port"
        );
    }
}
