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
//! - `heartbeat` covers when the run beats: from the start of its run until
//!   its shutdown starts.
//! - `endpoint` covers the metrics endpoint in the run: its startup line, and
//!   the run's own series on a real socket, on real time; `scrape`, shared
//!   with the managed run's tests, asks for them.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

mod endpoint;
mod fakes;
mod harness;
mod heartbeat;
mod sampling;
#[path = "../common/scrape.rs"]
mod scrape;
mod signals;
mod startup;
