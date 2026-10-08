//! The bot state machine (Plan.md Appendix E, refined by ADR-0010).
//!
//! [`transition`] is the only place that decides a bot's lifecycle. It takes
//! the current [`BotState`] and a [`BotEvent`] and returns the next state plus
//! the [`Effect`]s the bot's actor executes, in order. It's pure and total:
//! every event fits every state, and one that doesn't make sense there changes
//! nothing.
//!
//! In short:
//! - `Start` asks for a session, `SessionReady` connects, `Joined` goes
//!   online and starts the mode.
//! - A session that ends is classified ([`ConflictTexts::classify`]):
//!   transient ends back off and retry, a permanent kick fails the bot, a
//!   duplicate login pauses it, and a rejected session is retried once with a
//!   fresh token before the bot fails.
//! - After the stable-online period the attempt counter starts over, and the
//!   circuit breaker hears about every session's outcome.
//! - `Paused` and `Failed` are sticky: only `Resume` and `Reset` (or a crash
//!   loop) leave them, so the bot never fights a human playing on the account.
//! - `Stop` tears a session down through `Stopping`; `Start` during `Stopping`
//!   restarts the bot once the teardown has finished.
//!
//! [`BotRules`] carry what `transition` needs to know about one bot: the retry
//! policy, and the kick texts that count as a duplicate login. A bot's
//! [`BotSpec`] says what it should be doing, and its [`BotSnapshot`] what it's
//! doing now (ADR-0013).
//!
//! The actor's side of the contract:
//! - It holds the session credentials; events carry none.
//! - Leaving a state cancels that state's timer or session request.
//! - Only the current session's events reach [`transition`] (see
//!   [`BotEvent`]).
//! - It publishes every state change, plus `Died`, as events for the app.
//!   [`Effect::Notify`] is only for alerts that need a human.
//!
//! [`ConflictTexts::classify`]: crate::disconnect::ConflictTexts::classify

mod effect;
mod event;
mod rules;
mod snapshot;
mod spec;
mod state;
mod transition;

pub use effect::{BotNotification, Effect};
pub use event::BotEvent;
pub use rules::BotRules;
pub use snapshot::BotSnapshot;
pub use spec::{BotAccount, BotSpec, DesiredRunState};
pub use state::{BotState, FailReason, PauseReason};
pub use transition::{Transition, transition};
