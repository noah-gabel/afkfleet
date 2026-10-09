//! The runtime's metrics (Plan.md P4.9; ADR-0013), recorded through the
//! `metrics` facade into whatever recorder is installed. The agent installs
//! the Prometheus exporter before it builds the fleet (P5.3).
//!
//! - `afkfleet_bots{state}`, a gauge: how many bots are in each state. A bot
//!   counts from the moment the supervisor knows it until it's removed or
//!   the supervisor ends; [`SnapshotOwner`](crate::SnapshotOwner) keeps that
//!   true for every write of a bot's snapshot.
//! - `afkfleet_bot_reconnects_total`: every connect a bot makes on its own,
//!   that is every connect that isn't the first of a deliberate run (Start,
//!   Reset, Resume, Restart or a server change). The fresh-token retry after
//!   an auth rejection counts too.
//! - `afkfleet_watchdog_trips_total{kind}`: sessions the watchdog ended,
//!   `kind="tick"` for a hung session and `kind="packet"` for a dead link.
//! - `afkfleet_actor_restarts_total`: every actor the supervisor starts after
//!   a crash, the crash-loop start included.
//!
//! None of them has a `bot_id` label. [`register`] describes them and
//! registers every series at 0, so they all exist before anything happens.

use fleet_core::bot::BotState;
use metrics::{counter, describe_counter, describe_gauge, gauge};

use crate::actor::watchdog::Stall;

/// The bots in each state.
pub(crate) const BOTS: &str = "afkfleet_bots";
/// Connects a bot made on its own.
pub(crate) const RECONNECTS: &str = "afkfleet_bot_reconnects_total";
/// Sessions the watchdog ended.
pub(crate) const WATCHDOG_TRIPS: &str = "afkfleet_watchdog_trips_total";
/// Actors started again after a crash.
pub(crate) const ACTOR_RESTARTS: &str = "afkfleet_actor_restarts_total";

/// Every `state` label of [`BOTS`], one per [`BotState`] variant.
const STATE_LABELS: [&str; 8] = [
    "stopped",
    "awaiting_session",
    "connecting",
    "online",
    "backoff",
    "paused",
    "failed",
    "stopping",
];

/// Describes the metrics and registers every series at 0, in the recorder
/// installed now.
pub(crate) fn register() {
    describe_gauge!(BOTS, "The fleet's bots in each state.");
    describe_counter!(
        RECONNECTS,
        "Connects the bots made on their own: every connect that isn't the first of a deliberate run."
    );
    describe_counter!(
        WATCHDOG_TRIPS,
        "Sessions the watchdog ended: kind=\"tick\" for a hung session, kind=\"packet\" for a dead link."
    );
    describe_counter!(
        ACTOR_RESTARTS,
        "Bot actors the supervisor started again after a crash, the crash-loop start included."
    );
    for state in STATE_LABELS {
        gauge!(BOTS, "state" => state).increment(0.0);
    }
    counter!(RECONNECTS).increment(0);
    for stall in [Stall::Tick, Stall::Packet] {
        counter!(WATCHDOG_TRIPS, "kind" => trip_label(stall)).increment(0);
    }
    counter!(ACTOR_RESTARTS).increment(0);
}

/// A new bot counts in `state`.
pub(crate) fn bot_added(state: BotState) {
    gauge!(BOTS, "state" => state_label(state)).increment(1.0);
}

/// A bot that was in `state` no longer counts.
pub(crate) fn bot_gone(state: BotState) {
    gauge!(BOTS, "state" => state_label(state)).decrement(1.0);
}

/// A bot moved from `from` to `to`; nothing changes within one label.
pub(crate) fn bot_moved(from: BotState, to: BotState) {
    let (from, to) = (state_label(from), state_label(to));
    if from != to {
        gauge!(BOTS, "state" => from).decrement(1.0);
        gauge!(BOTS, "state" => to).increment(1.0);
    }
}

/// A bot connects on its own.
pub(crate) fn reconnect() {
    counter!(RECONNECTS).increment(1);
}

/// The watchdog ended a session that stalled as `stall` says.
pub(crate) fn watchdog_trip(stall: Stall) {
    counter!(WATCHDOG_TRIPS, "kind" => trip_label(stall)).increment(1);
}

/// The supervisor starts an actor after a crash.
pub(crate) fn actor_restart() {
    counter!(ACTOR_RESTARTS).increment(1);
}

