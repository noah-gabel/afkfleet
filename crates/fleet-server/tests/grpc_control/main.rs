//! `AgentControl.Connect` over the gRPC port (Plan.md P10.4, P10.9;
//! ADR-0019, S22–S30): the real listener on `127.0.0.1:0` with a fresh PKI
//! and database, called by tonic's client over TLS.
//!
//! - `identity`: who may open a stream, P10.9's four rejections among them.
//! - `handshake`: the hello, its checks and every answer before `Welcome`.
//! - `session`: what a welcomed stream does until it ends, and what its end
//!   records.
//! - `liveness`: heartbeats, silent streams and silent connections.
//! - `bound`: the connections' age, which a welcomed stream lifts.
//! - `sizes`: the message limits.
//! - `admission`: `[rate_limits] connect_per_cert_per_min`.
//! - `logs`: what never reaches a log line.
//! - `in_memory`: the one test on paused time, over an in-memory pipe with a
//!   fake behind the port (U6).
//!
//! Tests over real sockets run on real time, with short bounds and never a
//! sleep (CLAUDE.md); the services' clock is a `ManualClock` that starts at
//! the real time, so the certificates TLS checks are valid.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod admission;
mod bound;
mod handshake;
mod identity;
mod in_memory;
mod liveness;
mod logs;
mod proxy;
mod session;
mod sizes;
mod support;
