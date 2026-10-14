//! An account's [`Slot`]: its cached session, its backoff and its flight,
//! and the pure rules they follow.
//!
//! The slot's state is behind a std `Mutex`, which no code holds across an
//! `.await`, so a cache hit or a backoff never waits for a flight. The
//! flight is a tokio `Mutex` whose owned guard the refresh task holds until
//! it ends: at most one refresh per account, even when its caller is gone.

use core::num::NonZeroU32;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::resilience::Doubling;
use fleet_core::time;
use tokio::sync::Mutex as AsyncMutex;

use super::REUSE_MARGIN;
use crate::ports::msauth::{MinecraftSession, RefreshToken};

/// One account's slot. Slots are created only for accounts that may get a
/// session and are never removed, so an account never has two flights.
pub(super) struct Slot {
    state: Mutex<SlotState>,
    /// The flight: whoever holds it may refresh the account's token.
    pub(super) flight: Arc<AsyncMutex<Flight>>,
}

/// What a slot remembers between requests.
#[derive(Default)]
struct SlotState {
    cached: Option<Cached>,
    /// The number of the last minting started; a fresh request reuses only
    /// a session minted after it arrived.
    mint: u64,
    backoff: Backoff,
}

/// A session minted for the account, and the number of its minting.
struct Cached {
    session: MinecraftSession,
    mint: u64,
}

/// What only the flight's holder touches.
#[derive(Default)]
pub(super) struct Flight {
    /// A rotated refresh token whose store failed: the next flight stores
    /// it first.
    pub(super) unsaved: Option<Unsaved>,
}

/// A rotated refresh token that isn't stored yet.
pub(super) struct Unsaved {
    /// The token Microsoft answered with.
    pub(super) token: RefreshToken,
    /// The envelope it replaces: it's stored only while the row still holds
    /// this one.
    pub(super) replaces: Vec<u8>,
    /// Whether it differs from the token that was sent.
    pub(super) rotated: bool,
}

impl Slot {
    /// An empty slot.
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::default(),
            flight: Arc::default(),
        }
    }

    /// The state, poison-tolerantly.
    fn state(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The number of the last minting started.
    pub(super) fn mint(&self) -> u64 {
        self.state().mint
    }

    /// Starts a minting and returns its number.
    pub(super) fn begin_mint(&self) -> u64 {
        let mut state = self.state();
        state.mint = state.mint.saturating_add(1);
        state.mint
    }

    /// The cached session, if it may be handed out at `now`: see
    /// [`reusable`].
    pub(super) fn reusable(
        &self,
        now: DateTime<Utc>,
        fresh: bool,
        seen: u64,
    ) -> Option<MinecraftSession> {
        self.state()
            .cached
            .as_ref()
            .filter(|cached| reusable(cached.session.expires_at, cached.mint, now, fresh, seen))
            .map(|cached| cached.session.clone())
    }

    /// Caches `session`, minted by the minting `mint`.
    pub(super) fn cache(&self, session: &MinecraftSession, mint: u64) {
        self.state().cached = Some(Cached {
            session: session.clone(),
            mint,
        });
    }

    /// Drops the cached session.
    pub(super) fn clear_cache(&self) {
        self.state().cached = None;
    }

    /// Until when this account's backoff runs at `now`, if it does.
    pub(super) fn backoff_until(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.state().backoff.active_until(now)
    }

    /// Records a failure at `now` and returns when the backoff ends.
    pub(super) fn fail(&self, policy: Doubling, now: DateTime<Utc>) -> DateTime<Utc> {
        self.state().backoff.fail(policy, now)
    }

    /// Records a success: the backoff ends.
    pub(super) fn succeed(&self) {
        self.state().backoff.reset();
    }
}

/// Whether a session expiring at `expires_at`, minted by the minting `mint`,
/// may be handed out at `now`: more than [`REUSE_MARGIN`] left, and, for a
/// fresh request that saw the minting `seen` when it arrived, minted by a
/// later minting.
pub(super) fn reusable(
    expires_at: DateTime<Utc>,
    mint: u64,
    now: DateTime<Utc>,
    fresh: bool,
    seen: u64,
) -> bool {
    let minted_after = !fresh || mint > seen;
    minted_after && time::elapsed(now, expires_at) > REUSE_MARGIN
}

/// A run of failures and the wait after the last one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Backoff {
    failures: u32,
    until: Option<DateTime<Utc>>,
}

impl Backoff {
    /// Records a failure at `now`: the wait is `policy`'s for the run's
    /// length. Returns when it ends.
    pub(super) fn fail(&mut self, policy: Doubling, now: DateTime<Utc>) -> DateTime<Utc> {
        self.failures = self.failures.saturating_add(1);
        let until = policy.until(now, run_length(self.failures));
        self.until = Some(until);
        until
    }

    /// Ends the run and its wait.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Ends the run but lets a running wait run out: the next failure
    /// starts from the first delay again.
    pub(super) fn reset_streak(&mut self) {
        self.failures = 0;
    }

