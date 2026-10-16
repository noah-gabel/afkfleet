//! Deleting an account (Plan.md P9.6, ADR-0018): `DELETE
//! /api/v1/accounts/{id}`, through serve's own app. Manage deletes it in any
//! status and of any kind (offline ones too while `[accounts]
//! allow_offline` is off), after a step-up, with its grants; the Member's
//! quota slot is free again; the deletion is audited; and the service
//! re-checks everything in its transaction.

use std::sync::Arc;

use axum::http::Method;
use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_core::authz::{AuthzError, GrantLevel, Role};
use fleet_core::id::AccountId;
use fleet_server::app::accounts::{AccountService, AccountSettings, DeleteAccountError};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::{ManualClock, SeededRandom};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::http::{HttpSetup, Person, Seen, meta, text};
use crate::support::service::{T0, at, profile};

async fn delete(setup: &HttpSetup, by: &Person, id: AccountId) -> Seen {
    setup
        .api(Method::DELETE, &format!("/accounts/{id}"), by, None)
        .await
}

#[sqlx::test(migrations = false)]
async fn the_owner_deletes_their_account_with_its_grants_and_it_is_audited(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let member = setup.stepped_up(1, Role::Member).await;
    let friend = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(
            member.id,
            3,
            Some(AccountStatus::Refused(RefusalReason::NoProfile)),
        )
        .await;
    setup
        .add_grant(id, friend.id, GrantLevel::Control, member.id)
        .await;
    let bot = setup.add_bot(id, 3).await;

    let deleted = delete(&setup, &member, id).await;

    assert_eq!(deleted.status, 204, "{}", deleted.text);
    assert_eq!(deleted.text, "");
    assert_eq!(setup.stored(id).await, None);
    assert_eq!(setup.grant_of(id, friend.id).await, None);
    let entries = setup.entries(&AuditAction::ACCOUNT_DELETE).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(member.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Account(id)))
    );
    assert_eq!(meta(entry, "owner"), Some(text(&member.id.to_string())));
    assert_eq!(meta(entry, "kind"), Some(text("microsoft")));
    assert_eq!(meta(entry, "name"), Some(text("AfkBot3")));
    assert_eq!(meta(entry, "status"), Some(text("refused")));
    assert_eq!(meta(entry, "uuid"), Some(text(&profile(3))));
    assert_eq!(meta(entry, "bot"), Some(text(&bot.to_string())));
    assert_eq!(entry.metadata.entries().len(), 6);
    assert_eq!(setup.db.bots().find(bot).await.unwrap(), None);
}

#[sqlx::test(migrations = false)]
async fn an_offline_account_is_deleted_while_offline_accounts_are_off_freeing_the_slot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // `AccountsConfig::default()` has `allow_offline` off.
    let setup = HttpSetup::new(options).await;
    let member = setup.stepped_up(1, Role::Member).await;
    let id = setup.add_account(member.id, 2, None).await;

    let deleted = delete(&setup, &member, id).await;

    assert_eq!(deleted.status, 204, "{}", deleted.text);
    assert_eq!(setup.stored(id).await, None);
    assert_eq!(
        setup.db.accounts().count_owned_by(member.id).await.unwrap(),
        0
    );
    let entries = setup.entries(&AuditAction::ACCOUNT_DELETE).await;
    assert_eq!(meta(&entries[0], "kind"), Some(text("offline")));
    assert_eq!(meta(&entries[0], "uuid"), None);
}

