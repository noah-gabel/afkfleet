//! The handlers, one module per resource (Plan.md §4). A handler only parses
//! its input, authorizes, calls a service and maps the result: no business
//! logic and no SQL.
//!
//! - [`health`]: `GET /health/live` and `GET /health/ready` (P6.7).
//! - [`mfa`]: `POST /api/v1/me/mfa/enroll`, `/confirm` and `/reset` (P7.7).

pub(super) mod health;
pub(super) mod mfa;
