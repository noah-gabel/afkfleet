//! The ports through which the server reads the time and secure randomness
//! (ADR-0015).
//!
//! fleet-core itself never reads a clock or the OS's randomness: its
//! functions take `now` and an RNG as parameters (ADR-0010). The server needs
//! both for its own records (creation times, IDs, later tokens), so its
//! services hold a [`Clock`] and a [`SecureRandom`] instead of reading them
//! directly; fleet-server's `clippy.toml` bans the direct reads. The real
//! implementations, `SystemClock` and `OsRandom`, live in fleet-server's
//! `infra`. The test doubles, `ManualClock` and `SeededRandom`, live in
//! fleet-testkit, a dev-dependency only, so they can't reach a binary.
//!
//! [`mint`] makes an ID from a creation time and a [`SecureRandom`].

use core::fmt;

use chrono::{DateTime, Utc};

use crate::id::{IdError, V7Id};

/// Where the server reads the current time.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Returns the current time in UTC, truncated to whole milliseconds.
    ///
    /// The precision is part of the contract: milliseconds are what the
    /// database stores and what a version 7 ID holds, so a time the server
    /// keeps in memory compares equal to the same time loaded back.
    fn now(&self) -> DateTime<Utc>;
}

/// Where the server draws random bytes for IDs and, from Phase 7 on, for
/// secrets such as tokens and invite codes.
///
/// **Every implementation must be cryptographically secure** (security
/// rule 4). The only one outside tests is fleet-server's `OsRandom`, which
/// `main.rs` always wires; fleet-testkit's seeded fake is a dev-dependency
/// only.
pub trait SecureRandom: Send + Sync + fmt::Debug {
    /// Fills `dest` with random bytes.
    ///
    /// # Errors
    /// [`RandomError`] when the source fails. `dest` is then unspecified and
    /// must not be used.
    fn fill(&self, dest: &mut [u8]) -> Result<(), RandomError>;
}

/// Why a [`SecureRandom`] couldn't deliver bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RandomError {
    /// The OS reported an error number, kept for the operator.
    #[error("the OS's secure random source failed (OS error {code})")]
    Os {
        /// The OS's error number.
        code: i32,
    },
    /// The source failed without an error number.
    #[error("the secure random source failed")]
    Unavailable,
}

/// Why [`mint`] couldn't make an ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MintError {
    /// The random source failed.
    #[error("no random bytes for the ID")]
    Random(#[source] RandomError),
    /// The creation time doesn't fit a version 7 ID.
    #[error("the ID can't hold its creation time")]
    Id(#[source] IdError),
}

/// Returns `N` bytes from `random`.
///
/// # Errors
/// [`RandomError`] when the source fails.
pub fn random_bytes<const N: usize>(
    random: &(impl SecureRandom + ?Sized),
) -> Result<[u8; N], RandomError> {
    let mut bytes = [0; N];
    random.fill(&mut bytes)?;
    Ok(bytes)
}

/// Mints an ID created at `at`, with 10 random bytes from `random`.
///
/// The caller reads its [`Clock`] once and passes the time here and to the
/// record, so a record's creation time equals the time inside its ID.
///
/// # Errors
/// [`MintError::Random`] when the source fails, [`MintError::Id`] when `at`
/// is outside the range of a version 7 ID.
pub fn mint<I: V7Id>(
    at: DateTime<Utc>,
    random: &(impl SecureRandom + ?Sized),
) -> Result<I, MintError> {
    let bytes = random_bytes(random).map_err(MintError::Random)?;
    I::new_v7(at, bytes).map_err(MintError::Id)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::id::UserId;

    /// A source that counts up from 0, or always fails with `failure`.
    #[derive(Debug, Default)]
    struct Counting {
        next: Mutex<u8>,
        failure: Option<RandomError>,
    }

    impl SecureRandom for Counting {
        fn fill(&self, dest: &mut [u8]) -> Result<(), RandomError> {
            if let Some(error) = self.failure {
                return Err(error);
            }
            let mut next = self.next.lock().unwrap();
            for byte in dest {
                *byte = *next;
                *next = next.wrapping_add(1);
            }
            Ok(())
        }
    }

    fn failing(error: RandomError) -> Counting {
        Counting {
            failure: Some(error),
            ..Counting::default()
        }
    }

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    #[test]
    fn random_bytes_takes_every_byte_from_the_source() {
        let source = Counting::default();

        let first: [u8; 4] = random_bytes(&source).unwrap();
        let second: [u8; 3] = random_bytes(&source).unwrap();

        assert_eq!(first, [0, 1, 2, 3]);
        assert_eq!(second, [4, 5, 6]);
    }

    #[test]
    fn random_bytes_passes_the_error_through() {
        let source = failing(RandomError::Os { code: 5 });

        assert_eq!(random_bytes::<8>(&source), Err(RandomError::Os { code: 5 }));
    }

    #[test]
    fn mint_uses_the_time_and_the_next_ten_bytes() {
        let source = Counting::default();
        let created = at(1_800_000_000_123);

        let id: UserId = mint(created, &source).unwrap();

        let expected = UserId::new_v7(created, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();
        assert_eq!(id, expected);
    }

    #[test]
    fn mint_works_through_a_trait_object() {
        let source: &dyn SecureRandom = &Counting::default();

        let id: UserId = mint(at(1_800_000_000_000), source).unwrap();

        assert_eq!(
            id,
            UserId::new_v7(at(1_800_000_000_000), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap()
        );
    }

    #[test]
    fn mint_reports_a_failed_source() {
        let source = failing(RandomError::Unavailable);

        assert_eq!(
            mint::<UserId>(at(1_800_000_000_000), &source),
            Err(MintError::Random(RandomError::Unavailable))
        );
    }

    #[test]
    fn mint_reports_a_time_outside_the_id_range() {
        let source = Counting::default();

        assert_eq!(
            mint::<UserId>(at(-1), &source),
            Err(MintError::Id(IdError::TimestampOutOfRange))
        );
    }

    #[test]
    fn random_errors_name_the_os_code() {
        assert_eq!(
            RandomError::Os { code: 5 }.to_string(),
            "the OS's secure random source failed (OS error 5)"
        );
        assert_eq!(
            RandomError::Unavailable.to_string(),
            "the secure random source failed"
        );
    }
}
