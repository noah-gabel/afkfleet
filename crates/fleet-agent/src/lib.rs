//! The afkfleet agent: it runs the bots.
//!
//! The agent is the imperative shell around [`fleet_runtime`] and [`fleet_mc`].
//! It reads its config, sets up logging, wires the Minecraft adapter to the
//! runtime and reports the bots' state. In standalone mode (development
//! only) the config lists the bots; in managed mode (Phase 10) the server
//! assigns them.
//!
//! # Modules
//! - [`config`]: `agent.toml` plus `AFKFLEET_AGENT__…` environment variables,
//!   validated into the settings the runtime and the adapter take.
//! - [`telemetry`]: logging, with azalea's caps and a panic hook that logs
//!   through `tracing`.
//!
//! The `afkfleet-agent` binary arrives with the wiring (P5.3); until then this
//! crate is a library only (ADR-0014).

pub mod config;
pub mod telemetry;
