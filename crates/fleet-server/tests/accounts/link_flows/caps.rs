//! The caps (§7.4, ADR-0018): 10 starts a day per user, counted from the
//! audit log; 3 users with a flow at once; and the Member quota.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::authz::Role;
use fleet_server::app::link_flows::{LinkFlowSettings, LinkFlowStatus, LinkStartError};
use fleet_server::ports::msauth::{Failure, PollOutcome};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{held_start, script_link};
use crate::support::link::{LinkSetup, drive_until, meta, settings};
use crate::support::msauth::{Script, gated};
use crate::support::service::{PROFILE, bounded};

fn with(total: u32, per_day: u32, max_per_member: u32) -> LinkFlowSettings {
    LinkFlowSettings {
        total: NonZeroU32::new(total).unwrap(),
        per_user_per_day: NonZeroU32::new(per_day).unwrap(),
        max_per_member: NonZeroU32::new(max_per_member).unwrap(),
    }
}

// --- The daily cap -------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_eleventh_start_in_a_day_waits_until_one_is_a_day_old(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    for n in 0..10 {
        held_start(&setup, &alice, &format!("device-code-{n}")).await;
    }

    let refused = setup.start(&alice).await;
    setup
        .clock
        .advance(Duration::from_hours(24) - Duration::from_millis(1));
    let still = setup.start(&alice).await;
    setup.clock.advance(Duration::from_millis(1));
    let again = held_start(&setup, &alice, "device-code-again").await;

    assert!(
        matches!(refused, Err(LinkStartError::DailyCap { retry_after }) if retry_after == Duration::from_hours(24)),
        "{refused:?}"
    );
    assert!(
        matches!(still, Err(LinkStartError::DailyCap { retry_after }) if retry_after == Duration::from_millis(1)),
        "{still:?}"
    );
    assert!(setup.status(again).is_some());
    // Microsoft is never asked for a refused start.
    assert_eq!(setup.starts(), 11);
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await.len(),
        11
    );
}

#[sqlx::test(migrations = false)]
async fn the_daily_cap_survives_a_restart(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = LinkSetup::with(options, with(3, 2, 3)).await;
    let alice = setup.person(1, Role::Member).await;
    held_start(&setup, &alice, "device-code-1").await;
    held_start(&setup, &alice, "device-code-2").await;

    let restarted = setup.restarted();
    let refused = bounded(restarted.start(&alice)).await;

    assert!(
        matches!(refused, Err(LinkStartError::DailyCap { .. })),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn refused_starts_dont_count_and_replacing_ones_do(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(3, 2, 3)).await;
    let alice = setup.person(1, Role::Member).await;
    held_start(&setup, &alice, "device-code-1").await;
    setup
        .fake
        .script_start(Script::Answer(Err(Failure::Unavailable { status: 503 })));
    setup.start(&alice).await.unwrap_err();

    // Replaces the first flow, and counts.
    held_start(&setup, &alice, "device-code-2").await;
    let refused = setup.start(&alice).await;

    assert!(
        matches!(refused, Err(LinkStartError::DailyCap { .. })),
        "{refused:?}"
    );
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await.len(),
        2
    );
}

#[sqlx::test(migrations = false)]
async fn each_user_has_their_own_daily_cap(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(3, 1, 3)).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    held_start(&setup, &alice, "device-code-1").await;

    let bobs = held_start(&setup, &bob, "device-code-2").await;

    assert!(setup.status(bobs).is_some());
}

// --- The server-wide cap ----------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_fourth_user_finds_every_place_taken(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    for n in 1..=3 {
        let user = setup.person(n, Role::Member).await;
        held_start(&setup, &user, &format!("device-code-{n}")).await;
    }
    let dave = setup.person(4, Role::Admin).await;

    let refused = setup.start(&dave).await;

    assert!(
        matches!(refused, Err(LinkStartError::AllBusy)),
        "{refused:?}"
    );
    assert_eq!(setup.starts(), 3);
}

#[sqlx::test(migrations = false)]
async fn a_start_in_progress_holds_a_place_until_its_request_is_dropped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(1, 10, 3)).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let (script, mut gate) = gated();
    setup.fake.script_start(script);
    let mut start = Box::pin(setup.service.start(&alice));
    drive_until(&mut start, gate.entered()).await;

    let refused = setup.start(&bob).await;
    drop(start);
    let bobs = held_start(&setup, &bob, "device-code-2").await;

    assert!(
        matches!(refused, Err(LinkStartError::AllBusy)),
        "{refused:?}"
    );
    assert!(setup.status(bobs).is_some());
}

