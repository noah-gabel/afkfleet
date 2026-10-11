//! The password service's port: [`Passwords`] hashes new passwords and
//! checks passwords against stored hashes (Plan.md P7.2, ADR-0016).
//!
//! Passwords go only through it (security rule 5). Its errors name no crate's
//! type, as [`super::store::StoreError`] names no sqlx type.

use core::fmt;

use async_trait::async_trait;
use fleet_core::auth::{NewPassword, WeakSuggestion, WeakWarning};
use fleet_core::system::RandomError;
use fleet_core::value::Username;
use secrecy::SecretString;

use super::users::PasswordHash;

/// Why a new password couldn't be hashed.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// The strength check refused it (§7.2: zxcvbn score below 3). The hints
    /// describe the pattern the check found, never the password.
    #[error("the password is too weak")]
    Weak {
        /// What made the check refuse it, if it says.
        warning: Option<WeakWarning>,
        /// What would make it stronger.
        suggestions: Vec<WeakSuggestion>,
    },
    /// Every hashing slot and every waiting place is taken: the caller
    /// answers `busy` (503).
    #[error("the password service is busy")]
    Busy,
    /// The OS's randomness failed while drawing the salt.
    #[error("no salt could be drawn")]
    Random(#[from] RandomError),
    /// Hashing itself failed, which a valid parameter set never does, or the
    /// blocking task panicked.
    #[error("hashing failed")]
    Failed(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Why a password couldn't be checked. A wrong password isn't an error: it's
/// `Ok(false)`.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// Every hashing slot and every waiting place is taken: the caller
    /// answers `busy` (503).
    #[error("the password service is busy")]
    Busy,
    /// The stored hash isn't one this server writes: another algorithm or
    /// version than argon2id v19, a string that doesn't parse, or parameters
    /// above the config's upper bounds. Something else wrote the row, so it's
    /// an internal error, never "wrong password".
    #[error("the stored password hash isn't one this server writes")]
    Corrupt,
    /// Verifying failed for another reason, or the blocking task panicked.
    #[error("verifying failed")]
    Failed(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Hashing and checking passwords (§7.2). Every call runs in a blocking task
/// behind one bounded queue, so too many at once answer [`HashError::Busy`]
/// or [`VerifyError::Busy`] instead of waiting.
#[async_trait]
pub trait Passwords: Send + Sync + fmt::Debug {
    /// Checks `password`'s strength, with `username` among the words a
    /// strong password must not be built from, then hashes it.
    ///
    /// # Errors
    /// [`HashError::Weak`], [`HashError::Busy`], [`HashError::Random`] or
    /// [`HashError::Failed`].
    async fn hash(
        &self,
        password: &NewPassword,
        username: &Username,
    ) -> Result<PasswordHash, HashError>;

    /// Whether `password` matches `hash`.
    ///
    /// # Errors
    /// [`VerifyError::Corrupt`] for a hash this server doesn't write,
    /// [`VerifyError::Busy`] or [`VerifyError::Failed`].
    async fn verify(
        &self,
        password: &SecretString,
        hash: &PasswordHash,
    ) -> Result<bool, VerifyError>;

    /// Does exactly the work of [`Passwords::verify`], against a dummy hash
    /// with the configured parameters, and always answers `false`. Login calls
    /// it for an unknown username, so it costs the same time as a real one
    /// (§7.2).
    ///
    /// # Errors
    /// [`VerifyError::Busy`] or [`VerifyError::Failed`], like
    /// [`Passwords::verify`].
    async fn verify_unknown_user(&self, password: &SecretString) -> Result<bool, VerifyError>;

    /// Whether `hash` differs from what [`Passwords::hash`] would write now:
    /// another variant or version, or other m, t or p, up or down. Such a
    /// hash verifies with other timing than the dummy hash, so login
    /// re-hashes it (ADR-0016). A hash that doesn't parse needs one too.
    fn needs_rehash(&self, hash: &PasswordHash) -> bool;

    /// Hashes `password` again with the current parameters, in the same
    /// queue. Only for a password that just verified against a hash
    /// [`Passwords::needs_rehash`] flagged: it skips the strength check,
    /// since the password was accepted under its own rules. Never for a new
    /// password, which goes through [`Passwords::hash`].
    ///
    /// # Errors
    /// [`HashError::Busy`], [`HashError::Random`] or [`HashError::Failed`].
    async fn rehash_verified(&self, password: &SecretString) -> Result<PasswordHash, HashError>;
}
