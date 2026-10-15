//! Shutdown (ADR-0018): every flow stops at once, whatever it's waiting
//! for; only a commit that started finishes.

use fleet_core::authz::Role;
use fleet_core::id::{LinkFlowId, UserId};
use fleet_server::app::link_flows::LinkFlowStatus;
use fleet_server::ports::msauth::{Failure, SessionError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{approval, script_link};
use crate::support::link::LinkSetup;
use crate::support::msauth::{Script, gated};
use crate::support::service::{bounded, poll_once};

/// Stops the service, and checks that the flow `flow` of `owner` ended
/// without storing anything.
async fn stopped_without_storing(setup: &LinkSetup, flow: LinkFlowId, owner: UserId) {
    bounded(setup.service.shutdown()).await;

    assert_eq!(setup.status(flow), Some(LinkFlowStatus::Failed));
    assert_eq!(setup.owned(owner).await, 0);
    assert_eq!(setup.service.flows().active(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_flow_waiting_between_polls_stops_at_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;

    stopped_without_storing(&setup, flow, alice.actor.id).await;
    drop(wait);
}

#[sqlx::test(migrations = false)]
async fn a_poll_in_flight_is_dropped(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (poll, mut gate) = gated();
    setup.fake.script_poll(poll);
    let flow = setup.started(&alice, "device-code-1").await;
    gate.entered().await;

    stopped_without_storing(&setup, flow, alice.actor.id).await;
    drop(gate);
}

#[sqlx::test(migrations = false)]
async fn a_session_start_in_flight_is_dropped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (session, mut gate) = gated();
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup.fake.script_session(session);
    let flow = setup.started(&alice, "device-code-1").await;
    gate.entered().await;

    stopped_without_storing(&setup, flow, alice.actor.id).await;
    drop(gate);
}

#[sqlx::test(migrations = false)]
async fn a_wait_before_a_retry_stops_at_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (session, mut gate) = gated();
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup.fake.script_session(session);
    let flow = setup.started(&alice, "device-code-1").await;
    gate.entered().await;
    let mut retry = setup.pace.hold_next();
    gate.release(Err(SessionError::Failed(Failure::TimedOut)));
    retry.entered().await;

    stopped_without_storing(&setup, flow, alice.actor.id).await;
    drop(retry);
}

#[sqlx::test(migrations = false)]
async fn a_wait_before_a_commit_retry_stops_at_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    script_link(&setup, 1);
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    setup.store.fail_next_writes(1);
    let mut retry = setup.pace.hold_next();
    wait.release();
    retry.entered().await;

    stopped_without_storing(&setup, flow, alice.actor.id).await;
    drop(retry);
}

#[sqlx::test(migrations = false)]
async fn a_commit_that_started_finishes_before_shutdown_returns(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    script_link(&setup, 1);
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    let mut write = setup.store.gate_next_write();
    wait.release();
    write.entered().await;

    let mut shutdown = Box::pin(setup.service.shutdown());
    assert!(poll_once(&mut shutdown).await.is_none());
    write.release();
    bounded(shutdown).await;

    assert!(matches!(
        setup.status(flow),
        Some(LinkFlowStatus::Linked { .. })
    ));
    assert_eq!(setup.owned(alice.actor.id).await, 1);
}
