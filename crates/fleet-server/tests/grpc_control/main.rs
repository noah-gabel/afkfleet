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
//! - `registry`: one stream per agent, the ends of a permission, and what an
//!   agent may report.
//! - `logs`: what never reaches a log line.
//! - `reconcile`: the reconciler over the port: which agent runs which bot,
//!   and what an agent hears of it (P10.5).
//! - `grants`: the session grants: only to the stream that holds a bot,
//!   never a later one, and their bounds (P10.6).
//! - `ingest`: what a stream reports about the bots it holds, through the
//!   ingest to the chat log, the sticky states and the bus (P10.8).
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
mod grant_checks;
mod grants;
mod handshake;
mod identity;
mod in_memory;
mod ingest;
mod liveness;
mod logs;
mod proxy;
mod reconcile;
mod registry;
mod session;
mod sizes;
mod support;
