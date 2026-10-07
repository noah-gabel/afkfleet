//! fleet-mc against real Minecraft servers in local containers (Plan.md P3.6,
//! P3.7; ADR-0008, ADR-0011).
//!
//! Each `slow_` scenario starts its own `itzg/minecraft-server` container and
//! runs several steps against it. nextest can't share a container between
//! test processes, so the slow profile runs them one at a time, and they need
//! Docker: run them with `just test-slow`, which also turns on fleet-mc's
//! test-only `fault-injection` feature for the actions and containment
//! scenarios, which add systems to a session's App. The `pins` test is fast
//! and runs with every `just test`.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

#[cfg(feature = "fault-injection")]
mod actions;
#[cfg(feature = "fault-injection")]
mod checks;
#[cfg(feature = "fault-injection")]
mod containment;
mod harness;
mod offline;
mod online;
mod pins;
