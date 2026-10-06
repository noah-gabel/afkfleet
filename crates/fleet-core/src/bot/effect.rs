//! [`Effect`]: what the actor does after a transition.

use core::num::NonZeroU32;

use super::{FailReason, PauseReason};

/// Something the bot's actor must do. A transition returns its effects in the
/// order the actor executes them (Plan.md Appendix E; ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effect {
    /// Ask for session credentials. Every request gets exactly one answer:
    /// `SessionReady` or `SessionUnavailable`. A request that times out is
    /// answered with `SessionUnavailable { retryable: true }`.
    RequestSession {
        /// Bypass any token cache, because the last session was rejected.
        fresh: bool,
    },
    /// Connect with the session credentials. Connecting ends with `Joined`,
    /// `ConnectFailed` or `Disconnected`; the adapter enforces a connect
    /// timeout.
    Connect,
    /// Tear the session down completely (ADR-0008 §10), then send
    /// `SessionClosed`.
    Disconnect,
    /// Wait [`RetryPolicy::delay`](crate::resilience::RetryPolicy::delay) for
    /// `attempt`, or longer while the circuit breaker is open, then send
    /// `RetryDue`.
    ScheduleRetry {
        /// The attempt that failed.
        attempt: NonZeroU32,
    },
    /// Report a failed session to the circuit breaker: connecting failed, or
    /// the session ended before the stable period. Always followed by
    /// [`Effect::ScheduleRetry`].
    RecordFailure,
    /// Report a successful session to the circuit breaker: the bot leaves
    /// `Online` after the stable period.
    RecordSuccess,
    /// Close the circuit breaker and forget its failures, because the bot was
    /// started on purpose (`Start`, `Reset` or `Resume`).
    ResetBreaker,
    /// Start running the bot's mode.
    StartMode,
    /// Stop running the bot's mode.
    StopMode,
    /// Respawn the bot after it died.
    Respawn,
    /// Alert a human.
    Notify(BotNotification),
}

/// An alert that needs a human. The actor publishes every state change anyway;
/// this is only for the ones someone has to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BotNotification {
    /// The bot paused; `Resume` connects it again.
    Paused {
        /// Why.
        reason: PauseReason,
    },
    /// The bot failed; `Reset` connects it again.
    Failed {
        /// Why.
        reason: FailReason,
    },
}
