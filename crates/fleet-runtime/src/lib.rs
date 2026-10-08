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

mod credentials;

pub use credentials::OfflineCredentials;
