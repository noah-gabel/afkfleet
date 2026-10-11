//! The login routes over HTTP (Plan.md P7.6, P7.12, §7.2, §7.4,
//! ADR-0016), through the real router with `oneshot`:
//!
//! - [`login`]: both steps, enumeration, the lockout through login, the
//!   mfa token's limits, re-hashing and the re-checks before the commit.
//! - [`refresh`]: rotation, reuse revoking the session, expired sessions,
//!   and the read-pool check that keeps unknown tokens off the write
//!   connection.
//! - [`logout`]: ending a session.
//! - [`step_up`]: the stamp, a wrong password counted, and the re-checks.
//!
//! Each test gets a fresh database from `#[sqlx::test]`; time is a
//! `ManualClock`, never paused (CLAUDE.md). Every hash is a real Argon2id
//! at the baseline parameters.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod login;
mod logout;
mod refresh;
mod step_up;
mod support;
