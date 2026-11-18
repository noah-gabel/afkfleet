//! [`SecondBudget`]: how many operations a script may still run right now,
//! and for how long (ADR-0036, D3; ADR-0040's amendment).

use core::num::NonZeroU64;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::script::limits;
use fleet_core::time;

use crate::error::Limit;

/// How long a slice of time is that the operations are counted in.
const SLICE: Duration = Duration::from_millis(50);

/// How many slices back still count: a second's worth and one more, so
/// that everything within the last second is always among them, wherever
/// in its slice a moment falls.
const SLICES: usize = 21;

/// The operations a script ran lately and the time they took, and what's
/// left of its budgets: [`OPERATIONS_PER_HOOK`] and [`HOOK_TIME`] in one
/// hook, [`OPERATIONS_PER_SECOND`] and [`TIME_PER_SECOND`] in any one
/// second over all its hooks.
///
/// A hook counts at the moment it starts. The counts are kept in slices of
/// 50 ms; a second is judged over 21 of them, which errs towards refusing:
/// no second, whenever it starts, ever holds more than its budget.
///
/// It holds no clock; a moment earlier than the last one counts as the
/// last one, so a clock that goes back frees nothing.
///
/// [`OPERATIONS_PER_HOOK`]: limits::OPERATIONS_PER_HOOK
/// [`OPERATIONS_PER_SECOND`]: limits::OPERATIONS_PER_SECOND
/// [`HOOK_TIME`]: limits::HOOK_TIME
/// [`TIME_PER_SECOND`]: limits::TIME_PER_SECOND
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SecondBudget {
    /// The slices that hold something, oldest first: each slice's start,
    /// its operations and their time. At most [`SLICES`] of them.
    slices: Vec<(DateTime<Utc>, u64, Duration)>,
    /// The latest moment seen.
    latest: Option<DateTime<Utc>>,
}

/// What a hook may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Allowance {
    /// How many operations: never zero, which Rhai would take for "no
    /// limit".
    pub(crate) operations: NonZeroU64,
    /// Whether that's less than a hook's own budget, so that running out
    /// of it means the second's budget ran out.
    pub(crate) by_second: bool,
    /// For how long: never nothing.
    pub(crate) time: Duration,
    /// Whether that's less than a hook's own time, so that running past
    /// it means the second's time ran out.
    pub(crate) time_by_second: bool,
}

impl SecondBudget {
    /// `now`, or the latest moment seen if `now` is earlier.
    fn moment(&mut self, now: DateTime<Utc>) -> DateTime<Utc> {
        let moment = self.latest.map_or(now, |latest| latest.max(now));
        self.latest = Some(moment);
        moment
    }

    /// Forgets the slices that no second ending at `moment` reaches.
    fn forget(&mut self, moment: DateTime<Utc>) {
        let reach = SLICE.saturating_mul(u32::try_from(SLICES).unwrap_or(u32::MAX));
        self.slices
            .retain(|(start, ..)| time::elapsed(*start, moment) < reach);
    }

    /// What a hook that starts at `now` may run: a hook's operations and
    /// time, or what's left of the second's if that's less.
    ///
    /// # Errors
    /// The second's limit of which nothing is left: the hook must not run
    /// at all.
    pub(crate) fn allowance(&mut self, now: DateTime<Utc>) -> Result<Allowance, Limit> {
        let moment = self.moment(now);
        self.forget(moment);
        let (used, taken) = self.slices.iter().fold(
            (0_u64, Duration::ZERO),
            |(used, taken), (_, operations, time)| {
                (
                    used.saturating_add(*operations),
                    taken.saturating_add(*time),
                )
            },
        );
        let left = limits::OPERATIONS_PER_SECOND.saturating_sub(used);
        let operations = NonZeroU64::new(left.min(limits::OPERATIONS_PER_HOOK))
            .ok_or(Limit::OperationsPerSecond)?;
        let time_left = limits::TIME_PER_SECOND.saturating_sub(taken);
        if time_left.is_zero() {
            return Err(Limit::TimePerSecond);
        }
        Ok(Allowance {
            operations,
            by_second: left < limits::OPERATIONS_PER_HOOK,
            time: time_left.min(limits::HOOK_TIME),
            time_by_second: time_left < limits::HOOK_TIME,
        })
    }

