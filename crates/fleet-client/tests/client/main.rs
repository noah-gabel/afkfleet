//! fleet-client against the real server, run in-process on 127.0.0.1:0
//! with a manual clock (Plan.md P8.1, ADR-0017): every endpoint, the
//! server's refresh rules, and the certificate checks.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod accounts;
mod admin;
mod auth;
mod bots;
mod link;
mod me;
mod mfa;
mod refresh;
mod support;
mod tls;
