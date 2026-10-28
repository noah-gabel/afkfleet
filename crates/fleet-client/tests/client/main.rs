//! fleet-client against the real server, run in-process on 127.0.0.1:0
//! with a manual clock (Plan.md P8.1, ADR-0017): every endpoint, the
//! server's refresh rules, the certificate checks, and the live events socket
//! (P11.8; `relay` carries its connections for the log test). `console` runs
//! the dev console of `examples/dev-bots` (`dev_console`; P10.7, S71).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod accounts;
mod admin;
mod auth;
mod bots;
mod chat;
mod console;
#[path = "../../examples/dev-bots/console.rs"]
mod dev_console;
mod events;
mod link;
mod me;
mod mfa;
mod modes;
mod refresh;
mod relay;
mod server_settings;
mod support;
mod tls;
