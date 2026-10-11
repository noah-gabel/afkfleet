//! The `afkfleet-server` binary and the `serve` wiring (Plan.md P6.11,
//! ADR-0015): one test binary, so its modules share the helpers in
//! `support`.
//!
//! - `cli`: the real binary as a child process, its exit codes and output.
//! - `boot`: `serve::run` in-process on a listener the test binds.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod boot;
mod cli;
mod support;
