//! The failed logins per username (Plan.md P7.1, P7.10, §7.4, ADR-0016):
//! [`AttemptKey`], what a row is keyed by, [`LoginAttempt`] as stored, and
//! the read and write traits.
//!
//! A row is keyed by an HMAC of the normalized username under the server's
//! lockout key, never by the name itself: people type their password into
//! the username field (P7.13's note). A name that belongs to no user gets a
//! row exactly like a real user's, so the lockout can't reveal which names
//! exist. The repository only stores and finds rows; `fleet_core::auth`
//! decides what a row means.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::value::Username;
use subtle::ConstantTimeEq as _;

use super::store::StoreError;

/// What a `login_attempts` row is keyed by: an HMAC-SHA256 of the
/// normalized username.
///
/// It has no `PartialEq`: two keys are compared only through
/// [`AttemptKey::ct_eq`], in constant time, and its `Debug` shows none of
/// its bytes.
#[derive(Clone, Copy)]
pub struct AttemptKey([u8; AttemptKey::LEN]);

/// Why stored bytes aren't an [`AttemptKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an attempt key has {expected} bytes, not {len}")]
pub struct AttemptKeyLengthError {
    /// The length found.
    pub len: usize,
    /// [`AttemptKey::LEN`].
    pub expected: usize,
}

impl AttemptKey {
    /// An HMAC-SHA256's length in bytes.
    pub const LEN: usize = 32;

    /// Wraps a key.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The key's bytes, as the database stores them.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Whether both keys are equal, compared in constant time.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

/// For reading a stored key back.
impl TryFrom<&[u8]> for AttemptKey {
    type Error = AttemptKeyLengthError;

    fn try_from(bytes: &[u8]) -> Result<Self, AttemptKeyLengthError> {
        <[u8; Self::LEN]>::try_from(bytes)
            .map(Self)
            .map_err(|_| AttemptKeyLengthError {
                len: bytes.len(),
                expected: Self::LEN,
            })
    }
}

/// Shows none of the key's bytes.
impl fmt::Debug for AttemptKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttemptKey(..)")
    }
}

/// Making a row's key from a username (P7.10). The only implementation is
/// infra's `HmacAttemptKeys`, an HMAC under a key derived from the vault's;
/// it takes microseconds, so it's synchronous.
pub trait AttemptKeys: Send + Sync + fmt::Debug {
    /// The key of `username`'s row: the same for every spelling that
    /// `Username` normalizes alike, whether or not a user has the name.
    fn key(&self, username: &Username) -> AttemptKey;
}

/// A username's running failure streak, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginAttempt {
    /// The failures of the streak, at least 1.
    pub failures: u32,
    /// The streak's last failure.
    pub last_failure_at: DateTime<Utc>,
    /// When the running lock ends, once the streak reached the threshold.
    pub locked_until: Option<DateTime<Utc>>,
}

/// Reading failure streaks through the read connections.
#[async_trait]
pub trait LoginAttemptReads: Send + Sync + fmt::Debug {
    /// The streak stored under `key`, if there is one, expired or not.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&self, key: &AttemptKey) -> Result<Option<LoginAttempt>, StoreError>;
}

/// Reading and writing failure streaks inside a
/// [`super::store::WriteTx`], so a failure's count is read and written
/// under the write lock.
#[async_trait]
pub trait LoginAttemptWrites: Send {
    /// The streak stored under `key`, if there is one, expired or not.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, key: &AttemptKey) -> Result<Option<LoginAttempt>, StoreError>;

    /// Stores `attempt` under `key`, replacing any streak stored there.
    ///
    /// # Errors
    /// [`StoreError`]: a time that can't be stored is
    /// [`StoreError::Unstorable`], a count of 0 [`StoreError::Backend`].
    async fn upsert(&mut self, key: &AttemptKey, attempt: &LoginAttempt) -> Result<(), StoreError>;

    /// Deletes the streak stored under `key`. Whether there was one.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn delete(&mut self, key: &AttemptKey) -> Result<bool, StoreError>;

    /// Deletes at most `limit` streaks whose last failure is at or before
    /// `failed_by` and that have no lock running at `now` (no lock, or one
    /// that ended at or before `now`). How many it deleted.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`] for a time that
    /// can't be stored.
    async fn purge_stale(
        &mut self,
        failed_by: DateTime<Utc>,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_keys_compare_equal_and_others_dont() {
        let key = AttemptKey::from_bytes([7; 32]);

        assert!(key.ct_eq(&AttemptKey::from_bytes([7; 32])));
        assert!(!key.ct_eq(&AttemptKey::from_bytes([8; 32])));
    }

    #[test]
    fn stored_bytes_of_the_right_length_read_back() {
        let key = AttemptKey::try_from([3_u8; 32].as_slice()).unwrap();

        assert_eq!(key.as_bytes(), &[3; 32]);
    }

    #[test]
    fn stored_bytes_of_another_length_are_refused() {
        assert_eq!(
            AttemptKey::try_from([3_u8; 31].as_slice()).unwrap_err(),
            AttemptKeyLengthError {
                len: 31,
                expected: 32
            }
        );
    }

    #[test]
    fn the_debug_form_shows_no_byte() {
        let shown = format!("{:?}", AttemptKey::from_bytes([0xAB; 32]));

        assert_eq!(shown, "AttemptKey(..)");
    }
}
