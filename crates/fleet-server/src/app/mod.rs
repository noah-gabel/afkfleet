//! The services: one use case each, with its transaction and its audit entry
//! (Plan.md §4, ADR-0015). They hold the ports as `Arc<dyn …>` and read the
//! time and randomness only through fleet-core's `Clock` and `SecureRandom`.
//!
//! - [`audit`]: stamping audit entries, recording the ones that change
//!   nothing, and listing the log.
//! - [`auth`]: sessions and their tokens: issuing, refreshing with reuse
//!   detection (P7.3), and from P7.8 authenticating every request.
//! - [`health`]: whether the server is ready to serve requests.
//! - [`users`]: creating the Owner (P7.5); registration and the admins'
//!   changes arrive in group E.

pub mod audit;
pub mod auth;
pub mod health;
pub mod users;
