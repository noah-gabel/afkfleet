//! Component tests for the agent's run (Plan.md P5.3; ADR-0014), against
//! fleet-testkit's fake connector and a fake for fleet-mc's diagnostics, on
//! paused time.
//!
//! - `harness` holds what the test files share: values, paused-time helpers,
//!   and the run under test with the test's handles on it.
//! - `fakes` holds the fake diagnostics.
//! - `startup` covers the bots' IDs, the startup lines and every startup
//!   error.
//! - `sampling` covers the diagnostics: the abandoned-thread limit, the
//!   sampling period and the metrics.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

#[path = "../common/mod.rs"]
mod common;
mod fakes;
mod harness;
mod sampling;
mod startup;
