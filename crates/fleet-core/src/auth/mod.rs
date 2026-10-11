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
//! - [`access_decision`], [`touch_due`] and [`is_stepped_up`]: whether a
//!   request's access token counts, when it notes its session's use, and
//!   whether a step-up action is allowed (group B).
//! - [`LockoutPolicy`], [`check_lock`] and [`after_failure`]: when a
//!   username's failed logins lock it, for how long, and when its streak
//!   is forgotten (group C).

mod access;
mod lockout;
mod password;
mod session;
mod strength;

pub use access::{
    AccessFacts, AccessRejection, STEP_UP_WINDOW, TOUCH_INTERVAL, access_decision, is_stepped_up,
    touch_due, touch_stale_since,
};
pub use lockout::{
    FAILURE_MEMORY, FIRST_LOCK, FailureOutcome, FailureStreak, Locked, LockoutPolicy,
    LockoutPolicyError, MAX_LOCK, after_failure, check_lock, expired_failed_by, is_expired,
};
pub use password::{NewPassword, NewPasswordError};
pub use session::{
    RefreshDecision, RefreshFacts, RefreshMatch, SessionPolicy, SessionPolicyError, SessionTimes,
    refresh_decision,
};
pub use strength::{WeakSuggestion, WeakWarning};
