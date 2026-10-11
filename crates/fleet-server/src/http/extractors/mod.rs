//! The server's own extractors (Plan.md §4, P7.6, P7.7, P7.8).
//!
//! - [`AuthUser`]: who a request comes from, from its access token; every
//!   authed handler takes it and authorizes through [`AuthUser::require`].
//! - [`SteppedUpUser`]: an [`AuthUser`] who entered their password again
//!   within 5 minutes, for the step-up actions (§7.2).
//! - [`SetupToken`]: a request to enroll a TOTP authenticator, from its
//!   setup token.
//! - [`ValidJson`]: a JSON request body that passed its DTO's garde rules.
//!
//! An extractor that reads credentials comes before any body extractor, so
//! a request without them is refused before its body is read.

mod auth_user;
mod bearer;
mod json;
mod setup_token;
mod step_up;

pub use auth_user::AuthUser;
pub use json::ValidJson;
pub use setup_token::SetupToken;
pub use step_up::SteppedUpUser;
