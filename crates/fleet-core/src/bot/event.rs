//! [`BotEvent`]: what can happen to a bot.

use crate::disconnect::{ConnectFailure, DisconnectReason};

/// Something that happened to a bot: a command, an answer to an effect, or a
/// session event (Plan.md Appendix E; ADR-0010).
///
/// Session events (`Joined`, `ConnectFailed`, `Disconnected`, `Died`,
/// `WatchdogTimeout` and `SessionClosed`) come from the current session only.
/// Once the actor has executed [`Effect::Disconnect`](super::Effect::Disconnect)
/// for a session, it drops that session's events, and it reports
/// `SessionClosed` for it only if no newer session has connected since.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BotEvent {
    /// Run the bot.
    Start,
    /// Stop the bot.
    Stop,
    /// Try again after [`BotState::Failed`](super::BotState::Failed).
    Reset,
    /// Connect again after [`BotState::Paused`](super::BotState::Paused).
    Resume,
    /// The requested session credentials arrived.
    SessionReady,
    /// No session credentials could be had.
    SessionUnavailable {
        /// Whether asking again later may help.
        retryable: bool,
    },
    /// The bot joined the server.
    Joined,
    /// Connecting failed before the bot reached the server.
    ConnectFailed(ConnectFailure),
    /// The session ended.
    Disconnected(DisconnectReason),
    /// The backoff before the next attempt is over.
    RetryDue,
    /// The session stopped ticking: it hung.
    WatchdogTimeout,
    /// The bot died in the game.
    Died,
    /// The session is gone: its teardown finished or timed out, or it ended
    /// without saying why.
    SessionClosed,
    /// The bot's actor crashed too often (Plan.md §6, row 7).
    CrashLoop,
}
