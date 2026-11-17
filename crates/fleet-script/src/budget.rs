//! [`SecondBudget`]: how many operations a script may still run right now
//! (ADR-0036, D3).

use core::num::NonZeroU64;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::script::limits;
use fleet_core::time;

/// How long a slice of time is that the operations are counted in.
const SLICE: Duration = Duration::from_millis(50);

/// How many slices back still count: a second's worth and one more, so
/// that everything within the last second is always among them, wherever
/// in its slice a moment falls.
const SLICES: usize = 21;

/// The operations a script ran lately, and what's left of its budgets:
/// [`OPERATIONS_PER_HOOK`] in one hook, [`OPERATIONS_PER_SECOND`] in any
/// one second over all its hooks.
///
/// A hook counts at the moment it starts. The count is kept in slices of
/// 50 ms; a second is judged over 21 of them, which errs towards refusing:
/// no second, whenever it starts, ever holds more than its budget.
///
/// It holds no clock; a moment earlier than the last one counts as the
/// last one, so a clock that goes back frees nothing.
///
/// [`OPERATIONS_PER_HOOK`]: limits::OPERATIONS_PER_HOOK
/// [`OPERATIONS_PER_SECOND`]: limits::OPERATIONS_PER_SECOND
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SecondBudget {
    /// The slices that hold operations, oldest first: each slice's start
    /// and its operations. At most [`SLICES`] of them.
    slices: Vec<(DateTime<Utc>, u64)>,
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
            .retain(|(start, _)| time::elapsed(*start, moment) < reach);
    }

    /// How many operations a hook that starts at `now` may run: a hook's
    /// budget, or what's left of the second's if that's less. `None` when
    /// nothing is left: the hook must not run at all.
    pub(crate) fn allowance(&mut self, now: DateTime<Utc>) -> Option<Allowance> {
        let moment = self.moment(now);
        self.forget(moment);
        let used = self.slices.iter().fold(0_u64, |sum, (_, operations)| {
            sum.saturating_add(*operations)
        });
        let left = limits::OPERATIONS_PER_SECOND.saturating_sub(used);
        let operations = NonZeroU64::new(left.min(limits::OPERATIONS_PER_HOOK))?;
        Some(Allowance {
            operations,
            by_second: left < limits::OPERATIONS_PER_HOOK,
        })
    }

    /// Counts the `operations` a hook that started at `now` ran.
    pub(crate) fn spend(&mut self, now: DateTime<Utc>, operations: u64) {
        if operations == 0 {
            return;
        }
        let moment = self.moment(now);
        self.forget(moment);
        match self.slices.last_mut() {
            Some((start, used)) if time::elapsed(*start, moment) < SLICE => {
                *used = used.saturating_add(operations);
            }
            _ => self.slices.push((moment, operations)),
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
            .map(|allowance| allowance.operations.get())
    }

    #[test]
    fn a_fresh_budget_allows_a_whole_hook() {
        let mut budget = SecondBudget::default();

        assert_eq!(
            budget.allowance(start()),
            Some(Allowance {
                operations: NonZeroU64::new(10_000).unwrap(),
                by_second: false
            })
        );
    }

    #[test]
    fn what_hooks_ran_within_the_second_is_taken_off() {
        let mut budget = SecondBudget::default();

        budget.spend(after(0), 10_000);
        budget.spend(after(100), 10_000);
        // 10,000 are left of the second: exactly a hook's own budget.
        assert_eq!(
            budget.allowance(after(200)),
            Some(Allowance {
                operations: NonZeroU64::new(10_000).unwrap(),
                by_second: false
            })
        );
        budget.spend(after(200), 4_000);

        assert_eq!(
            budget.allowance(after(300)),
            Some(Allowance {
                operations: NonZeroU64::new(6_000).unwrap(),
                by_second: true
            })
        );
    }

    // The user's reviewer's finding 2: with nothing left the answer is
    // "don't run", never a budget of 0, which Rhai takes for "no limit".
    #[test]
    fn with_nothing_left_the_hook_is_refused_and_zero_is_never_handed_out() {
        let mut budget = SecondBudget::default();
        budget.spend(after(0), 10_000);
        budget.spend(after(0), 10_000);
        budget.spend(after(0), 9_999);
        assert_eq!(left(&mut budget, after(0)), Some(1));

        budget.spend(after(0), 1);

        assert_eq!(budget.allowance(after(0)), None);
        assert_eq!(budget.allowance(after(500)), None);
        // More than the budget was never spent, but if it were, nothing
        // would be left either.
        budget.spend(after(500), 50_000);
        assert_eq!(budget.allowance(after(600)), None);
    }

    #[test]
    fn the_budget_is_whole_again_once_a_second_and_a_slice_have_passed() {
        let mut budget = SecondBudget::default();
        budget.spend(after(0), 30_000);

        assert_eq!(left(&mut budget, after(999)), None);
        assert_eq!(left(&mut budget, after(1_049)), None);
        assert_eq!(left(&mut budget, after(1_050)), Some(10_000));
    }

    #[test]
    fn operations_come_back_slice_by_slice() {
        let mut budget = SecondBudget::default();
        budget.spend(after(0), 10_000);
        budget.spend(after(400), 10_000);
        budget.spend(after(800), 10_000);

        assert_eq!(left(&mut budget, after(1_000)), None);
        assert_eq!(left(&mut budget, after(1_050)), Some(10_000));
        budget.spend(after(1_050), 10_000);
        assert_eq!(left(&mut budget, after(1_400)), None);
        assert_eq!(left(&mut budget, after(1_450)), Some(10_000));
    }

    #[test]
    fn a_hook_that_ran_nothing_costs_nothing_and_keeps_nothing() {
        let mut budget = SecondBudget::default();

        for ms in 0..5_000 {
            budget.spend(after(ms), 0);
        }

        assert_eq!(budget.slices.len(), 0);
        assert_eq!(left(&mut budget, after(5_000)), Some(10_000));
    }

    #[test]
    fn it_never_keeps_more_slices_than_reach_a_second() {
        let mut budget = SecondBudget::default();

        for ms in 0..20_000 {
            budget.spend(after(ms), 1);
        }

        assert!(budget.slices.len() <= SLICES, "{}", budget.slices.len());
    }

    #[test]
    fn a_clock_that_goes_back_frees_nothing() {
        let mut budget = SecondBudget::default();
        budget.spend(after(5_000), 30_000);

        // An hour "earlier" counts as the latest moment seen.
        assert_eq!(budget.allowance(start() - Duration::from_secs(3_600)), None);
        budget.spend(after(0), 100);
        assert_eq!(budget.allowance(after(5_999)), None);
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
                let Some(allowance) = budget.allowance(after(at)) else { continue };
                let operations = wanted.min(allowance.operations.get());
                budget.spend(after(at), operations);
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

        // And it isn't stricter than a second and two slices: a script
        // that paused that long has its whole budget.
        #[test]
        fn after_a_second_and_two_slices_of_nothing_the_budget_is_whole(
            spent in 1..=30_000_u64,
            at in 0..10_000_u64,
        ) {
            let mut budget = SecondBudget::default();
            budget.spend(after(at), spent);

            prop_assert_eq!(left(&mut budget, after(at + 1_100)), Some(10_000));
        }
    }
}
