//! How long a finished flow's result stays readable (ADR-0018): 15 minutes,
//! then it's unknown, and the housekeeping's purge drops it.

use core::time::Duration;
use std::sync::Arc;

use fleet_core::auth::LockoutPolicy;
use fleet_core::authz::Role;
use fleet_core::system::Clock;
use fleet_server::app::challenges::Challenges;
use fleet_server::app::link_flows::{FINISHED_RETENTION, LinkFlowStatus};
use fleet_server::app::lockout::LockoutService;
use fleet_server::cli::housekeeping::Housekeeping;
use fleet_server::config::RateLimitConfig;
use fleet_server::http::rate_limit::RateLimits;
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::ports::msauth::PollOutcome;
use fleet_testkit::system::SeededRandom;
use secrecy::SecretBox;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_util::sync::CancellationToken;

use super::held_start;
use crate::support::link::LinkSetup;
use crate::support::msauth::Script;
use crate::support::service::bounded;

#[sqlx::test(migrations = false)]
async fn a_finished_result_reads_for_15_minutes_then_is_unknown(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Declined)));
    let flow = setup.started(&alice, "device-code-1").await;
    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Declined);

    setup.clock.advance(
        FINISHED_RETENTION
            .checked_sub(Duration::from_millis(1))
            .unwrap(),
    );
    let still = setup.status(flow);
    setup.clock.advance(Duration::from_millis(1));

    assert_eq!(still, Some(LinkFlowStatus::Declined));
    assert_eq!(setup.status(flow), None);
    assert_eq!(FINISHED_RETENTION, Duration::from_mins(15));
}

#[sqlx::test(migrations = false)]
async fn the_purge_drops_finished_flows_and_keeps_waiting_ones(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Expired)));
    let finished = setup.started(&alice, "device-code-1").await;
    assert_eq!(setup.ended(finished).await, LinkFlowStatus::Expired);
    let waiting = held_start(&setup, &bob, "device-code-2").await;
    setup.clock.advance(FINISHED_RETENTION);

    let dropped = setup.service.flows().purge_finished();

    assert_eq!(dropped, 1);
    assert_eq!(setup.service.flows().held(), 1);
    assert!(matches!(
        setup.status(waiting),
        Some(LinkFlowStatus::Waiting { .. })
    ));
}

#[sqlx::test(migrations = false)]
async fn each_housekeeping_run_drops_the_finished_flows(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Declined)));
    let flow = setup.started(&alice, "device-code-1").await;
    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Declined);
    let clock: Arc<dyn Clock> = setup.clock.clone();
    let housekeeping = Housekeeping::new(
        RateLimits::new(&RateLimitConfig::default(), Arc::clone(&clock)),
        LockoutService::new(
            Arc::clone(&clock),
            Arc::new(setup.db.clone()),
            Arc::new(HmacAttemptKeys::new(SecretBox::new(Box::new([7; 32])))),
            LockoutPolicy::default(),
        ),
        Challenges::new(
            Arc::clone(&clock),
            Arc::new(Sha256Tokens::new(Arc::new(SeededRandom::new(5)))),
        ),
        setup.service.flows(),
    );
    setup.clock.advance(FINISHED_RETENTION);

    bounded(housekeeping.run_once(&CancellationToken::new())).await;

    assert_eq!(setup.service.flows().held(), 0);
}
