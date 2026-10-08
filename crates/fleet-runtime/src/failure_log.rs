//! [`FailureLog`]: which level a failure in a session logs at.

use fleet_core::mc::SessionError;
use tracing::Level;

/// A kind of failure that a mode's step or the chat queue can run into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// A session call failed.
    Session(SessionError),
}

/// Remembers which kinds of failure a session has had, so the first of each
/// kind logs at `warn` and the rest at `debug` (ADR-0013). A session that
/// keeps failing the same way then warns once instead of on every step.
///
/// Each session gets a fresh log; the mode runner and the chat queue each
/// keep their own.
#[derive(Debug, Default)]
pub(crate) struct FailureLog {
    seen: Vec<FailureKind>,
}

impl FailureLog {
    /// Returns the level to log this failure at: [`Level::WARN`] the first
    /// time its kind comes up, [`Level::DEBUG`] after that.
    pub(crate) fn level(&mut self, kind: FailureKind) -> Level {
        if self.seen.contains(&kind) {
            Level::DEBUG
        } else {
            self.seen.push(kind);
            Level::WARN
        }
    }
}

/// Logs at `$level`, which is [`Level::WARN`] or [`Level::DEBUG`] (from
/// [`FailureLog::level`]).
macro_rules! warn_or_debug {
    ($level:expr, $($arg:tt)+) => {
        if $level == ::tracing::Level::WARN {
            ::tracing::warn!($($arg)+);
        } else {
            ::tracing::debug!($($arg)+);
        }
    };
}

pub(crate) use warn_or_debug;

#[cfg(test)]
mod tests {
    use super::*;

    const TIMED_OUT: FailureKind = FailureKind::Session(SessionError::TimedOut);
    const NOT_IN_WORLD: FailureKind = FailureKind::Session(SessionError::NotInWorld);
    const CLOSED: FailureKind = FailureKind::Session(SessionError::Closed);

    #[test]
    fn the_first_failure_of_a_kind_warns_and_the_rest_are_debug() {
        let mut log = FailureLog::default();

        assert_eq!(log.level(TIMED_OUT), Level::WARN);
        assert_eq!(log.level(TIMED_OUT), Level::DEBUG);
        assert_eq!(log.level(TIMED_OUT), Level::DEBUG);
    }

    #[test]
    fn each_kind_warns_once() {
        let mut log = FailureLog::default();

        let levels: Vec<_> = [TIMED_OUT, NOT_IN_WORLD, TIMED_OUT, CLOSED, NOT_IN_WORLD]
            .into_iter()
            .map(|kind| log.level(kind))
            .collect();

        assert_eq!(
            levels,
            [
                Level::WARN,
                Level::WARN,
                Level::DEBUG,
                Level::WARN,
                Level::DEBUG
            ]
        );
    }

    #[test]
    fn a_new_log_warns_again() {
        let mut first = FailureLog::default();
        assert_eq!(first.level(TIMED_OUT), Level::WARN);

        let mut next_session = FailureLog::default();

        assert_eq!(next_session.level(TIMED_OUT), Level::WARN);
    }
}