/// The `state` label of `state`. The match has no catch-all arm, so a new
/// variant doesn't compile until it has a label.
const fn state_label(state: BotState) -> &'static str {
    match state {
        BotState::Stopped => "stopped",
        BotState::AwaitingSession { .. } => "awaiting_session",
        BotState::Connecting { .. } => "connecting",
        BotState::Online { .. } => "online",
        BotState::Backoff { .. } => "backoff",
        BotState::Paused { .. } => "paused",
        BotState::Failed { .. } => "failed",
        BotState::Stopping { .. } => "stopping",
    }
}

/// The `kind` label of a watchdog trip.
const fn trip_label(stall: Stall) -> &'static str {
    match stall {
        Stall::Tick => "tick",
        Stall::Packet => "packet",
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! A recorder for the unit tests. The component tests in `tests/fleet/`
    //! have their own, since they can't see `cfg(test)` items.

    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use metrics::{
        Counter, Gauge, Histogram, Key, KeyName, LocalRecorderGuard, Metadata, SharedString, Unit,
    };

    /// Records counters and gauges by name and labels.
    #[derive(Debug, Default)]
    pub(crate) struct Recorder {
        cells: Mutex<BTreeMap<String, Arc<AtomicU64>>>,
    }

    impl Recorder {
        /// Makes this the current thread's recorder until the guard drops.
        pub(crate) fn install(&self) -> LocalRecorderGuard<'_> {
            metrics::set_default_local_recorder(self)
        }

        /// A counter's value, if it was registered.
        pub(crate) fn counter(&self, name: &str, labels: &[(&str, &str)]) -> Option<u64> {
            self.cell(&series(name, labels.iter().copied()))
                .map(|cell| cell.load(Ordering::SeqCst))
        }

        /// A gauge's value, if it was registered.
        pub(crate) fn gauge(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
            self.cell(&series(name, labels.iter().copied()))
                .map(|cell| f64::from_bits(cell.load(Ordering::SeqCst)))
        }

        fn cell(&self, series: &str) -> Option<Arc<AtomicU64>> {
            self.cells.lock().unwrap().get(series).cloned()
        }

        fn register(&self, key: &Key) -> Arc<AtomicU64> {
            let series = series(
                key.name(),
                key.labels().map(|label| (label.key(), label.value())),
            );
            Arc::clone(self.cells.lock().unwrap().entry(series).or_default())
        }
    }

    /// A series' name with its labels, sorted.
    fn series<'a>(name: &str, labels: impl Iterator<Item = (&'a str, &'a str)>) -> String {
        let mut labels: Vec<String> = labels
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        labels.sort();
        format!("{name}{{{}}}", labels.join(","))
    }

    impl metrics::Recorder for Recorder {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            Counter::from_arc(self.register(key))
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::from_arc(self.register(key))
        }

        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::collections::BTreeSet;

    use chrono::DateTime;
    use fleet_core::bot::{FailReason, PauseReason};
    use fleet_core::disconnect::ConflictKind;

    use super::*;

    /// One state of each variant.
    fn every_state() -> [BotState; 8] {
        let attempt = NonZeroU32::MIN;
        [
            BotState::Stopped,
            BotState::AwaitingSession {
                attempt,
                fresh: false,
            },
            BotState::Connecting {
                attempt,
                auth_retried: false,
            },
            BotState::Online {
                since: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                attempt,
            },
            BotState::Backoff { attempt },
            BotState::Paused {
                reason: PauseReason::Conflict {
                    kind: ConflictKind::DuplicateLogin,
                },
            },
            BotState::Failed {
                reason: FailReason::Auth,
            },
            BotState::Stopping { restart: false },
        ]
    }

    #[test]
    fn every_state_has_its_own_snake_case_label_and_all_are_registered() {
        let labels: Vec<&str> = every_state().into_iter().map(state_label).collect();

        let unique: BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(unique.len(), labels.len(), "{labels:?}");
        for label in &labels {
            assert!(
                !label.is_empty()
                    && label.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    && !label.starts_with('_')
                    && !label.ends_with('_'),
                "{label:?}"
            );
        }
        assert_eq!(unique, STATE_LABELS.into_iter().collect());
    }

    #[test]
    fn the_trip_kinds_are_tick_and_packet() {
        assert_eq!(trip_label(Stall::Tick), "tick");
        assert_eq!(trip_label(Stall::Packet), "packet");
    }
}
