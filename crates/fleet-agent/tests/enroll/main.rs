//! `afkfleet-agent enroll` (Plan.md P10.3; ADR-0019, S15, S18, S19): one
//! test binary, so its modules share the helpers in `support`.
//!
//! - `files`: the identity's files, checked, locked, written key first and
//!   published without replacing anything, in folders under cargo's temp
//!   folder.
//! - `command`: the whole command against a fake gRPC port over real TLS:
//!   the pin, an identity that exists, the answers, and what's written.
//!
//! Tests over real sockets run on real time, with short bounds and never a
//! sleep (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod command;
mod files;
mod support;