    /// Counts the `operations` a hook that started at `now` ran, and the
    /// `time` they took.
    pub(crate) fn spend(&mut self, now: DateTime<Utc>, operations: u64, time: Duration) {
        if operations == 0 && time.is_zero() {
            return;
        }
        let moment = self.moment(now);
        self.forget(moment);
        match self.slices.last_mut() {
            Some((start, used, taken)) if time::elapsed(*start, moment) < SLICE => {
                *used = used.saturating_add(operations);
                *taken = taken.saturating_add(time);
            }
            _ => self.slices.push((moment, operations, time)),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use proptest::prelude::*;

    use super::*;

    fn start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
    }

    fn after(ms: u64) -> DateTime<Utc> {
        time::add(start(), Duration::from_millis(ms))
    }

    fn left(budget: &mut SecondBudget, now: DateTime<Utc>) -> Option<u64> {
        budget
            .allowance(now)
            .ok()
            .map(|allowance| allowance.operations.get())
    }

    /// What's left of the second's time for a hook at `now`.
    fn time_left(budget: &mut SecondBudget, now: DateTime<Utc>) -> Option<Duration> {
        budget.allowance(now).ok().map(|allowance| allowance.time)
    }

    /// A whole hook's allowance.
    fn whole() -> Allowance {
        Allowance {
            operations: NonZeroU64::new(10_000).unwrap(),
            by_second: false,
            time: MS * 5,
            time_by_second: false,
        }
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_fresh_budget_allows_a_whole_hook() {
        let mut budget = SecondBudget::default();

        assert_eq!(budget.allowance(start()), Ok(whole()));
    }

    #[test]
    fn what_hooks_ran_within_the_second_is_taken_off() {
        let mut budget = SecondBudget::default();

        budget.spend(after(0), 10_000, Duration::ZERO);
        budget.spend(after(100), 10_000, Duration::ZERO);
        // 10,000 are left of the second: exactly a hook's own budget.
        assert_eq!(budget.allowance(after(200)), Ok(whole()));
        budget.spend(after(200), 4_000, Duration::ZERO);

        assert_eq!(
            budget.allowance(after(300)),
            Ok(Allowance {
                operations: NonZeroU64::new(6_000).unwrap(),
                by_second: true,
                ..whole()
            })
        );
    }

    // The user's reviewer's finding 2: with nothing left the answer is
    // "don't run", never a budget of 0, which Rhai takes for "no limit".
    #[test]
    fn with_nothing_left_the_hook_is_refused_and_zero_is_never_handed_out() {
        let mut budget = SecondBudget::default();
        budget.spend(after(0), 10_000, Duration::ZERO);
        budget.spend(after(0), 10_000, Duration::ZERO);
        budget.spend(after(0), 9_999, Duration::ZERO);
        assert_eq!(left(&mut budget, after(0)), Some(1));

        budget.spend(after(0), 1, Duration::ZERO);

        assert_eq!(budget.allowance(after(0)), Err(Limit::OperationsPerSecond));
        assert_eq!(
            budget.allowance(after(500)),
            Err(Limit::OperationsPerSecond)
        );
        // More than the budget was never spent, but if it were, nothing
        // would be left either.
        budget.spend(after(500), 50_000, Duration::ZERO);
        assert_eq!(
            budget.allowance(after(600)),
            Err(Limit::OperationsPerSecond)
        );
    }

    #[test]
    fn the_time_hooks_took_within_the_second_is_taken_off() {
        let mut budget = SecondBudget::default();

        budget.spend(after(0), 10, MS * 5);
        budget.spend(after(100), 10, MS * 5);
        budget.spend(after(200), 10, MS * 5);
        // 5 ms are left of the second's 20: exactly a hook's own time.
        assert_eq!(budget.allowance(after(300)), Ok(whole()));
        budget.spend(after(300), 10, MS * 2);

        assert_eq!(
            budget.allowance(after(400)),
            Ok(Allowance {
                time: MS * 3,
                time_by_second: true,
                ..whole()
            })
        );
    }

    #[test]
    fn with_no_time_left_the_hook_is_refused() {
        let mut budget = SecondBudget::default();
        // All of a second's 20 ms but a nanosecond.
        budget.spend(after(0), 1, Duration::from_nanos(19_999_999));
        assert_eq!(
            time_left(&mut budget, after(0)),
            Some(Duration::from_nanos(1))
        );

        budget.spend(after(0), 0, Duration::from_nanos(1));

        assert_eq!(budget.allowance(after(0)), Err(Limit::TimePerSecond));
        assert_eq!(budget.allowance(after(1_049)), Err(Limit::TimePerSecond));
        assert_eq!(budget.allowance(after(1_050)), Ok(whole()));
    }

    #[test]
    fn with_neither_left_the_operations_are_named() {
        let mut budget = SecondBudget::default();

        budget.spend(after(0), 30_000, MS * 20);

        assert_eq!(budget.allowance(after(0)), Err(Limit::OperationsPerSecond));
    }

    #[test]
    fn a_hook_that_took_time_and_ran_nothing_still_counts() {
        let mut budget = SecondBudget::default();

        budget.spend(after(0), 0, MS * 4);

        assert_eq!(budget.slices.len(), 1);
        assert_eq!(time_left(&mut budget, after(10)), Some(MS * 5));
        budget.spend(after(10), 0, MS * 14);
        assert_eq!(time_left(&mut budget, after(20)), Some(MS * 2));
    }

    #[test]
    fn the_budget_is_whole_again_once_a_second_and_a_slice_have_passed() {
        let mut budget = SecondBudget::default();
        budget.spend(after(0), 30_000, Duration::ZERO);

        assert_eq!(left(&mut budget, after(999)), None);
        assert_eq!(left(&mut budget, after(1_049)), None);
        assert_eq!(left(&mut budget, after(1_050)), Some(10_000));
    }

    #[test]
    fn operations_come_back_slice_by_slice() {
        let mut budget = SecondBudget::default();
        budget.spend(after(0), 10_000, Duration::ZERO);
        budget.spend(after(400), 10_000, Duration::ZERO);
        budget.spend(after(800), 10_000, Duration::ZERO);

        assert_eq!(left(&mut budget, after(1_000)), None);
        assert_eq!(left(&mut budget, after(1_050)), Some(10_000));
        budget.spend(after(1_050), 10_000, Duration::ZERO);
        assert_eq!(left(&mut budget, after(1_400)), None);
        assert_eq!(left(&mut budget, after(1_450)), Some(10_000));
    }

    #[test]
    fn a_hook_that_ran_nothing_costs_nothing_and_keeps_nothing() {
        let mut budget = SecondBudget::default();

        for ms in 0..5_000 {
            budget.spend(after(ms), 0, Duration::ZERO);
        }

        assert_eq!(budget.slices.len(), 0);
        assert_eq!(left(&mut budget, after(5_000)), Some(10_000));
    }

    #[test]
    fn it_never_keeps_more_slices_than_reach_a_second() {
        let mut budget = SecondBudget::default();

        for ms in 0..20_000 {
            budget.spend(after(ms), 1, Duration::ZERO);
        }

        assert!(budget.slices.len() <= SLICES, "{}", budget.slices.len());
    }

    #[test]
    fn a_clock_that_goes_back_frees_nothing() {
        let mut budget = SecondBudget::default();
        budget.spend(after(5_000), 30_000, Duration::ZERO);

        // An hour "earlier" counts as the latest moment seen.
        assert_eq!(
            budget.allowance(start() - Duration::from_secs(3_600)),
            Err(Limit::OperationsPerSecond)
        );
        budget.spend(after(0), 100, Duration::ZERO);
        assert_eq!(left(&mut budget, after(5_999)), None);
        assert_eq!(left(&mut budget, after(6_050)), Some(10_000));
    }

    proptest! {
        // Whatever hooks run and whenever, as long as each runs no more
        // than it was allowed: no second holds more than its budget.
        #[test]
        fn no_second_ever_holds_more_than_its_budget(
            hooks in prop::collection::vec((0..400_u64, 0..=10_000_u64), 1..200),
        ) {
            let mut budget = SecondBudget::default();
            let mut ran: Vec<(u64, u64)> = Vec::new();
            let mut at = 0_u64;
            for (pause, wanted) in hooks {
                at += pause;
                let Ok(allowance) = budget.allowance(after(at)) else { continue };
                let operations = wanted.min(allowance.operations.get());
                budget.spend(after(at), operations, Duration::ZERO);
                ran.push((at, operations));
            }

            for (index, (start, _)) in ran.iter().enumerate() {
                let within: u64 = ran
                    .iter()
                    .skip(index)
                    .take_while(|(at, _)| at - start < 1_000)
                    .map(|(_, operations)| operations)
                    .sum();
                prop_assert!(within <= limits::OPERATIONS_PER_SECOND, "{within} from {start} ms");
            }
        }

        // The same for time: whatever hooks run and whenever, as long as
        // each takes no longer than it was allowed.
        #[test]
        fn no_second_ever_holds_more_time_than_its_budget(
            hooks in prop::collection::vec((0..400_u64, 0..=5_000_u64), 1..200),
        ) {
            let mut budget = SecondBudget::default();
            let mut ran: Vec<(u64, Duration)> = Vec::new();
            let mut at = 0_u64;
            for (pause, wanted) in hooks {
                at += pause;
                let Ok(allowance) = budget.allowance(after(at)) else { continue };
                let took = Duration::from_micros(wanted).min(allowance.time);
                budget.spend(after(at), 1, took);
                ran.push((at, took));
            }

            for (index, (start, _)) in ran.iter().enumerate() {
                let within: Duration = ran
                    .iter()
                    .skip(index)
                    .take_while(|(at, _)| at - start < 1_000)
                    .map(|(_, took)| *took)
                    .sum();
                prop_assert!(within <= limits::TIME_PER_SECOND, "{within:?} from {start} ms");
            }
        }

        // And it isn't stricter than a second and two slices: a script
        // that paused that long has its whole budget.
        #[test]
        fn after_a_second_and_two_slices_of_nothing_the_budget_is_whole(
            spent in 1..=30_000_u64,
            at in 0..10_000_u64,
        ) {
            let mut budget = SecondBudget::default();
            budget.spend(after(at), spent, Duration::ZERO);

            prop_assert_eq!(left(&mut budget, after(at + 1_100)), Some(10_000));
        }
    }
}
