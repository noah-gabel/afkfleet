//! Storing a linked account (ADR-0018): the creator owns it, which gives
//! them Manage; it waits for approval unless the Owner linked it; a
//! Minecraft account is linked once; and the commit re-checks the creator
//! and the session that started the flow.

use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::authz::{Actor, AuthzError, Permission, ResourceContext, Role, UserRef, authorize};
use fleet_server::app::link_flows::LinkFlowStatus;
use fleet_server::app::users::ActingUser;
use fleet_server::ports::accounts::AccountCredential;
use fleet_server::ports::audit::AuditRecord;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use fleet_core::system::Clock as _;

use super::{approval, script_link};
use crate::support::link::{IP, LinkSetup, meta};
use crate::support::msauth::{Script, session};
use crate::support::service::{DAY, PROFILE, T0, at, profile};

/// Links `PROFILE` as `AfkBot1` for `by`; how the flow ended.
async fn linked(setup: &LinkSetup, by: &ActingUser) -> LinkFlowStatus {
    script_link(setup, 1);
    let flow = setup.started(by, "device-code-1").await;
    setup.ended(flow).await
}

#[sqlx::test(migrations = false)]
async fn the_creator_owns_the_stored_account_with_its_profile_and_status(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let cases = [
        (Role::Member, AccountStatus::Pending),
        (Role::Admin, AccountStatus::Pending),
        (Role::Owner, AccountStatus::Active),
    ];

    for (n, (role, status)) in (1_u8..).zip(cases) {
        let creator = setup.person(n, role).await;
        let name = format!("AfkBot{n}");
        setup.fake.script_poll(Script::Answer(Ok(approval(n))));
        setup.fake.script_session(Script::Answer(Ok(session(
            &format!("minecraft-{n}"),
            &profile(n),
            &name,
            at(T0 + DAY),
        ))));
        let flow = setup.started(&creator, &format!("device-code-{n}")).await;

        let ended = setup.ended(flow).await;

        let LinkFlowStatus::Linked {
            account,
            status: linked_status,
            name: linked_name,
        } = ended
        else {
            panic!("{role:?}: {ended:?}");
        };
        assert_eq!(linked_status, status, "{role:?}");
        assert_eq!(linked_name.as_str(), name);
        let stored = setup.account(account).await;
        assert_eq!(stored.owner, creator.actor.id);
        assert_eq!(stored.name.as_str(), name);
        assert_eq!(stored.created_at, setup.clock.now());
        assert_eq!(stored.status(), status);
        let AccountCredential::Microsoft { uuid, .. } = stored.credential else {
            panic!("a Microsoft account");
        };
        assert_eq!(uuid.to_string(), profile(n));
    }
}

#[sqlx::test(migrations = false)]
async fn the_refresh_token_is_sealed_for_the_accounts_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;

    let LinkFlowStatus::Linked { account, .. } = linked(&setup, &alice).await else {
        panic!("linked");
    };

    assert_eq!(setup.stored_token(account).await, "refresh-1");
}

#[sqlx::test(migrations = false)]
async fn account_link_records_the_creator_and_the_minecraft_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    script_link(&setup, 1);
    let flow = setup.started(&alice, "device-code-1").await;

    let LinkFlowStatus::Linked { account, .. } = setup.ended(flow).await else {
        panic!("linked");
    };

    let entries = setup.entries(&AuditAction::ACCOUNT_LINK).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(alice.actor.id));
    assert_eq!(entry.ip, Some(IP));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Account(account)))
    );
    let text = |value: &str| Some(RecordedValue::Text(value.to_owned()));
    assert_eq!(meta(entry, "kind").cloned(), text("microsoft"));
    assert_eq!(meta(entry, "name").cloned(), text("AfkBot1"));
    assert_eq!(meta(entry, "uuid").cloned(), text(PROFILE));
    assert_eq!(meta(entry, "status").cloned(), text("pending"));
    assert_eq!(meta(entry, "flow").cloned(), text(&flow.to_string()));
    assert_eq!(entry.metadata.entries().len(), 5);
}

#[sqlx::test(migrations = false)]
async fn the_creator_manages_the_new_account_and_another_member_gets_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;

    let LinkFlowStatus::Linked { account, .. } = linked(&setup, &alice).await else {
        panic!("linked");
    };

    let stored = setup.account(account).await;
    let context = ResourceContext::Account {
        owner: UserRef {
            id: stored.owner,
            role: Role::Member,
        },
        grant: None,
    };
    let as_actor = |by: &ActingUser| Actor {
        id: by.actor.id,
        role: by.actor.role,
    };
    for manage in [
        Permission::DeleteAccount,
        Permission::RelinkAccount,
        Permission::ManageGrants,
    ] {
        assert_eq!(authorize(&as_actor(&alice), manage, &context), Ok(()));
        assert_eq!(
            authorize(&as_actor(&bob), manage, &context),
            Err(AuthzError::NotFound)
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_linked_minecraft_account_is_taken_and_recorded_without_naming_its_owner(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Owner).await;
    assert!(matches!(
        linked(&setup, &alice).await,
        LinkFlowStatus::Linked { .. }
    ));

    let ended = linked(&setup, &bob).await;

    assert_eq!(ended, LinkFlowStatus::Taken);
    assert_eq!(setup.owned(bob.actor.id).await, 0);
    let entries = setup.entries(&AuditAction::ACCOUNT_LINK).await;
    assert_eq!(entries.len(), 2);
    let failure = &entries[1];
    assert_eq!(failure.outcome, AuditOutcome::Failure);
    assert_eq!(failure.actor, Some(bob.actor.id));
    assert_eq!(
        failure.target,
        Some(RecordedTarget::Known(AuditTarget::User(bob.actor.id)))
    );
    let text = |value: &str| Some(RecordedValue::Text(value.to_owned()));
    assert_eq!(meta(failure, "reason").cloned(), text("taken"));
    assert_eq!(meta(failure, "uuid").cloned(), text(PROFILE));
    assert_eq!(meta(failure, "name").cloned(), text("AfkBot1"));
    assert_eq!(meta(failure, "kind").cloned(), text("microsoft"));
    assert_eq!(failure.metadata.entries().len(), 5);
}

// --- The re-checks of the creator and their session ------------------------------

#[sqlx::test(migrations = false)]
async fn a_revoked_session_fails_the_flow_and_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    script_link(&setup, 1);
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    setup.revoke(&alice).await;

    wait.release();

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Failed);
    assert_eq!(setup.owned(alice.actor.id).await, 0);
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_LINK).await,
        Vec::<AuditRecord>::new()
    );
}

#[sqlx::test(migrations = false)]
async fn a_disabled_creator_fails_the_flow_and_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    script_link(&setup, 1);
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    setup.disable(&alice).await;

    wait.release();

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Failed);
    assert_eq!(setup.owned(alice.actor.id).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_session_refreshed_during_the_wait_still_links(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    script_link(&setup, 1);
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    setup.rotate(&alice, 1).await;

    wait.release();

    assert!(matches!(
        setup.ended(flow).await,
        LinkFlowStatus::Linked { .. }
    ));
    assert_eq!(setup.owned(alice.actor.id).await, 1);
}
