//! [`Timers`]: the named timers a script set (ADR-0036, D3, D6).

use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::script::{TimerName, limits};
use fleet_core::time;

/// Why a timer wasn't set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimerError {
    /// Its length isn't [`limits::MIN_TIMER`] to [`limits::MAX_TIMER`].
    Length,
    /// The script holds [`limits::MAX_TIMERS`] others already.
    TooMany,
}

/// One timer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Timer {
    name: TimerName,
    due: DateTime<Utc>,
    /// How long after each run it's due again; `None` for one that runs
    /// once.
    every: Option<Duration>,
    /// When it was set, among the others: earlier ones run first when
    /// several are due at the same moment.
    order: u64,
}

/// A script's timers: at most [`limits::MAX_TIMERS`], each with a name of
/// its own, each [`limits::MIN_TIMER`] to [`limits::MAX_TIMER`] long.
///
/// A name holds one timer; setting it again replaces it. It holds no
/// clock: setting one and asking what's due take `now` (ADR-0010).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Timers {
    timers: Vec<Timer>,
    /// How many were ever set, for their order.
    set: u64,
}

impl Timers {
    /// Sets the timer `name` to run `length` from `now`, and with
    /// `repeats`, every `length` after that. It replaces a timer of that
    /// name.
    ///
    /// # Errors
    /// [`TimerError::Length`], then [`TimerError::TooMany`]; nothing
    /// changes then.
    pub(crate) fn set(
        &mut self,
        name: TimerName,
        length: Duration,
        repeats: bool,
        now: DateTime<Utc>,
    ) -> Result<(), TimerError> {
        if !(limits::MIN_TIMER..=limits::MAX_TIMER).contains(&length) {
            return Err(TimerError::Length);
        }
        let known = self.timers.iter().position(|timer| timer.name == name);
        if known.is_none() && self.timers.len() >= limits::MAX_TIMERS {
            return Err(TimerError::TooMany);
        }
        self.set = self.set.saturating_add(1);
        let timer = Timer {
            name,
            due: time::add(now, length),
            every: repeats.then_some(length),
            order: self.set,
        };
        match known.and_then(|index| self.timers.get_mut(index)) {
            Some(slot) => *slot = timer,
            None => self.timers.push(timer),
        }
        Ok(())
    }

    /// Removes the timer `name`, if there is one.
    pub(crate) fn cancel(&mut self, name: &str) {
        self.timers.retain(|timer| timer.name.as_str() != name);
    }

    /// Removes every timer: at a disconnect, and when the script stops.
    pub(crate) fn clear(&mut self) {
        self.timers.clear();
    }

    /// How many timers are set.
    pub(crate) fn len(&self) -> usize {
        self.timers.len()
    }

    /// When the next timer is due.
    pub(crate) fn next_due(&self) -> Option<DateTime<Utc>> {
        self.timers.iter().map(|timer| timer.due).min()
    }

    /// Takes the timer that's due at `now` and was due first; of two due
    /// at the same moment, the one set first. One that runs once is
    /// removed; a repeating one is due again its interval from `now`, so
    /// it never catches up on runs it missed.
    ///
    /// Called until it returns `None`, it ends: nothing it leaves behind
    /// is due at `now` again.
    pub(crate) fn take_due(&mut self, now: DateTime<Utc>) -> Option<TimerName> {
        let index = self
            .timers
            .iter()
            .enumerate()
            .filter(|(_, timer)| timer.due <= now)
            .min_by_key(|(_, timer)| (timer.due, timer.order))
            .map(|(index, _)| index)?;
        let timer = self.timers.get_mut(index)?;
        let name = timer.name.clone();
        match timer.every {
            Some(every) => timer.due = time::add(now, every),
            None => {
                self.timers.remove(index);
            }
        }
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use rstest::rstest;

    use super::*;

    fn start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
    }

    fn after(ms: u64) -> DateTime<Utc> {
        time::add(start(), Duration::from_millis(ms))
    }

    fn name(text: &str) -> TimerName {
        TimerName::try_from(text).unwrap()
    }

