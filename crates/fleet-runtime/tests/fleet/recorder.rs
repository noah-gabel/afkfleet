//! [`Recorder`]: a hand-written `metrics` recorder (ADR-0013). A test
//! installs one on its own thread with `set_default_local_recorder`, and the
//! fleet's tasks run on the test's current-thread runtime, so everything the
//! fleet records lands in it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use metrics::{
    Counter, Gauge, Histogram, Key, KeyName, LocalRecorderGuard, Metadata, SharedString, Unit,
};

/// The bots gauge's name.
pub(crate) const BOTS: &str = "afkfleet_bots";
/// The reconnect counter's name.
pub(crate) const RECONNECTS: &str = "afkfleet_bot_reconnects_total";
/// The watchdog trip counter's name.
pub(crate) const WATCHDOG_TRIPS: &str = "afkfleet_watchdog_trips_total";
/// The actor restart counter's name.
pub(crate) const ACTOR_RESTARTS: &str = "afkfleet_actor_restarts_total";

/// Records counters and gauges by name and labels, and which metrics were
/// described. A counter or a gauge is an `AtomicU64`; a gauge holds its
/// `f64`'s bits.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    cells: Mutex<BTreeMap<String, Arc<AtomicU64>>>,
    described: Mutex<BTreeSet<String>>,
}

impl Recorder {
    /// Makes this the current thread's recorder until the guard drops.
    pub(crate) fn install(&self) -> LocalRecorderGuard<'_> {
        metrics::set_default_local_recorder(self)
    }

    /// Every registered series, as `name{key=value,…}`.
    pub(crate) fn series(&self) -> BTreeSet<String> {
        self.cells.lock().unwrap().keys().cloned().collect()
    }

    /// Whether the metric `name` was described.
    pub(crate) fn is_described(&self, name: &str) -> bool {
        self.described.lock().unwrap().contains(name)
    }

    /// A counter's value; 0 if it was never registered.
    pub(crate) fn counter(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
        self.cell(&series(name, labels.iter().copied()))
            .map_or(0, |cell| cell.load(Ordering::SeqCst))
    }

    /// `afkfleet_bot_reconnects_total`.
    pub(crate) fn reconnects(&self) -> u64 {
        self.counter(RECONNECTS, &[])
    }

    /// `afkfleet_watchdog_trips_total{kind}`.
    pub(crate) fn trips(&self, kind: &str) -> u64 {
        self.counter(WATCHDOG_TRIPS, &[("kind", kind)])
    }

    /// `afkfleet_actor_restarts_total`.
    pub(crate) fn restarts(&self) -> u64 {
        self.counter(ACTOR_RESTARTS, &[])
    }

    /// The bots gauge's states that aren't 0, with their values.
    pub(crate) fn bots(&self) -> BTreeMap<String, f64> {
        let prefix = format!("{BOTS}{{state=");
        self.cells
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(series, cell)| {
                let state = series.strip_prefix(&prefix)?.strip_suffix('}')?;
                let value = f64::from_bits(cell.load(Ordering::SeqCst));
                (value != 0.0).then(|| (state.to_owned(), value))
            })
            .collect()
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

    fn describe(&self, name: &KeyName) {
        self.described
            .lock()
            .unwrap()
            .insert(name.as_str().to_owned());
    }
}

/// The bots gauge, from `(state, value)` pairs.
pub(crate) fn bots<const N: usize>(states: [(&str, f64); N]) -> BTreeMap<String, f64> {
    states
        .into_iter()
        .map(|(state, value)| (state.to_owned(), value))
        .collect()
}

/// A series' name with its labels, sorted.
pub(crate) fn series<'a>(name: &str, labels: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut labels: Vec<String> = labels
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    labels.sort();
    format!("{name}{{{}}}", labels.join(","))
}

impl metrics::Recorder for Recorder {
    fn describe_counter(&self, name: KeyName, _: Option<Unit>, _: SharedString) {
        self.describe(&name);
    }

    fn describe_gauge(&self, name: KeyName, _: Option<Unit>, _: SharedString) {
        self.describe(&name);
    }

    fn describe_histogram(&self, name: KeyName, _: Option<Unit>, _: SharedString) {
        self.describe(&name);
    }

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
