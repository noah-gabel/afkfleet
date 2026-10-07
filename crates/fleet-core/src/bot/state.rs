//! [`BotState`]: where a bot is in its lifecycle.

use core::num::NonZeroU32;

use chrono::{DateTime, Utc};

use crate::disconnect::{ConflictKind, PermanentKind};

/// Where a bot is in its lifecycle (Plan.md Appendix E; ADR-0010).
///
/// `attempt` counts the connection attempts since the last stable session or
/// deliberate start; the first attempt is 1. In `AwaitingSession`, `Connecting`
/// and `Online` it's the number of the current attempt; in `Backoff` it's the
/// number of the attempt that just failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BotState {
    /// Not running, and not asked to.
    Stopped,
    /// Waiting for session credentials before connecting.
    AwaitingSession {
        /// The attempt the session is for.
        attempt: NonZeroU32,
        /// Whether a fresh session was requested, bypassing any token cache,
        /// because the previous one was rejected.
        fresh: bool,
    },
    /// Connecting to the Minecraft server; a session exists.
    Connecting {
        /// The current attempt.
        attempt: NonZeroU32,
        /// Whether this attempt already uses a fresh session after an auth
        /// failure. Another one then fails the bot.
        auth_retried: bool,
    },
    /// Joined the server; a session exists and the mode runs.
    Online {
        /// When the bot joined.
        since: DateTime<Utc>,
        /// The attempt that got the bot online.
        attempt: NonZeroU32,
    },
    /// Waiting before the next attempt.
    Backoff {
        /// The attempt that failed.
        attempt: NonZeroU32,
    },
    /// Stopped by itself because a human is playing on the account. Only
    /// `Resume` lets it connect again.
    Paused {
        /// Why.
        reason: PauseReason,
    },
    /// Stopped by itself because retrying won't help. Only `Reset` lets it
    /// connect again.
    Failed {
        /// Why.
        reason: FailReason,
    },
    /// The session is being torn down.
    Stopping {
        /// Whether the bot starts again once the teardown has finished.
        restart: bool,
    },
}

/// Why a bot is [`BotState::Failed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailReason {
    /// The session ended for a reason that won't heal: the server kicked the
    /// bot, or the session server refused the account (ADR-0011).
    Permanent {
        /// Why.
        kind: PermanentKind,
    },
    /// The server rejected the session, even a fresh one: the account needs
    /// to be signed in again.
    Auth,
    /// No session can be issued for the account.
    SessionDenied,
    /// The bot's actor crashed too often.
    CrashLoop,
}

/// Why a bot is [`BotState::Paused`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PauseReason {
    /// Someone else logged in to the account.
    Conflict {
        /// Why.
        kind: ConflictKind,
    },
}