    /// Every timer due at `now`, in the order they're taken.
    fn due(timers: &mut Timers, now: DateTime<Utc>) -> Vec<String> {
        core::iter::from_fn(|| timers.take_due(now))
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn a_timer_is_due_once_its_length_has_passed_and_then_gone() {
        let mut timers = Timers::default();
        timers
            .set(name("once"), Duration::from_secs(1), false, start())
            .unwrap();

        assert_eq!(timers.next_due(), Some(after(1_000)));
        assert_eq!(due(&mut timers, after(999)).len(), 0);
        assert_eq!(due(&mut timers, after(1_000)), ["once"]);
        assert_eq!(timers.len(), 0);
        assert_eq!(timers.next_due(), None);
    }

    #[test]
    fn a_repeating_timer_is_due_again_its_interval_after_it_ran() {
        let mut timers = Timers::default();
        timers
            .set(name("again"), Duration::from_millis(600), true, start())
            .unwrap();

        assert_eq!(due(&mut timers, after(600)), ["again"]);
        assert_eq!(timers.next_due(), Some(after(1_200)));
        // Run late, it's due an interval from then, not from when it
        // should have run: it never catches up.
        assert_eq!(due(&mut timers, after(10_000)), ["again"]);
        assert_eq!(timers.next_due(), Some(after(10_600)));
        assert_eq!(timers.len(), 1);
    }

    #[rstest]
    #[case(Duration::from_millis(249), false)]
    #[case(Duration::from_millis(250), true)]
    #[case(Duration::from_hours(10), true)]
    #[case(Duration::from_hours(24), true)]
    #[case(Duration::from_hours(24) + Duration::from_millis(1), false)]
    #[case(Duration::ZERO, false)]
    fn a_timer_is_250_ms_to_24_hours_long(#[case] length: Duration, #[case] allowed: bool) {
        for repeats in [false, true] {
            let mut timers = Timers::default();

            let set = timers.set(name("t"), length, repeats, start());

            assert_eq!(set.is_ok(), allowed);
            assert_eq!(timers.len(), usize::from(allowed));
            if !allowed {
                assert_eq!(set, Err(TimerError::Length));
            }
        }
    }

    #[test]
    fn a_script_holds_sixteen_timers_and_replacing_one_is_no_seventeenth() {
        let mut timers = Timers::default();
        for index in 0..16 {
            timers
                .set(
                    name(&format!("t{index}")),
                    Duration::from_secs(1),
                    false,
                    start(),
                )
                .unwrap();
        }

        assert_eq!(
            timers.set(name("one more"), Duration::from_secs(1), false, start()),
            Err(TimerError::TooMany)
        );
        assert_eq!(
            timers.set(name("t3"), Duration::from_secs(5), true, start()),
            Ok(())
        );
        assert_eq!(timers.len(), 16);
        // A length that isn't one is said first.
        assert_eq!(
            timers.set(name("one more"), Duration::ZERO, false, start()),
            Err(TimerError::Length)
        );
    }

    #[test]
    fn setting_a_name_again_replaces_its_timer() {
        let mut timers = Timers::default();
        timers
            .set(name("t"), Duration::from_secs(1), true, start())
            .unwrap();
        timers
            .set(name("t"), Duration::from_secs(5), false, after(100))
            .unwrap();

        assert_eq!(timers.len(), 1);
        assert_eq!(due(&mut timers, after(5_000)).len(), 0);
        assert_eq!(due(&mut timers, after(5_100)), ["t"]);
        assert_eq!(timers.len(), 0);
    }

    #[test]
    fn cancelling_removes_one_and_clearing_all() {
        let mut timers = Timers::default();
        timers
            .set(name("a"), Duration::from_secs(1), true, start())
            .unwrap();
        timers
            .set(name("b"), Duration::from_secs(1), true, start())
            .unwrap();

        timers.cancel("a");
        timers.cancel("never set");
        assert_eq!(due(&mut timers, after(1_000)), ["b"]);

        timers.clear();
        assert_eq!(timers.len(), 0);
        assert_eq!(timers.next_due(), None);
    }

    #[test]
    fn timers_due_together_are_taken_in_the_order_they_were_due_then_set() {
        let mut timers = Timers::default();
        timers
            .set(name("c"), Duration::from_secs(3), false, start())
            .unwrap();
        timers
            .set(name("a"), Duration::from_secs(1), false, start())
            .unwrap();
        timers
            .set(name("b2"), Duration::from_secs(2), false, start())
            .unwrap();
        timers
            .set(name("b1"), Duration::from_secs(2), true, start())
            .unwrap();
        // Set again: it's the newest of the two due at two seconds.
        timers
            .set(name("b2"), Duration::from_secs(2), false, start())
            .unwrap();

        assert_eq!(due(&mut timers, after(5_000)), ["a", "b1", "b2", "c"]);
        assert_eq!(timers.len(), 1);
    }

    #[test]
    fn taking_what_is_due_always_ends() {
        let mut timers = Timers::default();
        for index in 0..16 {
            timers
                .set(
                    name(&format!("t{index}")),
                    Duration::from_millis(250),
                    true,
                    start(),
                )
                .unwrap();
        }

        // A day later, each is taken once, not once for every interval
        // that passed.
        assert_eq!(due(&mut timers, after(86_400_000)).len(), 16);
    }

    #[test]
    fn a_timer_set_at_the_end_of_time_is_due_then_and_not_before() {
        let mut timers = Timers::default();
        timers
            .set(
                name("t"),
                Duration::from_hours(24),
                false,
                DateTime::<Utc>::MAX_UTC,
            )
            .unwrap();

        assert_eq!(timers.next_due(), Some(DateTime::<Utc>::MAX_UTC));
    }
}
