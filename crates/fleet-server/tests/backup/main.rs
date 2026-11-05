//! Tests for P12.3's backups (Plan.md P12.3; ADR-0022, S2–S5, R1, R2): one
//! backup file and the backups' folder (`file`), and the service that names,
//! schedules and prunes them (`service`).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod file;
mod fixtures;
mod service;
