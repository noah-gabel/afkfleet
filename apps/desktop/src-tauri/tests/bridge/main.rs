//! The app's commands against the real server, run in-process on 127.0.0.1:0
//! with a manual clock (Plan.md P8.4, ADR-0017): every command's logic, the
//! errors each maps, and the server profile's switch rules. Tauri around the
//! commands is tested on its mock runtime in `acl.rs`.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod account;
mod accounts;
mod auth;
mod bots;
mod chat;
mod live;
mod support;
mod switch;
