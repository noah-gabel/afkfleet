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
//! - [`candidate_steps`] and [`TotpCode`]: which TOTP time steps a code may
//!   be from, so none is accepted twice (group D).
//! - [`RecoveryCode`] and [`SecondFactor`]: the recovery codes, and what the
//!   login's one code field holds (group D).
//! - [`challenge_expired`] and [`used_up`]: when the login's mfa and setup
//!   tokens are over (group D).
//! - [`InviteTtl`], [`invite_state`] and [`pending_cap_reached`]: how long an
//!   invite lives, what a stored one is, and how many a user may hold
//!   (group E).

mod access;
mod challenge;
mod invite;
mod lockout;
mod password;
mod recovery;
mod session;
mod strength;
mod totp;

pub use access::{
    AccessFacts, AccessRejection, STEP_UP_WINDOW, TOUCH_INTERVAL, access_decision, is_stepped_up,
    touch_due, touch_stale_since,
};
pub use challenge::{
    EVENT_TICKET_TTL, MFA_ATTEMPTS, MFA_TOKEN_TTL, SETUP_TOKEN_TTL, challenge_expired, expires_in,
    used_up,
};
pub use invite::{
    InviteFacts, InviteState, InviteTtl, InviteTtlError, MAX_PENDING_INVITES, invite_state,
    pending_cap_reached,
};
pub use lockout::{
    FAILURE_MEMORY, FIRST_LOCK, FailureOutcome, FailureStreak, Locked, LockoutPolicy,
    LockoutPolicyError, MAX_LOCK, after_failure, check_lock, expired_failed_by, is_expired,
};
pub use password::{NewPassword, NewPasswordError};
pub use recovery::{RECOVERY_CODE_LEN, RECOVERY_CODES, RecoveryCode, SecondFactor};
pub use session::{
    RefreshDecision, RefreshFacts, RefreshMatch, SessionPolicy, SessionPolicyError, SessionTimes,
    refresh_decision,
};
pub use strength::{WeakSuggestion, WeakWarning};
pub use totp::{
    TOTP_DIGITS, TOTP_ISSUER, TOTP_SECRET_LEN, TOTP_SKEW, TOTP_STEP, TotpCode, candidate_steps,
    current_step, step_start,
};
