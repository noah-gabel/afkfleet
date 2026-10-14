//! The Microsoft accounts' tests (Plan.md Phase 9, ADR-0018), one binary so
//! the port's fake is shared without being compiled into several binaries:
//!
//! - [`support`]: the fake [`MicrosoftAuthProvider`](fleet_server::ports::msauth::MicrosoftAuthProvider),
//!   and the session-token service's setup.
//! - [`msauth_fake`]: the fake's own behavior.
//! - [`session_tokens`]: the session-token service (P9.5).
//! - [`manual`]: the user's hands-on check against Microsoft, which only the
//!   user runs (`just test-ms-account`).
//!
//! Later Phase 9 groups add their service tests here.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod manual;
mod msauth_fake;
mod session_tokens;
mod support;
