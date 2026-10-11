//! The server's own extractors (Plan.md §4, P7.8).
//!
//! - [`AuthUser`]: who a request comes from, from its access token; every
//!   authed handler takes it and authorizes through [`AuthUser::require`].

mod auth_user;

pub use auth_user::AuthUser;
