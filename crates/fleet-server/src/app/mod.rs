//! The services: one use case each, with its transaction and its audit entry
//! (Plan.md §4, ADR-0015). They hold the ports as `Arc<dyn …>` and read the
//! time and randomness only through fleet-core's `Clock` and `SecureRandom`.
//!
//! - [`audit`]: stamping audit entries, recording the ones that change
//!   nothing, and listing the log.
//! - [`auth`]: sessions and their tokens: issuing, refreshing with reuse
//!   detection (P7.3), and from P7.8 authenticating every request.
//! - [`challenges`]: the login's mfa and setup tokens, held in memory
//!   (P7.6, P7.7).
//! - [`health`]: whether the server is ready to serve requests.
//! - [`login`]: logging in (the password step and the code step) and
//!   stepping up (P7.6).
//! - [`lockout`]: the per-username lockout: when failed logins lock a name,
//!   and its periodic purge (P7.10).
//! - [`mfa`]: the second factor: enrolling and confirming a TOTP
//!   authenticator, replacing it, and checking a code at login (P7.7).
//! - [`invites`]: creating, listing and revoking the invites to register
//!   (P7.9).
//! - [`session_tokens`]: Minecraft session tokens for linked Microsoft
//!   accounts, one refresh per account at a time (P9.5).
//! - [`users`]: creating the Owner (P7.5), the admins' list and changes
//!   (P7.9), and the re-check every admin's write makes in its transaction.
//! - [`vault`]: what the vault's keys seal, and re-encrypting the secrets
//!   under the active key (P9.2).

pub mod audit;
pub mod auth;
pub mod challenges;
pub mod health;
pub mod invites;
pub mod lockout;
pub mod login;
pub mod mfa;
pub mod session_tokens;
pub mod users;
pub mod vault;
