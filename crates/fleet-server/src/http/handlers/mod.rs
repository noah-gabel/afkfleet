//! The handlers, one module per resource (Plan.md §4). A handler only parses
//! its input, authorizes, calls a service and maps the result: no business
//! logic and no SQL.
//!
//! - [`auth`]: `POST /api/v1/auth/login`, `/auth/login/mfa`,
//!   `/auth/refresh`, `/auth/logout` and `/auth/step-up` (P7.6).
//! - [`health`]: `GET /health/live` and `GET /health/ready` (P6.7).
//! - [`mfa`]: `POST /api/v1/me/mfa/enroll`, `/confirm` and `/reset` (P7.7).

pub(super) mod auth;
pub(super) mod health;
pub(super) mod mfa;