#[sqlx::test(migrations = false)]
async fn manage_deletes_any_status_and_below_manage_or_unseen_it_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.stepped_up(1, Role::Admin).await;
    let other_admin = setup.person(2, Role::Admin).await;
    let member = setup.person(3, Role::Member).await;
    let manager = setup.stepped_up(4, Role::Member).await;
    let controller = setup.stepped_up(5, Role::Member).await;
    let stranger = setup.stepped_up(6, Role::Member).await;
    let pending = setup
        .add_account(member.id, 7, Some(AccountStatus::Pending))
        .await;
    let active = setup
        .add_account(member.id, 8, Some(AccountStatus::Active))
        .await;
    let admins = setup.add_account(other_admin.id, 9, None).await;
    setup
        .add_grant(active, manager.id, GrantLevel::Manage, member.id)
        .await;
    setup
        .add_grant(pending, controller.id, GrantLevel::Control, member.id)
        .await;

    let refused = [
        ("a control grantee", &controller, pending, 403),
        ("a stranger", &stranger, active, 404),
        ("an admin on another admin's", &admin, admins, 404),
    ];
    for (case, by, id, status) in refused {
        let seen = delete(&setup, by, id).await;
        assert_eq!(seen.status, status, "{case}: {}", seen.text);
        assert!(setup.stored(id).await.is_some(), "{case}");
    }
    let by_admin = delete(&setup, &admin, pending).await;
    let by_manager = delete(&setup, &manager, active).await;

    assert_eq!(by_admin.status, 204, "{}", by_admin.text);
    assert_eq!(by_manager.status, 204, "{}", by_manager.text);
    assert_eq!(setup.stored(pending).await, None);
    assert_eq!(setup.stored(active).await, None);
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_DELETE).await.len(), 2);
}

#[sqlx::test(migrations = false)]
async fn deleting_needs_a_fresh_step_up_and_changes_nothing_without_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let member = setup.person(1, Role::Member).await;
    let id = setup.add_account(member.id, 2, None).await;

    let deleted = delete(&setup, &member, id).await;

    assert_eq!(deleted.status, 403, "{}", deleted.text);
    assert_eq!(deleted.code(), "step_up_required");
    assert!(setup.stored(id).await.is_some());
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_DELETE).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_account_is_404_and_a_deletion_spends_the_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let one = Rate::per_second(core::num::NonZeroU32::MIN, core::num::NonZeroU32::MIN);
    let limits = RateLimitConfig {
        per_user: one,
        ..RateLimitConfig::default()
    };
    let setup = HttpSetup::with(
        options,
        limits,
        fleet_server::config::AccountsConfig::default(),
    )
    .await;
    let owner = setup.stepped_up(1, Role::Owner).await;
    let unknown = AccountId::new_v7(at(T0), [99; 10]).unwrap();

    let first = delete(&setup, &owner, unknown).await;
    let second = delete(&setup, &owner, unknown).await;

    assert_eq!(first.status, 404, "{}", first.text);
    assert_eq!(second.status, 429, "{}", second.text);
    assert!(second.retry_after.is_some());
}

#[sqlx::test(migrations = false)]
async fn the_deletion_re_checks_the_actors_grant_and_session_in_its_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let member = setup.person(1, Role::Member).await;
    let manager = setup.person(2, Role::Member).await;
    let gone_session = setup.person(3, Role::Admin).await;
    let id = setup.add_account(member.id, 4, None).await;
    // The handler saw the grant; it's revoked before the service runs.
    let accounts = AccountService::new(
        Arc::new(ManualClock::new(at(T0))),
        Arc::new(SeededRandom::new(5)),
        Arc::new(setup.db.clone()),
        AccountSettings {
            allow_offline: false,
            max_per_member: core::num::NonZeroU32::MIN,
        },
    );
    setup.set_disabled(gone_session.id, true).await;

    let without_grant = accounts.delete(&manager.acting(), id).await;
    let without_session = accounts.delete(&gone_session.acting(), id).await;
    let unknown = accounts
        .delete(
            &member.acting(),
            AccountId::new_v7(at(T0), [99; 10]).unwrap(),
        )
        .await;

    assert!(
        matches!(
            without_grant,
            Err(DeleteAccountError::Denied(AuthzError::NotFound))
        ),
        "{without_grant:?}"
    );
    assert!(
        matches!(without_session, Err(DeleteAccountError::SessionGone)),
        "{without_session:?}"
    );
    assert!(
        matches!(unknown, Err(DeleteAccountError::NotFound)),
        "{unknown:?}"
    );
    assert!(setup.stored(id).await.is_some());
}
