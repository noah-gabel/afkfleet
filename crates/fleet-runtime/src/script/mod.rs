//! Scripted modes in the runtime: a bot's script in its session (Plan.md
//! P15.16 to P15.18, P15.33; ADR-0036, ADR-0037, ADR-0043).
//!
//! `fleet-script` runs a script's hooks; `fleet-core` decides what a script
//! may do; this module gives a script its events and its time, and performs
//! what it asks for:
//!
//! - [`ScriptRunner`] runs a script in one session, inside the session's
//!   mode runner task: an event or a due timer takes a snapshot, runs the
//!   hook to its end and performs its actions.
//! - [`ScriptSlot`] is what of a bot's script outlives a session: the
//!   instance with its state, the bot's limiter and its chat watch. The
//!   supervisor keeps it, as it keeps the bot's chat bucket.
//!   [`ScriptPrograms`] are the compiled scripts the fleet's bots share.
//! - [`ScriptFeed`] and [`ScriptInbox`] carry the session's events from
//!   the bot's actor, their one reader, to the runner: bounded, whatever a
//!   server sends.
//! - [`ScriptEvent`] is what the app hears of a script: its status, its
//!   error with the line, its log.
//!
//! **A hook runs on the runner's own task,** an async worker: a bounded
//! computation of a few microseconds as a rule and 5 ms at most, 20 ms a
//! second for one script. ADR-0043 has what that does to the other tasks
//! of a runtime with 200 scripted bots. Compiling a script runs off the
//! workers, in `spawn_blocking`.
//!
//! **The runtime still reads no clock of the OS's.** A script's timers,
//! its `hold_for`s and its replies' windows run on tokio's clock, and a
//! hook's own time is read from the [`Stopwatch`] the fleet is built with:
//! the OS's monotonic clock in an agent, a manual one in tests.
//!
//! [`Stopwatch`]: fleet_core::script::Stopwatch

mod event;
mod feed;
mod runner;
mod slot;

pub use event::{LoggedValue, NotSent, ScriptEvent, ScriptLog, ScriptStatus};
pub use feed::{Life, ScriptFeed, ScriptInbox, script_feed};
pub use runner::ScriptParts;
pub(crate) use runner::{Borrowed, Next, ScriptRunner};
pub use slot::{ScriptPrograms, ScriptSlot};
