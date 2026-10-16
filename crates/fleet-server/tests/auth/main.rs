//! The auth routes over HTTP (Plan.md P7.6, P7.9, P7.12, §7.2, §7.4,
//! ADR-0016), through the real router with `oneshot`:
//!
//! - [`login`]: both steps, enumeration, the lockout through login, the
//!   mfa token's limits, re-hashing and the re-checks before the commit.
//! - [`refresh`]: rotation, reuse revoking the session, expired sessions,
//!   and the read-pool check that keeps unknown tokens off the write
//!   connection.
//! - [`logout`]: ending a session.
//! - [`step_up`]: the stamp, a wrong password counted, and the re-checks.
//! - [`users`]: the admins' list and change, P7.12's escalation and
//!   disabling, and the re-check in the transaction.
//! - [`enrollment_tokens`]: the Owner's enrollment token, shown once; the
//!   step-up, the cap, the switched-off control plane and the re-checks.
//! - [`invites`]: who may invite whom, the code shown once, the cap, the
//!   list, revoking, and the re-check in the transaction.
//! - [`register`]: an invite registers one user; reuse, expiry and every
//!   other unusable invite; the checks' order and the races.
//! - [`me`]: the profile and the wrong codes since the previous login.
//! - [`password`]: a new password after stepping up, and its re-checks.
//! - [`sessions`]: the live sessions, and ending another one.
//!
//! Each test gets a fresh database from `#[sqlx::test]`; time is a
//! `ManualClock`, never paused (CLAUDE.md). Every hash is a real Argon2id
//! at the baseline parameters.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod enrollment_tokens;
mod invites;
mod login;
mod logout;
mod me;
mod password;
mod refresh;
mod register;
mod sessions;
mod step_up;
mod support;
mod users;
