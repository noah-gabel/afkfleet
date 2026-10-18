//! Component tests for the agent's managed mode (Plan.md P10.6; ADR-0019,
//! S3, S62, S63), on paused time.
//!
//! - `fakes` holds the fake link to the control stream.
//! - `credentials` covers the managed credentials provider: correlation,
//!   late answers, the link's ends, expiry, kept grants and its logs.
//! - `identity` covers the identity's load at start (P10.7, S68), with
//!   `pki`'s seeded test PKI.
//! - `outbound` covers the outbox's request stream (P10.7, S66).
//! - `conductor` covers the conductor (P10.7, S65) on `rig`: a real fleet on
//!   the fake connector, the real outbox and credentials, the test playing
//!   the link.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

mod conductor;
mod credentials;
mod fakes;
mod identity;
mod outbound;
#[path = "../common/pki.rs"]
mod pki;
mod rig;
