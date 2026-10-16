//! Approving and refusing a pending link (Plan.md P9.6; ADR-0018, group
//! E): `POST /api/v1/accounts/{id}/approve` and `/refuse`, through serve's
//! own app. Someone above the owner reviews it, by the owner's current
//! role, after a step-up; only a pending account is reviewed; approving
//! makes it active and refusing deletes it with its grants; each is
//! audited, and the service re-checks everything in its transaction.

use std::sync::Arc;

use axum::http::Method;
use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_core::authz::{AuthzError, GrantLevel, Role};
use fleet_core::id::AccountId;
use fleet_server::app::accounts::{AccountService, AccountSettings, ReviewError};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::accounts::AccountCredential;
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::{ManualClock, SeededRandom};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::http::{HttpSetup, Person, Seen, meta, text};
use crate::support::service::{T0, at, profile};

async fn approve(setup: &HttpSetup, by: &Person, id: AccountId) -> Seen {
    setup
        .api(Method::POST, &format!("/accounts/{id}/approve"), by, None)
        .await
}

async fn refuse(setup: &HttpSetup, by: &Person, id: AccountId) -> Seen {
    setup
        .api(Method::POST, &format!("/accounts/{id}/refuse"), by, None)
        .await
}

/// The account service over `setup`'s database, as the handlers' service
/// is, for the re-checks a request can't reach.
fn service(setup: &HttpSetup) -> AccountService {
    AccountService::new(
        Arc::new(ManualClock::new(at(T0))),
        Arc::new(SeededRandom::new(5)),
        Arc::new(setup.db.clone()),
        AccountSettings {
            allow_offline: false,
            max_per_member: core::num::NonZeroU32::MIN,
        },
    )
}

#[sqlx::test(migrations = false)]
async fn an_admin_approves_a_members_pending_link_which_is_active_from_then_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.stepped_up(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(member.id, 3, Some(AccountStatus::Pending))
        .await;

    let approved = approve(&setup, &admin, id).await;

    assert_eq!(approved.status, 200, "{}", approved.text);
    assert_eq!(approved.body["status"], "active");
    assert_eq!(approved.body["level"], "manage");
    assert_eq!(approved.body["owner"]["username"], "user2");
    assert_eq!(
        setup.stored(id).await.unwrap().status(),
        AccountStatus::Active
    );
    let entries = setup.entries(&AuditAction::ACCOUNT_APPROVE).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(admin.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Account(id)))
    );
    assert_eq!(meta(entry, "owner"), Some(text(&member.id.to_string())));
    assert_eq!(meta(entry, "name"), Some(text("AfkBot3")));
    assert_eq!(meta(entry, "uuid"), Some(text(&profile(3))));
    assert_eq!(entry.metadata.entries().len(), 3);
}

#[sqlx::test(migrations = false)]
async fn the_owner_reviews_admins_and_members_links(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.stepped_up(1, Role::Owner).await;
    let admin = setup.person(2, Role::Admin).await;
    let member = setup.person(3, Role::Member).await;
    let admins = setup
        .add_account(admin.id, 4, Some(AccountStatus::Pending))
        .await;
    let members = setup
        .add_account(member.id, 5, Some(AccountStatus::Pending))
        .await;

    let approved = approve(&setup, &owner, admins).await;
    let refused = refuse(&setup, &owner, members).await;

    assert_eq!(approved.status, 200, "{}", approved.text);
    assert_eq!(refused.status, 204, "{}", refused.text);
    assert_eq!(
        setup.stored(admins).await.unwrap().status(),
        AccountStatus::Active
    );
    assert_eq!(setup.stored(members).await, None);
}

#[sqlx::test(migrations = false)]
async fn only_someone_above_the_owner_reviews_whatever_grant_anyone_holds(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.stepped_up(1, Role::Admin).await;
    let other_admin = setup.person(2, Role::Admin).await;
    let member = setup.stepped_up(3, Role::Member).await;
    let other_member = setup.stepped_up(4, Role::Member).await;
    let admins = setup
        .add_account(other_admin.id, 5, Some(AccountStatus::Pending))
        .await;
    let members = setup
        .add_account(member.id, 6, Some(AccountStatus::Pending))
        .await;
    let hidden = setup
        .add_account(other_admin.id, 7, Some(AccountStatus::Pending))
        .await;
    setup
        .add_grant(admins, admin.id, GrantLevel::Manage, other_admin.id)
        .await;
    setup
        .add_grant(members, other_member.id, GrantLevel::Manage, member.id)
        .await;

    let cases = [
        (
            "an admin granted manage on another admin's",
            &admin,
            admins,
            403,
        ),
        (
            "an admin on another admin's hidden one",
            &admin,
            hidden,
            404,
        ),
        ("a member granted manage", &other_member, members, 403),
        ("the owner of the link", &member, members, 403),
    ];
    for (case, by, id, status) in cases {
        for seen in [approve(&setup, by, id).await, refuse(&setup, by, id).await] {
            assert_eq!(seen.status, status, "{case}: {}", seen.text);
            if status == 403 {
                assert_eq!(seen.code(), "forbidden", "{case}");
            }
        }
    }
    for id in [admins, members, hidden] {
        assert_eq!(
            setup.stored(id).await.unwrap().status(),
            AccountStatus::Pending
        );
    }
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_APPROVE).await.len(), 0);
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_REFUSE).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn reviewing_needs_a_fresh_step_up_and_changes_nothing_without_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.person(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(member.id, 3, Some(AccountStatus::Pending))
        .await;

    let approved = approve(&setup, &admin, id).await;
    let refused = refuse(&setup, &admin, id).await;
    setup.step_up(&admin).await;
    setup.clock.advance(core::time::Duration::from_secs(301));
    let stale = approve(&setup, &admin, id).await;

    for seen in [approved, refused, stale] {
        assert_eq!(seen.status, 403, "{}", seen.text);
        assert_eq!(seen.code(), "step_up_required");
    }
    assert_eq!(
        setup.stored(id).await.unwrap().status(),
        AccountStatus::Pending
    );
}

