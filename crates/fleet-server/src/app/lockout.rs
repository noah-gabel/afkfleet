//! [`LockoutService`]: the per-username lockout (Plan.md P7.10, §7.4,
//! ADR-0016), for D's login and step-up.
//!
//! - **Keys, not names:** a username's row is keyed by an HMAC of its
//!   normalized form ([`AttemptKeys`]), computed before any transaction. A
//!   name that belongs to no user takes exactly the same path as a real
//!   user's, so the lockout can't tell anyone which names exist.
//! - **[`LockoutService::begin`]** starts an attempt, before any password
//!   check:
//!   - **One attempt per name in flight:** a concurrent one is refused
//!     ([`LockoutError::InFlight`], answered 429 with `Retry-After: 1`), so
//!     parallel requests can't all pass the check before the first failure
//!     is written. The guard is released when the [`Attempt`] is dropped.
//!   - **While a lock runs:** [`LockoutError::Locked`] with its rest
//!     (`fleet_core::auth::check_lock`). The refusal isn't a failure, so it
//!     can't extend the lock.
//! - **[`Attempt::record_failure`]** counts a failure inside the caller's
//!   transaction (`fleet_core::auth::after_failure`), so it commits with the
//!   caller's own entry. When the failure starts a lock, it adds
//!   `security.lockout` as a second entry and logs one `info` line, with
//!   the user's ID when the name is a user's, never the name.
//! - **[`Attempt::clear`]** forgets the streak, for a successful login.
//! - **[`LockoutService::purge_stale`]** deletes expired streaks for the
//!   server's housekeeping, in batches of [`PURGE_BATCH`], each its own
//!   transaction committed with [`UnauditedWrite::LoginAttemptsPurge`], until
//!   a batch comes back short or the server stops.
//! - **Refusals are counted** ([`LockoutService::take_refusals`]) for the
//!   housekeeping's summary, like the rate limits'.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::{
    FailureStreak, LockoutPolicy, after_failure, check_lock, expired_failed_by,
};
use fleet_core::id::UserId;
use fleet_core::system::Clock;
use fleet_core::value::Username;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::audit::AuditService;
use crate::ports::login_attempts::{AttemptKey, AttemptKeys, LoginAttempt};
use crate::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};

/// The most rows one purge batch deletes, each batch in its own short
/// transaction.
pub const PURGE_BATCH: u32 = 1000;

/// How long a client refused for an attempt already in flight waits.
pub const IN_FLIGHT_RETRY: Duration = Duration::from_secs(1);

/// Why an attempt can't start.
#[derive(Debug, thiserror::Error)]
pub enum LockoutError {
    /// The name is locked; a login may be tried again after `retry_after`.
    #[error("the username is locked")]
    Locked {
        /// The rest of the lock.
        retry_after: Duration,
    },
    /// Another attempt for the same name is in flight.
    #[error("another login for the username is in flight")]
    InFlight,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a failure couldn't be recorded.
#[derive(Debug, thiserror::Error)]
pub enum RecordFailureError {
    /// The lock's audit entry couldn't be built.
    #[error("the lockout's audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// How many attempts the lockout refused since the last
/// [`LockoutService::take_refusals`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockoutRefusals {
    /// Refused while their name was locked.
    pub locked: u64,
    /// Refused while another attempt for their name was in flight.
    pub in_flight: u64,
}

impl LockoutRefusals {
    /// Whether nothing was refused.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.locked == 0 && self.in_flight == 0
    }
}

/// What the service shares between its clones.
#[derive(Debug, Default)]
struct Shared {
    /// The keys of the attempts in flight.
    in_flight: Mutex<HashSet<[u8; AttemptKey::LEN]>>,
    /// Refusals while locked.
    locked: AtomicU64,
    /// Refusals while another attempt was in flight.
    refused_in_flight: AtomicU64,
}

impl Shared {
    /// Locks the set. A panic while holding it leaves a consistent set, so a
    /// poisoned lock is used as it is.
    fn in_flight(&self) -> MutexGuard<'_, HashSet<[u8; AttemptKey::LEN]>> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The per-username lockout; clones share the attempts in flight and the
/// counts. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct LockoutService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    keys: Arc<dyn AttemptKeys>,
    policy: LockoutPolicy,
    audit: AuditService,
    shared: Arc<Shared>,
}

/// One attempt for a username, in flight until it's dropped.
#[derive(Debug)]
pub struct Attempt {
    service: LockoutService,
    key: AttemptKey,
}

