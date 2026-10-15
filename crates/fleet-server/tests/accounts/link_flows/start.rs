//! What a start answers and records, and the starts that fail.

use core::num::NonZeroU32;
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::authz::Role;
use fleet_core::system::RandomError;

use fleet_server::app::link_flows::{LinkFlowSettings, LinkFlowStatus, LinkStartError};
use fleet_server::ports::audit::AuditRecord;
use fleet_server::ports::msauth::{Endpoint, Failure, Rule};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::held_start;
use crate::support::link::{CODE_LIFETIME, IP, LinkSetup, USER_CODE, drive_until, meta, settings};
use crate::support::msauth::{Script, gated};
use crate::support::service::{T0, at, bounded};

/// The caps with one place on the whole server, so a start that kept its
/// place would refuse the next.
fn one_place() -> LinkFlowSettings {
    LinkFlowSettings {
        total: NonZeroU32::MIN,
        ..settings()
    }
}

#[sqlx::test(migrations = false)]
async fn a_start_answers_the_code_and_records_account_start_link(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    drop(setup.pace.hold_next());
    setup.script_start("device-code-1");

    let started = setup.start(&alice).await.unwrap();

    assert_eq!(started.user_code.as_str(), USER_CODE);
    assert_eq!(
        started.verification_uri.as_str(),
        "https://www.microsoft.com/link"
    );
    assert_eq!(started.expires_at, at(T0 + CODE_LIFETIME));
    assert_eq!(
        setup.status(started.id),
        Some(LinkFlowStatus::Waiting {
            expires_at: at(T0 + CODE_LIFETIME)
        })
    );
    let view = setup.service.status(started.id).unwrap();
    assert_eq!(view.creator, alice.actor.id);
    let entries = setup.entries(&AuditAction::ACCOUNT_START_LINK).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(alice.actor.id));
    assert_eq!(entry.ip, Some(IP));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(alice.actor.id)))
    );
    assert_eq!(
        meta(entry, "flow"),
        Some(&RecordedValue::Text(started.id.to_string()))
    );
    assert_eq!(entry.metadata.entries().len(), 1);
}

#[sqlx::test(migrations = false)]
async fn every_start_holds_a_place_until_its_flow_ends(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;

    held_start(&setup, &alice, "device-code-1").await;
    held_start(&setup, &bob, "device-code-2").await;

    assert_eq!(setup.service.flows().active(), 2);
}

#[sqlx::test(migrations = false)]
async fn a_session_gone_before_the_start_commits_records_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup.revoke(&alice).await;
    setup.script_start("device-code-1");

    let refused = setup.start(&alice).await;

    assert!(
        matches!(refused, Err(LinkStartError::SessionGone)),
        "{refused:?}"
    );
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await,
        Vec::<AuditRecord>::new()
    );
    assert_eq!(setup.service.flows().active(), 0);
    assert_eq!(setup.service.flows().held(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_failed_microsoft_start_records_nothing_and_frees_the_place(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, one_place()).await;
    let alice = setup.person(1, Role::Member).await;
    let failures = [
        Failure::Unreachable,
        Failure::TimedOut,
        Failure::RateLimited,
        Failure::Unavailable { status: 503 },
        Failure::Certificate,
        Failure::Unexpected {
            endpoint: Endpoint::DeviceCode,
            rule: Rule::NotJson,
        },
    ];

    for failure in failures {
        setup
            .fake
            .script_start(Script::Answer(Err(failure.clone())));

        let refused = setup.start(&alice).await;

        match refused {
            Err(LinkStartError::Microsoft(got)) => assert_eq!(got, failure),
            other => panic!("{other:?}"),
        }
        assert_eq!(setup.service.flows().active(), 0);
    }
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await,
        Vec::<AuditRecord>::new()
    );
    assert_eq!(setup.service.flows().active(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_failed_flow_id_records_nothing_and_frees_the_place(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup.script_start("device-code-1");
    setup.random.fail_next(RandomError::Unavailable);

    let refused = setup.start(&alice).await;

    assert!(
        matches!(refused, Err(LinkStartError::Mint(_))),
        "{refused:?}"
    );
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await,
        Vec::<AuditRecord>::new()
    );
    assert_eq!(setup.service.flows().active(), 0);
    assert_eq!(setup.service.flows().held(), 0);
    assert_eq!(setup.polled_with(), Vec::<String>::new());
}

#[sqlx::test(migrations = false)]
async fn a_start_after_shutdown_is_refused_without_asking_microsoft(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    bounded(setup.service.shutdown()).await;

    let refused = setup.start(&alice).await;

    assert!(
        matches!(refused, Err(LinkStartError::ShuttingDown)),
        "{refused:?}"
    );
    assert_eq!(setup.starts(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_start_waiting_for_microsoft_stops_with_the_server(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (script, mut gate) = gated();
    setup.fake.script_start(script);
    let mut start = Box::pin(setup.service.start(&alice));
    drive_until(&mut start, gate.entered()).await;

    let (refused, ()) = bounded(async { tokio::join!(start, setup.service.shutdown()) }).await;

    assert!(
        matches!(refused, Err(LinkStartError::ShuttingDown)),
        "{refused:?}"
    );
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await,
        Vec::<AuditRecord>::new()
    );
    assert_eq!(setup.service.flows().active(), 0);
}
