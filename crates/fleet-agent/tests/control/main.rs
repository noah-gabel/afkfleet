//! The managed agent against a fake control server over mTLS on loopback
//! (Plan.md P10.7; ADR-0019, U19), in real time with short timings: every
//! wait has an upper bound that runs out only when a test fails, and none
//! sleeps.
//!
//! - `server` is the fake `AgentControl` server: the generated service with
//!   fleet-proto's codec, client certificates required, each call handed to
//!   the test after its hello.
//! - `proxy` stalls the connections already open, as a dead server does.
//! - `agent` runs the real managed run on fleet-testkit's fake connector.
//! - `link` holds the cases; `pki` is the seeded test PKI.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

mod agent;
mod link;
#[path = "../common/pki.rs"]
mod pki;
mod proxy;
mod server;
