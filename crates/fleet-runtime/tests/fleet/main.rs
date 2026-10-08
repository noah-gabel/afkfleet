//! Component tests for the fleet (Plan.md P4.7; ADR-0013): its supervisor
//! and the `Fleet` handle, against fleet-testkit's fakes on paused time.
//!
//! - `panicky` wraps the fake connector so connects or sessions panic on
//!   cue, as a bug in the ports would; the fakes stay panic-free.
//! - `supervisor` holds the scenarios.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

#[path = "../common/mod.rs"]
mod common;
mod panicky;
mod supervisor;
