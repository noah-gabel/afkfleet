//! fleet-mc's diagnostics, sampled by the agent (Plan.md P5.3; ADR-0013,
//! ADR-0014).
//!
//! fleet-runtime can't read fleet-mc's numbers, so the agent samples them
//! every [`SAMPLE_PERIOD`] through [`HostDiagnostics`] and exports them as
//! metrics. The same sample tells the agent when the abandoned-thread limit
//! is reached, at which point it shuts down and exits so Docker restarts it
//! (Plan.md §6 row 8).
//!
//! - `afkfleet_mc_abandoned_threads_total`, `afkfleet_mc_dropped_chat_total`
//!   and `afkfleet_mc_ignored_action_bar_total` are counters, set from
//!   fleet-mc's own running totals.
//! - `afkfleet_mc_host_threads` and `afkfleet_mc_worlds` are gauges.
//!
//! None of them has a label. The run describes them and registers each
//! at 0, so they exist before the first sample.

use core::time::Duration;

use fleet_mc::AzaleaConnector;
use metrics::{counter, describe_counter, describe_gauge, gauge};

/// How often the agent samples the diagnostics.
pub const SAMPLE_PERIOD: Duration = Duration::from_secs(5);

/// Hung host threads that were abandoned.
const ABANDONED_THREADS: &str = "afkfleet_mc_abandoned_threads_total";
/// Incoming chat that was dropped.
const DROPPED_CHAT: &str = "afkfleet_mc_dropped_chat_total";
/// Action-bar messages that were ignored.
const IGNORED_ACTION_BAR: &str = "afkfleet_mc_ignored_action_bar_total";
/// Running host threads.
const HOST_THREADS: &str = "afkfleet_mc_host_threads";
/// Tracked Minecraft worlds.
const WORLDS: &str = "afkfleet_mc_worlds";

/// One reading of the Minecraft adapter's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostSample {
    /// Hung host threads that were abandoned; it only counts up.
    pub abandoned_threads: usize,
    /// Running host threads, abandoned ones that still run included.
    pub host_threads: usize,
    /// The Minecraft worlds the adapter tracks.
    pub worlds: usize,
    /// Incoming chat dropped because a session's event queue was full; it
    /// only counts up.
    pub dropped_chat: u64,
    /// Action-bar messages that were ignored; it only counts up.
    pub ignored_action_bar: u64,
}

/// Where the agent reads the Minecraft adapter's numbers: fleet-mc's
/// [`AzaleaConnector`] in the binary, a fake in tests.
pub trait HostDiagnostics: Send + Sync + 'static {
    /// Reads the numbers now.
    fn sample(&self) -> HostSample;
}

impl HostDiagnostics for AzaleaConnector {
    fn sample(&self) -> HostSample {
        HostSample {
            abandoned_threads: self.pool().abandoned_threads(),
            host_threads: self.pool().live_threads(),
            worlds: self.live_worlds(),
            dropped_chat: self.dropped_chat(),
            ignored_action_bar: self.ignored_action_bar(),
        }
    }
}

/// Describes the metrics and registers each at 0, in the recorder installed
/// now.
pub(crate) fn register() {
    describe_counter!(
        ABANDONED_THREADS,
        "Hung Minecraft host threads that were abandoned. At the limit, the agent exits."
    );
    describe_counter!(
        DROPPED_CHAT,
        "Incoming chat dropped because a session's event queue was full."
    );
    describe_counter!(
        IGNORED_ACTION_BAR,
        "Action-bar messages the Minecraft adapter ignored."
    );
    describe_gauge!(
        HOST_THREADS,
        "Running host threads, including abandoned threads that still run."
    );
    describe_gauge!(WORLDS, "The Minecraft worlds the adapter tracks.");
    record(&HostSample::default());
}

/// Records `sample` in the metrics. The counters take fleet-mc's own
/// running totals as they are.
pub(crate) fn record(sample: &HostSample) {
    counter!(ABANDONED_THREADS).absolute(counter_value(sample.abandoned_threads));
    counter!(DROPPED_CHAT).absolute(sample.dropped_chat);
    counter!(IGNORED_ACTION_BAR).absolute(sample.ignored_action_bar);
    gauge!(HOST_THREADS).set(gauge_value(sample.host_threads));
    gauge!(WORLDS).set(gauge_value(sample.worlds));
}

/// A count as a counter's value; a count above `u64::MAX` saturates.
fn counter_value(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

/// A count as a gauge's value; a count above `u32::MAX` saturates, which
/// keeps the conversion exact.
fn gauge_value(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use fleet_mc::McConfig;
    use metrics_exporter_prometheus::PrometheusBuilder;

    use super::*;

    /// The value of the series `name` in a Prometheus render, if it's there.
    fn value(render: &str, name: &str) -> Option<String> {
        render.lines().find_map(|line| {
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(' '))
                .map(str::to_owned)
        })
    }

    #[test]
    fn a_new_azalea_connector_samples_all_zero() {
        let connector = AzaleaConnector::new(&McConfig::default());

        assert_eq!(connector.sample(), HostSample::default());
    }

    #[test]
    fn every_metric_is_registered_at_zero() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        metrics::with_local_recorder(&recorder, register);

        let render = handle.render();
        for name in [
            ABANDONED_THREADS,
            DROPPED_CHAT,
            IGNORED_ACTION_BAR,
            HOST_THREADS,
            WORLDS,
        ] {
            assert_eq!(value(&render, name).as_deref(), Some("0"), "{render}");
        }
    }

    #[test]
    fn every_metric_is_described() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        metrics::with_local_recorder(&recorder, register);

        let render = handle.render();
        assert!(
            render.contains("# HELP afkfleet_mc_host_threads Running host threads, including abandoned threads that still run."),
            "{render}"
        );
        assert!(
            render.contains("# HELP afkfleet_mc_dropped_chat_total Incoming chat dropped because a session's event queue was full."),
            "{render}"
        );
        assert!(
            render.contains("# TYPE afkfleet_mc_abandoned_threads_total counter"),
            "{render}"
        );
        assert!(
            render.contains("# TYPE afkfleet_mc_worlds gauge"),
            "{render}"
        );
    }

    #[test]
    fn a_sample_sets_every_metric() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let sample = HostSample {
            abandoned_threads: 1,
            host_threads: 4,
            worlds: 3,
            dropped_chat: 17,
            ignored_action_bar: 9,
        };

        metrics::with_local_recorder(&recorder, || {
            register();
            record(&sample);
        });

        let render = handle.render();
        assert_eq!(value(&render, ABANDONED_THREADS).as_deref(), Some("1"));
        assert_eq!(value(&render, HOST_THREADS).as_deref(), Some("4"));
        assert_eq!(value(&render, WORLDS).as_deref(), Some("3"));
        assert_eq!(value(&render, DROPPED_CHAT).as_deref(), Some("17"));
        assert_eq!(value(&render, IGNORED_ACTION_BAR).as_deref(), Some("9"));
    }

    #[test]
    fn counts_convert_exactly_and_saturate() {
        assert_eq!(counter_value(42), 42);
        assert_eq!(
            counter_value(usize::MAX),
            u64::try_from(usize::MAX).unwrap_or(u64::MAX)
        );
        assert_eq!(gauge_value(42).to_bits(), 42.0_f64.to_bits());
        assert_eq!(
            gauge_value(usize::MAX).to_bits(),
            f64::from(u32::MAX).to_bits()
        );
    }
}
