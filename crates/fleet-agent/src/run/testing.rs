//! What the run's unit tests share: a real fleet on fleet-testkit's fake
//! connector, and paused-time helpers.

use core::num::NonZeroUsize;
use core::time::Duration;
use std::sync::Arc;

use chrono::DateTime;
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_runtime::{Fleet, FleetParts, OfflineCredentials, RuntimeConfig, Supervisor};
use fleet_testkit::mc::FakeConnector;

pub(crate) const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// The supervisor of a test fleet.
pub(crate) type TestSupervisor = Supervisor<FakeConnector, OfflineCredentials>;

/// A fleet with the runtime's defaults and a supervisor queue of `queue`
/// calls, its supervisor (not running yet) and the fake connector behind
/// it.
pub(crate) fn fleet(queue: usize) -> (Fleet, TestSupervisor, FakeConnector) {
    let fake = FakeConnector::new();
    let config = RuntimeConfig {
        supervisor_queue: NonZeroUsize::new(queue).unwrap(),
        ..RuntimeConfig::default()
    };
    let (fleet, supervisor) = Fleet::new(FleetParts {
        connector: Arc::new(fake.clone()),
        credentials: Arc::new(OfflineCredentials),
        retry: RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap(),
        circuit: CircuitPolicy::try_new(NonZeroUsize::new(8).unwrap(), secs(600), secs(900))
            .unwrap(),
        config,
        anchor: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        seed: 1,
    })
    .unwrap();
    (fleet, supervisor, fake)
}

/// Lets every spawned task run until it waits; time doesn't move.
pub(crate) async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// Moves paused time forward by `duration` and lets the tasks run.
pub(crate) async fn advance(duration: Duration) {
    tokio::time::advance(duration).await;
    settle().await;
}