    /// Until when the wait runs at `now`, if it does: it ends from its exact
    /// millisecond on.
    pub(super) fn active_until(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.until.filter(|until| *until > now)
    }
}

/// The later of two waits.
pub(super) fn later(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The number of failures as the policy takes it: at least one.
fn run_length(failures: u32) -> NonZeroU32 {
    NonZeroU32::new(failures).unwrap_or(NonZeroU32::MIN)
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use rstest::rstest;

    use super::*;

    const T0: i64 = 1_800_000_000_000;
    const POLICY: Doubling = Doubling::new(Duration::from_secs(5), Duration::from_secs(20));

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    /// `millis` after the margin starts, for a session expiring at `T0 + 1 h`.
    fn margin_offset(millis: i64) -> DateTime<Utc> {
        let margin = i64::try_from(REUSE_MARGIN.as_millis()).unwrap();
        at(T0 + 3_600_000 - margin + millis)
    }

    #[rstest]
    #[case::well_before(at(T0), true)]
    #[case::one_ms_before_the_margin(margin_offset(-1), true)]
    #[case::at_the_margin(margin_offset(0), false)]
    #[case::inside_the_margin(margin_offset(1), false)]
    #[case::expired(at(T0 + 3_600_000), false)]
    #[case::long_expired(at(T0 + 7_200_000), false)]
    fn a_session_is_reused_while_more_than_the_margin_is_left(
        #[case] now: DateTime<Utc>,
        #[case] reused: bool,
    ) {
        assert_eq!(reusable(at(T0 + 3_600_000), 1, now, false, 5), reused);
    }

    #[rstest]
    #[case::minted_before_the_request(3, 3, false)]
    #[case::minted_long_before(1, 3, false)]
    #[case::minted_after_the_request(4, 3, true)]
    #[case::first_minting_after_an_empty_slot(1, 0, true)]
    fn a_fresh_request_reuses_only_a_later_minting(
        #[case] mint: u64,
        #[case] seen: u64,
        #[case] reused: bool,
    ) {
        assert_eq!(
            reusable(at(T0 + 3_600_000), mint, at(T0), true, seen),
            reused
        );
    }

    #[test]
    fn a_fresh_request_still_needs_the_margin() {
        assert!(!reusable(margin_offset(0), 4, margin_offset(0), true, 3));
    }

    #[test]
    fn failures_double_the_wait_up_to_the_policys_maximum() {
        let mut backoff = Backoff::default();

        let waits: Vec<DateTime<Utc>> = (0..4).map(|_| backoff.fail(POLICY, at(T0))).collect();

        assert_eq!(
            waits,
            [
                at(T0 + 5_000),
                at(T0 + 10_000),
                at(T0 + 20_000),
                at(T0 + 20_000)
            ]
        );
    }

    #[test]
    fn the_wait_ends_from_its_exact_millisecond_on() {
        let mut backoff = Backoff::default();
        let until = backoff.fail(POLICY, at(T0));

        assert_eq!(backoff.active_until(at(T0)), Some(until));
        assert_eq!(backoff.active_until(at(T0 + 4_999)), Some(until));
        assert_eq!(backoff.active_until(at(T0 + 5_000)), None);
    }

    #[test]
    fn a_reset_ends_the_wait_and_the_run() {
        let mut backoff = Backoff::default();
        backoff.fail(POLICY, at(T0));
        backoff.fail(POLICY, at(T0));

        backoff.reset();

        assert_eq!(backoff.active_until(at(T0)), None);
        assert_eq!(backoff.fail(POLICY, at(T0)), at(T0 + 5_000));
    }

    #[test]
    fn a_reset_streak_lets_the_wait_run_out_but_starts_the_run_over() {
        let mut backoff = Backoff::default();
        backoff.fail(POLICY, at(T0));
        let until = backoff.fail(POLICY, at(T0));

        backoff.reset_streak();

        assert_eq!(backoff.active_until(at(T0)), Some(until));
        assert_eq!(backoff.fail(POLICY, at(T0 + 1)), at(T0 + 5_001));
    }

    #[rstest]
    #[case::none(None, None, None)]
    #[case::first(Some(at(T0 + 9)), None, Some(at(T0 + 9)))]
    #[case::second(None, Some(at(T0 + 9)), Some(at(T0 + 9)))]
    #[case::the_later_of_both(Some(at(T0 + 5)), Some(at(T0 + 9)), Some(at(T0 + 9)))]
    #[case::the_later_first(Some(at(T0 + 9)), Some(at(T0 + 5)), Some(at(T0 + 9)))]
    fn the_later_wait_wins(
        #[case] a: Option<DateTime<Utc>>,
        #[case] b: Option<DateTime<Utc>>,
        #[case] expected: Option<DateTime<Utc>>,
    ) {
        assert_eq!(later(a, b), expected);
    }

    #[test]
    fn a_run_is_at_least_one_failure_long() {
        assert_eq!(run_length(0).get(), 1);
        assert_eq!(run_length(3).get(), 3);
    }
}
