//! The bot runtime of afkfleet (Plan.md Phase 4; ADR-0013).
//!
//! Every bot runs as one actor, a tokio task that feeds commands, session
//! events and timers to `fleet_core::bot::transition` and executes the effects
//! it returns. A supervisor owns the actors and restarts one that panics, and
//! a `Fleet` handle is the runtime's API. The runtime is generic over the
//! fleet-core ports, so it never sees azalea: tests drive it with
//! fleet-testkit's fakes on paused time, and the agent wires in fleet-mc.
//!
//! The runtime never reads the wall clock or the OS's randomness. Its caller
//! passes in a wall-clock anchor and a seed, and all time goes through tokio's
//! clock, so paused-time tests control everything (ADR-0013).
//!
//! - [`OfflineCredentials`] is the standalone agent's session-credential
//!   provider: offline accounts log in with their name.
//! - [`RuntimeClock`] is the runtime's wall clock, and [`RuntimeConfig`] its
//!   settings.
//! - [`ChatQueue`] is a bot's outbound chat: a bounded queue with a rate
//!   limit, shared by user chat and mode chat.
//! - [`ModeRunner`] runs a bot's mode in one session.
//! - [`BotActor`] is one bot's actor. It takes a [`BotCommand`] per `Fleet`
//!   call through its [`BotInbox`] and ends with an [`ActorExit`].
//! - [`FleetEvent`]s tell the app what happened to a bot.

mod actor;
mod chat;
mod clock;
mod config;
mod credentials;
mod event;
mod failure_log;
mod mode;

pub use actor::{
    ActorExit, BotActor, BotActorParts, BotCommand, BotInbox, CrashedTask, InboxError,
};
pub use chat::{
    ChatBucket, ChatBucketError, ChatDelivery, ChatError, ChatFailure, ChatQueue, ChatTicket,
    ChatTickets, ModeChat,
};
pub use clock::RuntimeClock;
pub use config::RuntimeConfig;
pub use credentials::OfflineCredentials;
pub use event::{FleetEvent, FleetEventKind};
pub use mode::ModeRunner;