#[sqlx::test(migrations = false)]
async fn only_a_pending_link_is_reviewed(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.stepped_up(1, Role::Owner).await;
    let member = setup.person(2, Role::Member).await;
    let statuses = [
        Some(AccountStatus::Active),
        Some(AccountStatus::ReauthRequired),
        Some(AccountStatus::Refused(RefusalReason::NotOwned)),
        None,
    ];
    for (n, status) in (3..).zip(statuses) {
        let id = setup.add_account(member.id, n, status).await;
        let before = setup.stored(id).await;

        let approved = approve(&setup, &owner, id).await;
        let refused = refuse(&setup, &owner, id).await;

        assert_eq!(approved.status, 409, "{status:?}: {}", approved.text);
        assert_eq!(approved.code(), "conflict");
        assert_eq!(refused.status, 409, "{status:?}: {}", refused.text);
        assert_eq!(setup.stored(id).await, before, "{status:?}");
    }
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_APPROVE).await.len(), 0);
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_REFUSE).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn refusing_deletes_the_link_with_its_grants_and_frees_the_slot_and_the_uuid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.stepped_up(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let friend = setup.person(3, Role::Member).await;
    let id = setup
        .add_account(member.id, 4, Some(AccountStatus::Pending))
        .await;
    // Grants can't be made on a pending account, but none must survive it.
    setup
        .add_grant(id, friend.id, GrantLevel::View, member.id)
        .await;
    let bot = setup.add_bot(id, 4).await;

    let refused = refuse(&setup, &admin, id).await;

    assert_eq!(refused.status, 204, "{}", refused.text);
    assert_eq!(refused.text, "");
    assert_eq!(setup.stored(id).await, None);
    assert_eq!(setup.grant_of(id, friend.id).await, None);
    assert_eq!(
        setup.db.accounts().count_owned_by(member.id).await.unwrap(),
        0
    );
    // The Minecraft account can be linked again: its UUID is free.
    let again = setup
        .add_account(friend.id, 4, Some(AccountStatus::Pending))
        .await;
    assert!(matches!(
        setup.stored(again).await.unwrap().credential,
        AccountCredential::Microsoft { .. }
    ));
    let entries = setup.entries(&AuditAction::ACCOUNT_REFUSE).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(admin.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Account(id)))
    );
    assert_eq!(meta(entry, "owner"), Some(text(&member.id.to_string())));
    assert_eq!(meta(entry, "name"), Some(text("AfkBot4")));
    assert_eq!(meta(entry, "uuid"), Some(text(&profile(4))));
    assert_eq!(meta(entry, "bot"), Some(text(&bot.to_string())));
    assert_eq!(setup.db.bots().find(bot).await.unwrap(), None);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_account_is_404_and_a_review_spends_the_users_limit(
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

    let first = approve(&setup, &owner, unknown).await;
    let second = refuse(&setup, &owner, unknown).await;

    assert_eq!(first.status, 404, "{}", first.text);
    assert_eq!(second.status, 429, "{}", second.text);
    assert!(second.retry_after.is_some());
}

#[sqlx::test(migrations = false)]
async fn the_review_re_reads_the_owners_role_in_its_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.person(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(member.id, 3, Some(AccountStatus::Pending))
        .await;
    // The handler checked; the owner is promoted before the service runs.
    setup.set_role(member.id, Role::Admin).await;
    let accounts = service(&setup);

    let approved = accounts.approve(&admin.acting(), id).await;
    let refused = accounts.refuse(&admin.acting(), id).await;

    assert!(
        matches!(approved, Err(ReviewError::Denied(AuthzError::NotFound))),
        "{approved:?}"
    );
    assert!(
        matches!(refused, Err(ReviewError::Denied(AuthzError::NotFound))),
        "{refused:?}"
    );
    assert_eq!(
        setup.stored(id).await.unwrap().status(),
        AccountStatus::Pending
    );
}

#[sqlx::test(migrations = false)]
async fn a_review_whose_session_ended_meanwhile_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.person(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(member.id, 3, Some(AccountStatus::Pending))
        .await;
    setup.set_disabled(admin.id, true).await;
    let accounts = service(&setup);

    let approved = accounts.approve(&admin.acting(), id).await;
    let refused = accounts.refuse(&admin.acting(), id).await;
    let gone = accounts
        .approve(
            &admin.acting(),
            AccountId::new_v7(at(T0), [99; 10]).unwrap(),
        )
        .await;

    assert!(
        matches!(approved, Err(ReviewError::SessionGone)),
        "{approved:?}"
    );
    assert!(
        matches!(refused, Err(ReviewError::SessionGone)),
        "{refused:?}"
    );
    assert!(matches!(gone, Err(ReviewError::SessionGone)), "{gone:?}");
    assert_eq!(
        setup.stored(id).await.unwrap().status(),
        AccountStatus::Pending
    );
}

#[sqlx::test(migrations = false)]
async fn an_account_gone_before_the_transaction_is_not_found(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.person(1, Role::Admin).await;
    let unknown = AccountId::new_v7(at(T0), [99; 10]).unwrap();
    let accounts = service(&setup);

    let approved = accounts.approve(&admin.acting(), unknown).await;
    let refused = accounts.refuse(&admin.acting(), unknown).await;

    assert!(
        matches!(approved, Err(ReviewError::NotFound)),
        "{approved:?}"
    );
    assert!(matches!(refused, Err(ReviewError::NotFound)), "{refused:?}");
}
