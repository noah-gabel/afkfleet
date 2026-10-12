//! The handlers, one module per resource (Plan.md §4). A handler only parses
//! its input, authorizes, calls a service and maps the result: no business
//! logic and no SQL.
//!
//! - [`auth`]: `POST /api/v1/auth/login`, `/auth/login/mfa`,
//!   `/auth/refresh`, `/auth/logout` and `/auth/step-up` (P7.6).
//! - [`health`]: `GET /health/live` and `GET /health/ready` (P6.7).
//! - [`invites`]: `POST` and `GET /api/v1/invites`, `DELETE
//!   /api/v1/invites/{id}` (P7.9).
//! - [`mfa`]: `POST /api/v1/me/mfa/enroll`, `/confirm` and `/reset` (P7.7).
//! - [`users`]: `GET /api/v1/users` and `PATCH /api/v1/users/{id}` (P7.9).
//!
//! The helpers here turn what the services hand back into response fields.

pub(super) mod auth;
pub(super) mod health;
pub(super) mod invites;
pub(super) mod mfa;
pub(super) mod users;

use chrono::{DateTime, Utc};
use fleet_api_types::auth::USERNAME_MAX_CHARS;
use fleet_api_types::{BoundedText, PageQuery, SecretText, Timestamp};
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, SecretString};

use super::error::ApiError;
use crate::ports::page::{Page, PageLimit};

/// A secret the service made, as a response field.
fn secret_text<const MAX: usize>(secret: &SecretString) -> SecretText<MAX> {
    SecretText::new(secret.expose_secret().to_owned())
}

/// A stored time as a response field. Stored times are within the years 0
/// to 9999, so one outside them is a bug.
fn timestamp(at: DateTime<Utc>) -> Result<Timestamp, ApiError> {
    Timestamp::try_from(at).map_err(ApiError::internal)
}

/// A username as a response field: always within its cap.
fn username(name: &Username) -> BoundedText<USERNAME_MAX_CHARS> {
    BoundedText::new(name.as_str())
}

/// The page a list's query asks for. garde has checked the limit, so one
/// out of range is a bug.
fn page<I: Copy>(query: &PageQuery<I>) -> Result<Page<I>, ApiError> {
    Ok(Page {
        before: query.before,
        limit: PageLimit::new(query.limit_or_default()).map_err(ApiError::internal)?,
    })
}
