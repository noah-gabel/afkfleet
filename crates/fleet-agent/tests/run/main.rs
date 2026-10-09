//! Component tests for the agent's run (Plan.md P5.3, P5.4; ADR-0014),
//! against fleet-testkit's fake connector and fakes for fleet-mc's
//! diagnostics and the stop signals, on paused time.
//!
//! - `harness` holds what the test files share: values, paused-time helpers,
//!   and the run under test with the test's handles on it.
//! - `fakes` holds the fake diagnostics and signals.
//! - `startup` covers the bots' IDs, the startup lines and every startup
//!   error.
//! - `sampling` covers the diagnostics: the abandoned-thread limit, the
//!   sampling period and the metrics.
//! - `signals` covers the graceful shutdown on a signal.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

#[path = "../common/mod.rs"]
mod common;
mod fakes;
mod harness;
mod sampling;
mod signals;
mod startup;
