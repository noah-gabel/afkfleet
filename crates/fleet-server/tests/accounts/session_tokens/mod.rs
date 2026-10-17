//! The session-token service (Plan.md P9.5, ADR-0018), over a fresh
//! database from `#[sqlx::test]`, the Microsoft fake and a manual clock:
//!
//! - [`refusals`]: who gets no session, without a call to Microsoft.
//! - [`cache`]: a session's refresh, store and minting, the cache, fresh
//!   requests and renames.
//! - [`single_flight`]: one refresh per account, whatever its callers do.
//! - [`status`]: Microsoft's refusals and the status changes they make.
//! - [`backoff`]: the account's backoff and the global pause.
//! - [`store_failures`]: a rotated token that couldn't be stored is kept.
//! - [`envelope`]: a stored refresh token that can't be used.
//! - [`grants`]: the session grants over the service: a link again during a
//!   grant, the margin, fresh requests and a disabled owner (P10.6).
//! - [`lifecycle`]: the wait bound, shutdown, a task that ends without an
//!   answer, `Debug` and the logs' redaction.
//!
//! Time isn't paused: sqlx's worker threads would make paused timeouts fire
//! early (CLAUDE.md). Every wait is bounded, and every expiry comes from the
//! manual clock.

mod backoff;
mod cache;
mod envelope;
mod grants;
mod lifecycle;
mod refusals;
mod single_flight;
mod status;
mod store_failures;
