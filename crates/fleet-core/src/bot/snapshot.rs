//! [`BotSnapshot`]: a bot's status at one point in time.

use core::num::NonZeroU32;
use core::time::Duration;

use chrono::{DateTime, Utc};

use super::BotState;
use crate::disconnect::DisconnectReason;
use crate::id::BotId;
use crate::time;

/// A bot's observable status (Plan.md P4.1; ADR-0013).
///
/// The bot's actor publishes a new snapshot whenever its state changes, so
/// nothing in it goes stale between changes: the attempt and the uptime are
/// derived from the state when they're read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotSnapshot {
    /// The bot.
    pub bot_id: BotId,
    /// Where the bot is in its lifecycle.
    pub state: BotState,
    /// When the bot entered `state`.
    pub since: DateTime<Utc>,
    /// Why the bot's last session ended, if one has: a kick, a failed
    /// connect, a crash or a watchdog trip.
    pub last_disconnect: Option<DisconnectReason>,
}

impl BotSnapshot {
    /// Returns the connection attempt the bot is on, counted from 1. In
    /// `Backoff` it's the attempt that failed. `None` while the bot isn't
    /// trying to connect: `Stopped`, `Paused`, `Failed` or `Stopping`.
    #[must_use]
    pub const fn attempt(&self) -> Option<NonZeroU32> {
        match self.state {
            BotState::AwaitingSession { attempt, .. }
            | BotState::Connecting { attempt, .. }
            | BotState::Online { attempt, .. }
            | BotState::Backoff { attempt } => Some(attempt),
            BotState::Stopped
            | BotState::Paused { .. }
            | BotState::Failed { .. }
            | BotState::Stopping { .. } => None,
        }
    }

    /// Returns how long the bot has been online at `now`, or `None` if it
    /// isn't `Online`. It's zero if the clock went backwards.
    #[must_use]
    pub fn uptime(&self, now: DateTime<Utc>) -> Option<Duration> {
        match self.state {
            BotState::Online { since, .. } => Some(time::elapsed(since, now)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::{FailReason, PauseReason};
    use crate::disconnect::ConflictKind;
    use rstest::rstest;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn n(attempt: u32) -> NonZeroU32 {
        NonZeroU32::new(attempt).unwrap()
    }

    fn snapshot(state: BotState) -> BotSnapshot {
        BotSnapshot {
            bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            state,
            since: at(1_000),
            last_disconnect: None,
        }
    }

    #[rstest]
    #[case::awaiting_session(BotState::AwaitingSession { attempt: n(2), fresh: false }, Some(n(2)))]
    #[case::connecting(BotState::Connecting { attempt: n(3), auth_retried: true }, Some(n(3)))]
    #[case::online(BotState::Online { since: at(1_000), attempt: n(4) }, Some(n(4)))]
    #[case::backoff(BotState::Backoff { attempt: n(5) }, Some(n(5)))]
    #[case::stopped(BotState::Stopped, None)]
    #[case::stopping(BotState::Stopping { restart: true }, None)]
    #[case::paused(
        BotState::Paused { reason: PauseReason::Conflict { kind: ConflictKind::DuplicateLogin } },
        None
    )]
    #[case::failed(BotState::Failed { reason: FailReason::Auth }, None)]
    fn attempt_comes_from_the_state(#[case] state: BotState, #[case] expected: Option<NonZeroU32>) {
        assert_eq!(snapshot(state).attempt(), expected);
    }

    #[test]
    fn uptime_counts_from_the_join() {
        let online = snapshot(BotState::Online {
            since: at(1_000),
            attempt: n(1),
        });

        assert_eq!(online.uptime(at(1_090)), Some(Duration::from_secs(90)));
    }

    #[test]
    fn uptime_is_zero_when_the_clock_went_backwards() {
        let online = snapshot(BotState::Online {
            since: at(1_000),
            attempt: n(1),
        });

        assert_eq!(online.uptime(at(900)), Some(Duration::ZERO));
    }

    #[rstest]
    #[case::stopped(BotState::Stopped)]
    #[case::connecting(BotState::Connecting { attempt: n(1), auth_retried: false })]
    #[case::backoff(BotState::Backoff { attempt: n(1) })]
    #[case::failed(BotState::Failed { reason: FailReason::CrashLoop })]
    fn uptime_is_none_unless_online(#[case] state: BotState) {
        assert_eq!(snapshot(state).uptime(at(2_000)), None);
    }
}
