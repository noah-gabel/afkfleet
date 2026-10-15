//! The handlers, one module per resource (Plan.md §4). A handler only parses
//! its input, authorizes, calls a service and maps the result: no business
//! logic and no SQL.
//!
//! - [`accounts`]: `POST /api/v1/accounts/link`, `GET
//!   /api/v1/accounts/link/{flow_id}` (P9.4) and `POST
//!   /api/v1/accounts/offline` (P9.7).
//! - [`auth`]: `POST /api/v1/auth/login`, `/auth/login/mfa`,
//!   `/auth/refresh`, `/auth/logout`, `/auth/step-up` and `/auth/register`
//!   (P7.6).
//! - [`health`]: `GET /health/live` and `GET /health/ready` (P6.7).
//! - [`invites`]: `POST` and `GET /api/v1/invites`, `DELETE
//!   /api/v1/invites/{id}` (P7.9).
//! - [`me`]: `GET /api/v1/me`, `POST /api/v1/me/password`, `GET
//!   /api/v1/me/sessions` and `DELETE /api/v1/me/sessions/{id}` (P7.6).
//! - [`mfa`]: `POST /api/v1/me/mfa/enroll`, `/confirm` and `/reset` (P7.7).
//! - [`users`]: `GET /api/v1/users` and `PATCH /api/v1/users/{id}` (P7.9).
//!
//! The helpers here turn what the services hand back into response fields.

pub(super) mod accounts;
pub(super) mod auth;
pub(super) mod health;
pub(super) mod invites;
pub(super) mod me;
pub(super) mod mfa;
pub(super) mod users;

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use fleet_api_types::auth::USERNAME_MAX_CHARS;
use fleet_api_types::{BoundedText, FieldError, PageQuery, SecretText, Timestamp};
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, SecretString};

use super::error::ApiError;
use super::extractors::AuthUser;
use crate::app::users::ActingUser;
use crate::ports::page::{Page, PageLimit};
use crate::ports::passwords::HashError;

/// The first 422 message for a new password the strength check refused;
/// zxcvbn's warning and suggestions follow it.
const TOO_WEAK: &str = "is too weak";

/// A new password the password service refused, as the API answers it
/// (ADR-0016): a weak one is 422 on `field` with "is too weak", then one
/// entry per warning and per suggestion in zxcvbn's own words; a full queue
/// is `busy`; anything else is the server's fault.
fn refused_password(field: &str, error: HashError) -> ApiError {
    match error {
        HashError::Weak {
            warning,
            suggestions,
        } => {
            let hints = warning
                .iter()
                .map(|warning| FieldError::new(field, warning.text()))
                .chain(
                    suggestions
                        .iter()
                        .map(|suggestion| FieldError::new(field, suggestion.text())),
                );
            ApiError::Validation(
                core::iter::once(FieldError::new(field, TOO_WEAK))
                    .chain(hints)
                    .collect(),
            )
        }
        HashError::Busy => ApiError::busy(),
        other @ (HashError::Random(_) | HashError::Failed(_)) => ApiError::internal(other),
    }
}

/// Who makes a write: the authenticated user, their session and their
/// client's address.
fn acting(user: &AuthUser, ip: IpAddr) -> ActingUser {
    ActingUser {
        actor: user.actor(),
        session_id: user.session_id(),
        ip,
    }
}

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
