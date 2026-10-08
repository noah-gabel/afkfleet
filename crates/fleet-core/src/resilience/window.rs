//! [`FailureWindow`]: counts failures within a sliding time window.

use core::num::NonZeroUsize;
use core::time::Duration;
use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use crate::time;

/// Detects "`threshold` failures within `window`".
///
/// It keeps at most `threshold` timestamps, so its memory is bounded. A
/// failure counts while it's at most `window` old.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureWindow {
    threshold: NonZeroUsize,
    window: Duration,
    failures: VecDeque<DateTime<Utc>>,
}

impl FailureWindow {
    /// Creates an empty window.
    #[must_use]
    pub fn new(threshold: NonZeroUsize, window: Duration) -> Self {
        Self {
            threshold,
            window,
            failures: VecDeque::new(),
        }
    }

    /// Records a failure at `at` and returns whether the window now holds
    /// `threshold` failures.
    pub fn record(&mut self, at: DateTime<Utc>) -> bool {
        self.failures
            .retain(|&failure| time::elapsed(failure, at) <= self.window);
        self.failures.push_back(at);
        while self.failures.len() > self.threshold.get() {
            self.failures.pop_front();
        }
        self.failures.len() >= self.threshold.get()
    }

    /// Forgets every recorded failure.
    pub fn clear(&mut self) {
        self.failures.clear();
    }

    /// Returns how many failures the window held after the last
    /// [`record`](Self::record): at most `threshold`. Failures that have aged
    /// out since are only dropped by the next `record`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.failures.len()
    }

    /// Whether no failure is recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.failures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn three_per_minute() -> FailureWindow {
        FailureWindow::new(NonZeroUsize::new(3).unwrap(), Duration::from_secs(60))
    }

    #[test]
    fn trips_at_the_threshold_within_the_window() {
        let mut window = three_per_minute();

        assert!(!window.record(at(0)));
        assert!(!window.record(at(10)));
        assert!(window.record(at(20)));
    }

    #[test]
    fn counts_a_failure_exactly_one_window_old() {
        let mut window = three_per_minute();

        window.record(at(0));
        window.record(at(30));
        assert!(window.record(at(60)));
    }

    #[test]
    fn spread_out_failures_dont_trip() {
        let mut window = three_per_minute();

        window.record(at(0));
        window.record(at(40));
        assert!(!window.record(at(80)));
        assert!(window.record(at(90)));
    }

    #[test]
    fn keeps_at_most_threshold_timestamps() {
        let mut window = three_per_minute();
        for second in 0..100 {
            window.record(at(second));
        }
        assert_eq!(window.failures.len(), 3);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut window = three_per_minute();
        window.record(at(0));
        window.record(at(1));

        window.clear();
        assert!(!window.record(at(2)));
        assert!(!window.record(at(3)));
    }

    #[test]
    fn len_counts_the_failures_within_the_window() {
        let mut window = three_per_minute();
        assert!(window.is_empty());

        window.record(at(0));
        window.record(at(10));
        assert_eq!(window.len(), 2);
        assert!(!window.is_empty());

        window.record(at(80));
        assert_eq!(window.len(), 1, "the first two aged out");
    }

    #[test]
    fn len_never_exceeds_the_threshold() {
        let mut window = three_per_minute();
        for second in 0..10 {
            window.record(at(second));
        }

        assert_eq!(window.len(), 3);
    }

    #[test]
    fn a_threshold_of_one_trips_at_once() {
        let mut window = FailureWindow::new(NonZeroUsize::MIN, Duration::from_secs(60));
        assert!(window.record(at(0)));
    }

    #[test]
    fn survives_the_clock_going_backwards() {
        let mut window = three_per_minute();

        window.record(at(100));
        window.record(at(50));
        assert!(window.record(at(0)));
    }
}