impl LockoutService {
    /// The lockout over `store`, keying rows with `keys`, stamping with
    /// `clock` (the wall clock: the rows and the entries are stored), with
    /// `policy`'s threshold.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        store: Arc<dyn Store>,
        keys: Arc<dyn AttemptKeys>,
        policy: LockoutPolicy,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            store,
            keys,
            policy,
            audit,
            shared: Arc::default(),
        }
    }

    /// Starts an attempt for `username`, before its password is checked;
    /// see the [module docs](self).
    ///
    /// # Errors
    /// [`LockoutError::InFlight`], [`LockoutError::Locked`], or
    /// [`LockoutError::Store`] when the database fails.
    pub async fn begin(&self, username: &Username) -> Result<Attempt, LockoutError> {
        let key = self.keys.key(username);
        if !self.shared.in_flight().insert(*key.as_bytes()) {
            self.shared
                .refused_in_flight
                .fetch_add(1, Ordering::Relaxed);
            return Err(LockoutError::InFlight);
        }
        // The attempt holds the name from here on; dropping it, on any return,
        // releases it.
        let attempt = Attempt {
            service: self.clone(),
            key,
        };
        let found = self.store.login_attempts().find(&key).await?;
        if let Err(locked) = check_lock(found.map(streak).as_ref(), self.clock.now()) {
            self.shared.locked.fetch_add(1, Ordering::Relaxed);
            return Err(LockoutError::Locked {
                retry_after: locked.remaining,
            });
        }
        Ok(attempt)
    }

    /// Deletes every expired streak without a running lock, in batches of
    /// [`PURGE_BATCH`], each committed on its own, until a batch comes back
    /// short or `stop` is cancelled (checked before each batch). How many
    /// it deleted.
    ///
    /// # Errors
    /// [`StoreError`] when a batch fails; the batches before it stay
    /// committed.
    pub async fn purge_stale(&self, stop: &CancellationToken) -> Result<u64, StoreError> {
        let mut deleted = 0_u64;
        while !stop.is_cancelled() {
            let now = self.clock.now();
            let mut tx = self.store.write().await?;
            let batch = tx
                .login_attempts()
                .purge_stale(expired_failed_by(now), now, PURGE_BATCH)
                .await?;
            tx.commit_unaudited(UnauditedWrite::LoginAttemptsPurge)
                .await?;
            deleted = deleted.saturating_add(batch);
            if batch < u64::from(PURGE_BATCH) {
                break;
            }
        }
        Ok(deleted)
    }

    /// The refusals since the last call, and starts counting again.
    #[must_use]
    pub fn take_refusals(&self) -> LockoutRefusals {
        LockoutRefusals {
            locked: self.shared.locked.swap(0, Ordering::Relaxed),
            in_flight: self.shared.refused_in_flight.swap(0, Ordering::Relaxed),
        }
    }
}

impl Attempt {
    /// Counts this attempt as a failure, inside the caller's transaction
    /// `tx`, which commits with the caller's own entry. `target` is the
    /// user whose name it was, if any, and `ip` the client's. When the
    /// failure starts a lock, `security.lockout` is recorded in `tx` too,
    /// and one `info` line logged. Returns when that lock ends, if this
    /// failure started one.
    ///
    /// # Errors
    /// [`RecordFailureError`]; nothing is committed then.
    pub async fn record_failure(
        &self,
        tx: &mut dyn WriteTx,
        target: Option<UserId>,
        ip: IpAddr,
    ) -> Result<Option<DateTime<Utc>>, RecordFailureError> {
        let service = &self.service;
        let found = tx.login_attempts().find(&self.key).await?;
        let outcome = after_failure(
            &service.policy,
            found.map(streak).as_ref(),
            service.clock.now(),
        );
        tx.login_attempts()
            .upsert(&self.key, &stored(outcome.streak))
            .await?;
        if let Some(until) = outcome.lock_started {
            let metadata = AuditMetadata::new()
                .int("failures", i64::from(outcome.streak.failures))?
                .time("locked_until", until)?;
            let mut entry = service
                .audit
                .entry(AuditAction::SECURITY_LOCKOUT, AuditOutcome::Failure)
                .ip(ip)
                .metadata(metadata);
            if let Some(user) = target {
                entry = entry.target(AuditTarget::User(user));
            }
            tx.audit().record(&entry).await?;
            info_line(target, outcome.streak.failures, until);
        }
        Ok(outcome.lock_started)
    }

    /// Forgets the streak, inside the caller's transaction `tx`, for a
    /// successful login.
    ///
    /// # Errors
    /// [`StoreError`]; nothing is committed then.
    pub async fn clear(&self, tx: &mut dyn WriteTx) -> Result<(), StoreError> {
        tx.login_attempts().delete(&self.key).await?;
        Ok(())
    }
}

/// Releases the name, so the next attempt for it can start.
impl Drop for Attempt {
    fn drop(&mut self) {
        self.service.shared.in_flight().remove(self.key.as_bytes());
    }
}

/// The streak as the rule sees it.
const fn streak(stored: LoginAttempt) -> FailureStreak {
    FailureStreak {
        failures: stored.failures,
        last_failure_at: stored.last_failure_at,
        locked_until: stored.locked_until,
    }
}

/// The streak as it's stored.
const fn stored(streak: FailureStreak) -> LoginAttempt {
    LoginAttempt {
        failures: streak.failures,
        last_failure_at: streak.last_failure_at,
        locked_until: streak.locked_until,
    }
}

/// The lock's one `info` line.
fn info_line(target: Option<UserId>, failures: u32, until: DateTime<Utc>) {
    info!(
        user_id = target.map(tracing::field::display),
        failures,
        locked_until = %until,
        "a username is locked after failed logins"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_streak_converts_both_ways() {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let row = LoginAttempt {
            failures: 6,
            last_failure_at: at,
            locked_until: Some(at),
        };

        assert_eq!(stored(streak(row)), row);
    }

    #[test]
    fn no_refusals_are_empty_and_any_isnt() {
        assert!(LockoutRefusals::default().is_empty());
        assert!(
            !LockoutRefusals {
                locked: 1,
                in_flight: 0
            }
            .is_empty()
        );
        assert!(
            !LockoutRefusals {
                locked: 0,
                in_flight: 2
            }
            .is_empty()
        );
    }
}
