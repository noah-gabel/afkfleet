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
//! - A managed agent's outbox (P12.4; ADR-0019, S63): the counters
//!   `afkfleet_outbox_chat_dropped_total`, `afkfleet_outbox_refused_total`
//!   and `afkfleet_outbox_session_requests_dropped_total`, set from the
//!   outbox's own running totals at each sample. A standalone agent has no
//!   outbox, and they stay at 0.
//!
//! - A managed agent's certificate (P13.6; ADR-0027): the gauge
//!   `afkfleet_identity_not_after_timestamp_seconds`, when it ends, set at
//!   the start and at each renewal, and the counter
//!   `afkfleet_identity_renewals_total` by `outcome` (`renewed` or
//!   `failed`). A standalone agent has no certificate, and they stay at 0.
//!
//! Only the renewals have a label. The run describes them all and registers
//! each at 0, so they exist before the first sample.

use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_mc::AzaleaConnector;
use metrics::{counter, describe_counter, describe_gauge, gauge};

use crate::managed::outbound::Counts;

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
/// Chat the outbox dropped for newer chat.
const OUTBOX_CHAT_DROPPED: &str = "afkfleet_outbox_chat_dropped_total";
/// Items of a never-drop class the outbox refused, full.
const OUTBOX_REFUSED: &str = "afkfleet_outbox_refused_total";
/// Session requests the outbox replaced, or dropped at a stream's end.
const OUTBOX_REQUESTS_DROPPED: &str = "afkfleet_outbox_session_requests_dropped_total";
/// When a managed agent's certificate ends.
const IDENTITY_NOT_AFTER: &str = "afkfleet_identity_not_after_timestamp_seconds";
/// A managed agent's renewals of its certificate, by outcome.
const IDENTITY_RENEWALS: &str = "afkfleet_identity_renewals_total";

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
    describe_counter!(
        OUTBOX_CHAT_DROPPED,
        "Chat a managed agent's outbox dropped for newer chat while the server didn't take it."
    );
    describe_counter!(
        OUTBOX_REFUSED,
        "Items a managed agent's outbox refused, full: each ends the stream, and the next hello resyncs."
    );
    describe_counter!(
        OUTBOX_REQUESTS_DROPPED,
        "Session requests a managed agent's outbox replaced, or dropped unsent at a stream's end."
    );
    describe_gauge!(
        IDENTITY_NOT_AFTER,
        "When a managed agent's certificate ends, in Unix seconds; 0 for a standalone agent. It renews with a third of its lifetime left: one near its end didn't."
    );
    describe_counter!(
        IDENTITY_RENEWALS,
        "A managed agent's attempts to renew its certificate, by outcome."
    );
    record(&HostSample::default());
    record_outbox(&Counts::default());
    gauge!(IDENTITY_NOT_AFTER).set(0.0);
    for outcome in [RENEWED, FAILED] {
        counter!(IDENTITY_RENEWALS, "outcome" => outcome).increment(0);
    }
}

/// The `outcome` of a renewal that stored a new certificate.
const RENEWED: &str = "renewed";
/// The `outcome` of an attempt that stored nothing.
const FAILED: &str = "failed";

/// A managed agent's certificate is valid until `not_after`: at the start,
/// and at each renewal (P13.6).
pub(crate) fn identity_valid_until(not_after: DateTime<Utc>) {
    gauge!(IDENTITY_NOT_AFTER).set(timestamp_value(not_after));
}

/// A managed agent renewed its certificate, now valid until `not_after`.
pub(crate) fn identity_renewed(not_after: DateTime<Utc>) {
    counter!(IDENTITY_RENEWALS, "outcome" => RENEWED).increment(1);
    identity_valid_until(not_after);
}

/// A managed agent's attempt to renew its certificate failed.
pub(crate) fn identity_renewal_failed() {
    counter!(IDENTITY_RENEWALS, "outcome" => FAILED).increment(1);
}

/// Records a managed agent's outbox `counts`, its own running totals.
pub(crate) fn record_outbox(counts: &Counts) {
    counter!(OUTBOX_CHAT_DROPPED).absolute(counts.chat_dropped);
    counter!(OUTBOX_REFUSED).absolute(counts.refused);
    counter!(OUTBOX_REQUESTS_DROPPED).absolute(counts.requests_dropped);
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

/// `at` in Unix seconds as a gauge's value, exact; a time before 1970 is 0.
fn timestamp_value(at: DateTime<Utc>) -> f64 {
    let secs = u64::try_from(at.timestamp()).unwrap_or(0);
    let high = u32::try_from(secs >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(secs & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
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
            OUTBOX_CHAT_DROPPED,
            OUTBOX_REFUSED,
            OUTBOX_REQUESTS_DROPPED,
            IDENTITY_NOT_AFTER,
            "afkfleet_identity_renewals_total{outcome=\"renewed\"}",
            "afkfleet_identity_renewals_total{outcome=\"failed\"}",
        ] {
            assert_eq!(value(&render, name).as_deref(), Some("0"), "{render}");
        }
    }

    #[test]
    fn a_renewal_counts_and_moves_the_certificates_end() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let at = |secs| DateTime::from_timestamp(secs, 0).unwrap();

        metrics::with_local_recorder(&recorder, || {
            register();
            identity_valid_until(at(1_800_000_000));
            identity_renewal_failed();
            identity_renewal_failed();
            identity_renewed(at(1_805_184_000));
        });

        let render = handle.render();
        assert_eq!(
            value(&render, IDENTITY_NOT_AFTER).as_deref(),
            Some("1805184000")
        );
        assert_eq!(
            value(
                &render,
                "afkfleet_identity_renewals_total{outcome=\"renewed\"}"
            )
            .as_deref(),
            Some("1")
        );
        assert_eq!(
            value(
                &render,
                "afkfleet_identity_renewals_total{outcome=\"failed\"}"
            )
            .as_deref(),
            Some("2")
        );
        assert!(
            render.contains("# TYPE afkfleet_identity_not_after_timestamp_seconds gauge"),
            "{render}"
        );
        assert_eq!(
            timestamp_value(at(8_000_000_000)).to_bits(),
            8_000_000_000.0_f64.to_bits(),
            "past 2106 too"
        );
        assert_eq!(timestamp_value(at(-5)).to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn the_outbox_counts_set_their_counters() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let counts = Counts {
            chat_dropped: 12,
            refused: 1,
            requests_dropped: 3,
        };

        metrics::with_local_recorder(&recorder, || {
            register();
            record_outbox(&counts);
        });

        let render = handle.render();
        assert_eq!(value(&render, OUTBOX_CHAT_DROPPED).as_deref(), Some("12"));
        assert_eq!(value(&render, OUTBOX_REFUSED).as_deref(), Some("1"));
        assert_eq!(
            value(&render, OUTBOX_REQUESTS_DROPPED).as_deref(),
            Some("3")
        );
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
        assert!(
            render.contains("# HELP afkfleet_outbox_refused_total Items a managed agent's outbox refused, full: each ends the stream, and the next hello resyncs."),
            "{render}"
        );
        assert!(
            render.contains("# TYPE afkfleet_outbox_chat_dropped_total counter"),
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
