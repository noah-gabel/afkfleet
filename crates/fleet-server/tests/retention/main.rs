//! Tests for P11.6's retention (Plan.md P11.6; ADR-0020, U19–U22,
//! S44–S46, R47–R56): what the store deletes and where it cuts
//! (`repository`), what one run deletes and how it holds up against the
//! ingest, failures and the stop (`service`), the task's summary
//! (`task`), and that deleted text doesn't stay in the database's files
//! (`at_rest`).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod at_rest;
mod fixtures;
mod repository;
mod service;
mod task;
mod watched;
