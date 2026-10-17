//! Component tests for the agent's managed mode (Plan.md P10.6; ADR-0019,
//! S3, S62, S63), on paused time.
//!
//! - `fakes` holds the fake link to the control stream.
//! - `credentials` covers the managed credentials provider: correlation,
//!   late answers, the link's ends, expiry, kept grants and its logs.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

mod credentials;
mod fakes;
