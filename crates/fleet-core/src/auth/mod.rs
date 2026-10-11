//! The rules of authentication, as pure decisions (Plan.md Phase 7,
//! ADR-0016).
//!
//! The server loads the facts, calls a rule here and does the IO, the way
//! its handlers call [`crate::authz::authorize`]. Time is passed in as
//! `DateTime<Utc>`, and nothing here hashes, encrypts or reads a clock: the
//! crypto (argon2, SHA-256, TOTP, zxcvbn) stays in fleet-server. Each Phase 7
//! group adds the rules its feature needs:
//!
//! - [`NewPassword`]: what a new password must be (§7.2's 12 to 128
//!   characters, no control characters), checked before the server's
//!   strength check ever sees it.
//! - [`WeakWarning`] and [`WeakSuggestion`]: why the server's strength check
//!   refused a password, in our own words, so its port names no crate's type.
//! - [`SessionPolicy`] and [`refresh_decision`]: a session's lifetimes, when
//!   its tokens expire, and what a refresh comes to (group B).

mod password;
mod session;
mod strength;

pub use password::{NewPassword, NewPasswordError};
pub use session::{
    RefreshDecision, RefreshFacts, RefreshMatch, SessionPolicy, SessionPolicyError, SessionTimes,
    refresh_decision,
};
pub use strength::{WeakSuggestion, WeakWarning};
