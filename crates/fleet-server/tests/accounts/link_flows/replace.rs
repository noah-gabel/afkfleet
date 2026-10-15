//! One flow per user (ADR-0018): a new start replaces the user's waiting
//! flow, never one Microsoft already approved.

use fleet_core::audit::AuditAction;
use fleet_core::authz::Role;
use fleet_core::system::Clock as _;
use fleet_server::app::link_flows::{LinkFlowStatus, LinkStartError};
use fleet_server::ports::msauth::PollOutcome;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{approval, held_start, profile_session};
use crate::support::link::{LinkSetup, drive_until};
use crate::support::msauth::{Script, device_flow, gated};
use crate::support::service::bounded;

#[sqlx::test(migrations = false)]
async fn a_new_start_replaces_the_waiting_flow_whose_id_is_then_unknown(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let first = held_start(&setup, &alice, "device-code-1").await;

    let second = held_start(&setup, &alice, "device-code-2").await;

    assert_eq!(setup.status(first), None);
    assert!(matches!(
        setup.status(second),
        Some(LinkFlowStatus::Waiting { .. })
    ));
    assert_eq!(setup.service.flows().active(), 1);
}

#[sqlx::test(migrations = false)]
async fn the_replaced_code_is_never_polled_again(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (in_flight, mut gate) = gated();
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Pending)));
    setup.fake.script_poll(in_flight);
    setup.started(&alice, "device-code-1").await;
    gate.entered().await;

    held_start(&setup, &alice, "device-code-2").await;
    // An approval of the replaced code now reaches nobody.
    gate.release(Ok(approval(1)));
    tokio::task::yield_now().await;

    assert_eq!(setup.polled_with(), ["device-code-1", "device-code-1"]);
    assert_eq!(setup.sessions_started(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_finishing_flow_isnt_replaced(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (session, mut gate) = gated();
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup.fake.script_session(session);
    let flow = setup.started(&alice, "device-code-1").await;
    gate.entered().await;

    let refused = setup.start(&alice).await;
    gate.release(Ok(profile_session()));

    assert!(
        matches!(refused, Err(LinkStartError::InProgress)),
        "{refused:?}"
    );
    assert_eq!(setup.starts(), 1);
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await.len(),
        1
    );
    assert!(matches!(
        setup.ended(flow).await,
        LinkFlowStatus::Linked { .. }
    ));
}

#[sqlx::test(migrations = false)]
async fn a_flow_approved_while_a_new_start_asks_microsoft_wins(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup.fake.script_session(Script::Hang);
    let first = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    let (start, mut asked) = gated();
    setup.fake.script_start(start);
    let mut second = Box::pin(setup.service.start(&alice));
    drive_until(&mut second, asked.entered()).await;

    wait.release();
    setup.finishing(first).await;
    asked.release(Ok(device_flow(
        "device-code-2",
        "C8Y5-LEMR",
        setup.clock.now(),
    )));
    let refused = bounded(second).await;

    assert!(
        matches!(refused, Err(LinkStartError::InProgress)),
        "{refused:?}"
    );
    assert_eq!(setup.status(first), Some(LinkFlowStatus::Finishing));
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await.len(),
        1
    );
}

#[sqlx::test(migrations = false)]
async fn a_second_start_while_the_first_asks_microsoft_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (start, mut asked) = gated();
    setup.fake.script_start(start);
    let mut first = Box::pin(setup.service.start(&alice));
    drive_until(&mut first, asked.entered()).await;

    let refused = setup.start(&alice).await;

    assert!(
        matches!(refused, Err(LinkStartError::InProgress)),
        "{refused:?}"
    );
    assert_eq!(setup.starts(), 1);
}

#[sqlx::test(migrations = false)]
async fn a_new_start_replaces_a_finished_result(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Declined)));
    let first = setup.started(&alice, "device-code-1").await;
    assert_eq!(setup.ended(first).await, LinkFlowStatus::Declined);

    let second = held_start(&setup, &alice, "device-code-2").await;

    assert_eq!(setup.status(first), None);
    assert!(setup.status(second).is_some());
}
