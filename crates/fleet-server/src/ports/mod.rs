//! The repository traits the services depend on (Plan.md §4, P6.4,
//! ADR-0015). Services hold them as `Arc<dyn …>`; `infra` implements them,
//! and nothing here names sqlx, so a different database could implement
//! them too (ADR-0005).
//!
//! - [`store`]: the database as a unit of work. [`store::Store`] hands out
//!   read handles and one write transaction at a time, [`store::WriteTx`],
//!   whose commit always records an audit entry.
//! - [`users`]: the users.
//! - [`audit`]: the audit log. It's append-only: the only operations are
//!   recording (inside a write transaction) and listing.

pub mod audit;
pub mod store;
pub mod users;
