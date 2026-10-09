//! The afkfleet agent: it runs the bots.
//!
//! The agent is the imperative shell around [`fleet_runtime`] and [`fleet_mc`].
//! It reads its config, sets up logging, wires the Minecraft adapter to the
//! runtime and reports the bots' state. In standalone mode (development
//! only) the config lists the bots; in managed mode (Phase 10) the server
//! assigns them.
//!
//! # Modules
//! - [`cli`]: the command line of the `afkfleet-agent` binary.
//! - [`config`]: `agent.toml` plus `AFKFLEET_AGENT__…` environment variables,
//!   validated into the settings the runtime and the adapter take.
//! - [`telemetry`]: logging, with azalea's caps and a panic hook that logs
//!   through `tracing`.
//! - [`diagnostics`]: fleet-mc's numbers, sampled into metrics.
//! - [`run`]: the fleet's run, from the first bot to the exit code.
//!
//! The binary (`src/main.rs`) only wires these together: it loads the
//! config before the async runtime starts, installs the TLS provider and the
//! metrics recorder, and passes fleet-mc's `AzaleaConnector` to
//! [`run::run`].

pub mod cli;
pub mod config;
pub mod diagnostics;
pub mod run;
pub mod telemetry;