#[sqlx::test(migrations = false)]
async fn a_finished_flow_frees_its_place(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = LinkSetup::with(options, with(1, 10, 3)).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Declined)));
    let alices = setup.started(&alice, "device-code-1").await;
    assert_eq!(setup.ended(alices).await, LinkFlowStatus::Declined);

    let bobs = held_start(&setup, &bob, "device-code-2").await;

    assert!(setup.status(bobs).is_some());
}

// --- The Member quota --------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_member_at_the_quota_is_refused_before_microsoft(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    for n in 1..=3 {
        setup
            .add_account(alice.actor.id, n, Some(AccountStatus::Active))
            .await;
    }

    let refused = setup.start(&alice).await;

    assert!(
        matches!(refused, Err(LinkStartError::OverQuota)),
        "{refused:?}"
    );
    assert_eq!(setup.starts(), 0);
    assert_eq!(setup.service.flows().active(), 0);
}

#[sqlx::test(migrations = false)]
async fn every_account_a_member_owns_counts_toward_the_quota(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(3, 10, 1)).await;
    let statuses = [
        Some(AccountStatus::Pending),
        Some(AccountStatus::Active),
        Some(AccountStatus::ReauthRequired),
        Some(AccountStatus::Refused(RefusalReason::NotOwned)),
        None,
    ];

    for (n, status) in (1_u8..).zip(statuses) {
        let member = setup.person(n, Role::Member).await;
        setup.add_account(member.actor.id, n, status).await;

        let refused = setup.start(&member).await;

        assert!(
            matches!(refused, Err(LinkStartError::OverQuota)),
            "{status:?}: {refused:?}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn another_users_accounts_dont_count(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(3, 10, 1)).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    setup
        .add_account(bob.actor.id, 1, Some(AccountStatus::Active))
        .await;

    let alices = held_start(&setup, &alice, "device-code-1").await;

    assert!(setup.status(alices).is_some());
}

#[sqlx::test(migrations = false)]
async fn admins_and_the_owner_have_no_quota(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(3, 10, 1)).await;

    for (n, role) in [(1_u8, Role::Admin), (2, Role::Owner)] {
        let user = setup.person(n, role).await;
        for account in 1..=5 {
            setup
                .add_account(user.actor.id, n * 10 + account, Some(AccountStatus::Active))
                .await;
        }

        let flow = held_start(&setup, &user, &format!("device-code-{n}")).await;

        assert!(setup.status(flow).is_some(), "{role:?}");
    }
}

#[sqlx::test(migrations = false)]
async fn an_account_added_during_the_wait_makes_the_commit_over_quota(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(3, 10, 1)).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    setup
        .add_account(alice.actor.id, 1, Some(AccountStatus::Active))
        .await;
    script_link(&setup, 1);

    wait.release();

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::OverQuota);
    assert_eq!(setup.owned(alice.actor.id).await, 1);
    let failures = setup.entries(&AuditAction::ACCOUNT_LINK).await;
    assert_eq!(failures.len(), 1);
    let failure = &failures[0];
    assert_eq!(failure.outcome, AuditOutcome::Failure);
    assert_eq!(failure.actor, Some(alice.actor.id));
    assert_eq!(
        failure.target,
        Some(RecordedTarget::Known(AuditTarget::User(alice.actor.id)))
    );
    let text = |value: &str| Some(RecordedValue::Text(value.to_owned()));
    assert_eq!(meta(failure, "reason").cloned(), text("over_quota"));
    assert_eq!(meta(failure, "kind").cloned(), text("microsoft"));
    assert_eq!(meta(failure, "name").cloned(), text("AfkBot1"));
    assert_eq!(meta(failure, "uuid").cloned(), text(PROFILE));
    assert_eq!(meta(failure, "flow").cloned(), text(&flow.to_string()));
}

#[sqlx::test(migrations = false)]
async fn the_quota_follows_the_role_at_the_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::with(options, with(3, 10, 1)).await;
    let admin = setup.person(1, Role::Admin).await;
    setup
        .add_account(admin.actor.id, 1, Some(AccountStatus::Active))
        .await;
    let mut wait = setup.pace.hold_next();
    let flow = setup.started(&admin, "device-code-1").await;
    wait.entered().await;
    setup.set_role(&admin, Role::Member).await;
    script_link(&setup, 1);

    wait.release();

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::OverQuota);
}

#[test]
fn the_default_caps_are_section_7_4s() {
    let caps = settings();

    assert_eq!(caps.total.get(), 3);
    assert_eq!(caps.per_user_per_day.get(), 10);
    assert_eq!(caps.max_per_member.get(), 3);
}
