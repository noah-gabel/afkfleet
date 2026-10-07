//! The azalea adapter of afkfleet (Plan.md Phase 3; ADR-0008, ADR-0011): it
//! implements the `fleet_core::mc` ports with azalea, and it's the only crate
//! that depends on azalea.
//!
//! Every session runs on a host thread of its own (ADR-0008 §2), so a panic or
//! a hang in azalea affects exactly one bot:
//!
//! - [`AzaleaConnector`] is the `MinecraftConnector`. It starts each session
//!   on a new host thread with azalea's auto-reconnect and auto-respawn off and
//!   Bevy's single-threaded executor, bounds connecting with the connect
//!   timeout, and tears a session down in ADR-0008 §10's order.
//!   [`McSession`] is the actor's handle to it.
//! - [`McHostPool`] starts one [`HostThread`] per session: a named OS thread
//!   with a current-thread tokio runtime and a `LocalSet`, which is what azalea
//!   needs. Work reaches it over a bounded queue. A thread that hangs is
//!   abandoned and counted, and at the limit the pool refuses new threads.
//! - [`McEvents`] delivers a session's events to its actor. On the host
//!   thread, azalea's events are mapped to `SessionEvent`s and pass a bounded
//!   bridge that only ever drops chat; ticks and received packets only stamp
//!   the session's liveness.
//! - A session logs in with the server-issued Minecraft token, never a
//!   Microsoft one (security rule 6). Its account joins through the session
//!   server under a timeout and reports a refused join to the session, since
//!   azalea raises no event for it. Offline sessions use azalea's offline
//!   account.
//! - [`McConfig`] holds the tuning values that ADR-0011 decided.
//!
//! Like all library code here, it never panics: failures are errors, and
//! overload is [`JobError::QueueFull`] instead of a wait.

#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!(
    "fleet-mc's `fault-injection` feature is for the slow tests only: no release build may include its App hook (ADR-0011)"
);

mod account;
mod actions;
mod config;
mod connector;
mod events;
mod host;

pub use config::McConfig;
pub use connector::{AzaleaConnector, McSession};
pub use events::McEvents;
pub use host::{HostThread, JobError, McHostPool, ShutdownOutcome, SpawnError};
