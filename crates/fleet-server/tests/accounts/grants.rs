//! Sharing an account (Plan.md P9.1, P9.6, §7.3; ADR-0010, ADR-0018 group
//! E): `GET` and `POST /api/v1/accounts/{id}/grants`, `PUT` and `DELETE
//! /api/v1/accounts/{id}/grants/{user}`, through serve's own app. Manage
//! edits grants, after a step-up; the grantee is named by username, and
//! every name that can't be granted gets the same 422, recorded without the
//! name; creating by name spends its own limit; a pending account takes no
//! grant; a raise for someone with implicit Manage is refused; every write
//! is audited and re-checked in its transaction.

use core::num::NonZeroU32;
use std::sync::Arc;

use axum::http::Method;
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_core::authz::{AuthzError, GrantLevel, Role};
use fleet_core::id::{AccountId, UserId};
use fleet_server::app::grants::{
    ChangeGrantError, CreateGrantError, GrantService, RevokeGrantError,
};
use fleet_server::config::{AccountsConfig, Rate, RateLimitConfig};
use fleet_server::ports::live_hints::LiveHints;
use fleet_testkit::system::ManualClock;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::hints::Heard;
use crate::support::http::{HttpSetup, Person, Seen, meta, text};
use crate::support::service::{T0, at};

async fn create(setup: &HttpSetup, by: &Person, id: AccountId, body: Value) -> Seen {
    setup
        .api(
            Method::POST,
            &format!("/accounts/{id}/grants"),
            by,
            Some(body),
        )
        .await
}

async fn change(setup: &HttpSetup, by: &Person, id: AccountId, user: UserId, level: &str) -> Seen {
    setup
        .api(
            Method::PUT,
            &format!("/accounts/{id}/grants/{user}"),
            by,
            Some(json!({ "level": level })),
        )
        .await
}

async fn revoke(setup: &HttpSetup, by: &Person, id: AccountId, user: UserId) -> Seen {
    setup
        .api(
            Method::DELETE,
            &format!("/accounts/{id}/grants/{user}"),
            by,
            None,
        )
        .await
}

async fn list(setup: &HttpSetup, by: &Person, id: AccountId) -> Seen {
    setup
        .api(Method::GET, &format!("/accounts/{id}/grants"), by, None)
        .await
}

/// `body` with every ID replaced, for a snapshot.
fn redacted(mut body: Value) -> Value {
    for field in ["user", "granted_by"] {
        if let Some(label) = body.get_mut(field) {
            label["id"] = json!(format!("[{field}]"));
        }
    }
    body
}

/// The grant service over `setup`'s database, for the re-checks a request
/// can't reach.
fn service(setup: &HttpSetup) -> GrantService {
    GrantService::new(
        Arc::new(ManualClock::new(at(T0))),
        Arc::new(setup.db.clone()),
    )
}

#[sqlx::test(migrations = false)]
async fn the_owner_shares_an_account_by_name_and_lists_it_audited(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(alice.id, 3, Some(AccountStatus::Active))
        .await;

    let created = create(
        &setup,
        &alice,
        id,
        json!({ "username": "USER2", "level": "control" }),
    )
    .await;
    let listed = list(&setup, &alice, id).await;

    assert_eq!(created.status, 201, "{}", created.text);
    assert_eq!(created.body["user"]["id"], bob.id.to_string());
    insta::assert_json_snapshot!("grant_created", redacted(created.body.clone()));
    assert_eq!(listed.status, 200, "{}", listed.text);
    assert_eq!(listed.body["items"], json!([created.body]));
    assert!(listed.body.get("next").is_none());
    let stored = setup.grant_of(id, bob.id).await.unwrap();
    assert_eq!(stored.level, GrantLevel::Control);
    assert_eq!(stored.granted_by, alice.id);
    let entries = setup.entries(&AuditAction::GRANT_CREATE).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(alice.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Account(id)))
    );
    assert_eq!(meta(entry, "grantee"), Some(text(&bob.id.to_string())));
    assert_eq!(meta(entry, "level"), Some(text("control")));
    assert_eq!(entry.metadata.entries().len(), 2);
}

#[sqlx::test(migrations = false)]
async fn every_name_that_cant_be_granted_gets_the_same_422_recorded_without_the_name(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    setup.add_user(1, Role::Owner).await;
    setup.add_user(2, Role::Admin).await;
    let member = setup.stepped_up(3, Role::Member).await;
    let disabled = setup.add_user(4, Role::Member).await;
    setup.set_disabled(disabled, true).await;
    let id = setup
        .add_account(member.id, 5, Some(AccountStatus::Active))
        .await;

    let cases = [
        ("nobody99", "unknown_user"),
        ("user4", "disabled"),
        ("user3", "implicit_manage"),
        ("user1", "implicit_manage"),
        ("user2", "implicit_manage"),
    ];
    let mut bodies = Vec::new();
    for (name, _) in cases {
        let seen = create(
            &setup,
            &member,
            id,
            json!({ "username": name, "level": "view" }),
        )
        .await;
        assert_eq!(seen.status, 422, "{name}: {}", seen.text);
        assert!(!seen.text.contains(name), "{name}: {}", seen.text);
        bodies.push(seen.body_without_request_id());
    }

    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "{bodies:?}"
    );
    assert_eq!(
        bodies[0]["error"]["fields"],
        json!([{
            "path": "username",
            "message": fleet_api_types::accounts::UNGRANTABLE
        }])
    );
    let entries = setup.entries(&AuditAction::GRANT_CREATE).await;
    assert_eq!(entries.len(), cases.len());
    for (entry, (name, reason)) in entries.iter().zip(cases) {
        assert_eq!(entry.outcome, AuditOutcome::Failure, "{name}");
        assert_eq!(entry.actor, Some(member.id), "{name}");
        assert_eq!(
            entry.target,
            Some(RecordedTarget::Known(AuditTarget::Account(id))),
            "{name}"
        );
        assert_eq!(meta(entry, "reason"), Some(text(reason)), "{name}");
        assert_eq!(entry.metadata.entries().len(), 1, "{name}");
    }
    assert_eq!(list(&setup, &member, id).await.body["items"], json!([]));
}

#[sqlx::test(migrations = false)]
async fn a_name_that_isnt_a_username_is_422_and_a_bad_body_400(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let id = setup.add_account(alice.id, 2, None).await;

    let bad_name = create(
        &setup,
        &alice,
        id,
        json!({ "username": "a b", "level": "view" }),
    )
    .await;
    let unknown_field = create(
        &setup,
        &alice,
        id,
        json!({ "username": "user2", "level": "view", "x": 1 }),
    )
    .await;
    let bad_level = change(&setup, &alice, id, alice.id, "owner").await;

    assert_eq!(bad_name.status, 422, "{}", bad_name.text);
    assert_eq!(bad_name.body["error"]["fields"][0]["path"], "username");
    assert!(!bad_name.text.contains("a b"), "{}", bad_name.text);
    assert_eq!(unknown_field.status, 400, "{}", unknown_field.text);
    assert_eq!(bad_level.status, 400, "{}", bad_level.text);
    assert_eq!(setup.entries(&AuditAction::GRANT_CREATE).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_second_grant_to_the_same_user_and_any_grant_on_a_pending_account_are_409(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let active = setup
        .add_account(alice.id, 3, Some(AccountStatus::Active))
        .await;
    let pending = setup
        .add_account(alice.id, 4, Some(AccountStatus::Pending))
        .await;
    setup
        .add_grant(active, bob.id, GrantLevel::View, alice.id)
        .await;

    let again = create(
        &setup,
        &alice,
        active,
        json!({ "username": "user2", "level": "manage" }),
    )
    .await;
    let on_pending = create(
        &setup,
        &alice,
        pending,
        json!({ "username": "user2", "level": "view" }),
    )
    .await;

    assert_eq!(again.status, 409, "{}", again.text);
    assert_eq!(on_pending.status, 409, "{}", on_pending.text);
    assert_eq!(
        setup.grant_of(active, bob.id).await.unwrap().level,
        GrantLevel::View
    );
    assert_eq!(setup.grant_of(pending, bob.id).await, None);
    assert_eq!(setup.entries(&AuditAction::GRANT_CREATE).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn only_manage_edits_grants_and_an_unseen_account_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let control = setup.stepped_up(2, Role::Member).await;
    let stranger = setup.stepped_up(3, Role::Member).await;
    let bob = setup.person(4, Role::Member).await;
    let id = setup
        .add_account(alice.id, 5, Some(AccountStatus::Active))
        .await;
    setup
        .add_grant(id, control.id, GrantLevel::Control, alice.id)
        .await;
    setup
        .add_grant(id, bob.id, GrantLevel::View, alice.id)
        .await;

    for (who, by, status) in [
        ("a control grantee", &control, 403),
        ("a stranger", &stranger, 404),
    ] {
        let answers = [
            list(&setup, by, id).await,
            create(
                &setup,
                by,
                id,
                json!({ "username": "user4", "level": "view" }),
            )
            .await,
            change(&setup, by, id, bob.id, "control").await,
            revoke(&setup, by, id, bob.id).await,
        ];
        for seen in answers {
            assert_eq!(seen.status, status, "{who}: {}", seen.text);
        }
    }
    assert_eq!(
        setup.grant_of(id, bob.id).await.unwrap().level,
        GrantLevel::View
    );
}

#[sqlx::test(migrations = false)]
async fn a_manage_grantee_shares_further_but_never_with_themselves(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let manager = setup.stepped_up(2, Role::Member).await;
    setup.add_user(3, Role::Member).await;
    let id = setup
        .add_account(alice.id, 4, Some(AccountStatus::Active))
        .await;
    setup
        .add_grant(id, manager.id, GrantLevel::Manage, alice.id)
        .await;

    let to_bob = create(
        &setup,
        &manager,
        id,
        json!({ "username": "user3", "level": "manage" }),
    )
    .await;
    let lower_self = change(&setup, &manager, id, manager.id, "view").await;

    assert_eq!(to_bob.status, 201, "{}", to_bob.text);
    assert_eq!(to_bob.body["granted_by"]["username"], "user2");
    assert_eq!(lower_self.status, 403, "{}", lower_self.text);
    assert_eq!(lower_self.code(), "forbidden");
    assert_eq!(
        setup.grant_of(id, manager.id).await.unwrap().level,
        GrantLevel::Manage
    );
}

#[sqlx::test(migrations = false)]
async fn changing_a_grant_rewrites_it_and_records_both_levels(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    setup
        .add_grant(id, bob.id, GrantLevel::View, alice.id)
        .await;
    setup.clock.advance(core::time::Duration::from_mins(1));

    let changed = change(&setup, &alice, id, bob.id, "manage").await;
    let same = change(&setup, &alice, id, bob.id, "manage").await;

    assert_eq!(changed.status, 200, "{}", changed.text);
    assert_eq!(changed.body["level"], "manage");
    assert_eq!(changed.body["granted_at"], "2027-01-15T08:01:00.000Z");
    assert_eq!(same.status, 200, "{}", same.text);
    assert_eq!(same.body, changed.body);
    let entries = setup.entries(&AuditAction::GRANT_SET_LEVEL).await;
    assert_eq!(entries.len(), 1, "the same level changes nothing");
    let entry = &entries[0];
    assert_eq!(entry.actor, Some(alice.id));
    assert_eq!(meta(entry, "grantee"), Some(text(&bob.id.to_string())));
    assert_eq!(meta(entry, "level"), Some(text("manage")));
    assert_eq!(meta(entry, "previous_level"), Some(text("view")));
}

#[sqlx::test(migrations = false)]
async fn a_raise_for_someone_with_implicit_manage_is_409_and_lowering_works(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    setup
        .add_grant(id, bob.id, GrantLevel::Control, alice.id)
        .await;
    // Promoted, Bob has implicit Manage on a Member's account.
    setup.set_role(bob.id, Role::Admin).await;

    let raised = change(&setup, &alice, id, bob.id, "manage").await;
    let lowered = change(&setup, &alice, id, bob.id, "view").await;

    assert_eq!(raised.status, 409, "{}", raised.text);
    assert_eq!(lowered.status, 200, "{}", lowered.text);
    assert_eq!(
        setup.grant_of(id, bob.id).await.unwrap().level,
        GrantLevel::View
    );
}

#[sqlx::test(migrations = false)]
async fn revoking_removes_the_grant_audited_and_a_missing_grant_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(alice.id, 3, Some(AccountStatus::Pending))
        .await;
    setup
        .add_grant(id, bob.id, GrantLevel::View, alice.id)
        .await;

    let revoked = revoke(&setup, &alice, id, bob.id).await;
    let again = revoke(&setup, &alice, id, bob.id).await;
    let change_missing = change(&setup, &alice, id, bob.id, "view").await;

    assert_eq!(revoked.status, 204, "{}", revoked.text);
    assert_eq!(setup.grant_of(id, bob.id).await, None);
    assert_eq!(again.status, 404, "{}", again.text);
    assert_eq!(change_missing.status, 404, "{}", change_missing.text);
    let entries = setup.entries(&AuditAction::GRANT_REVOKE).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        meta(&entries[0], "grantee"),
        Some(text(&bob.id.to_string()))
    );
    assert_eq!(meta(&entries[0], "level"), Some(text("view")));
}

#[sqlx::test(migrations = false)]
async fn every_grant_write_needs_a_fresh_step_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    setup
        .add_grant(id, bob.id, GrantLevel::View, alice.id)
        .await;

    let answers = [
        create(
            &setup,
            &alice,
            id,
            json!({ "username": "user2", "level": "view" }),
        )
        .await,
        change(&setup, &alice, id, bob.id, "manage").await,
        revoke(&setup, &alice, id, bob.id).await,
    ];

    for seen in answers {
        assert_eq!(seen.status, 403, "{}", seen.text);
        assert_eq!(seen.code(), "step_up_required");
    }
    assert_eq!(
        setup.grant_of(id, bob.id).await.unwrap().level,
        GrantLevel::View
    );
    assert_eq!(list(&setup, &alice, id).await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn granting_by_name_spends_its_own_per_user_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let limits = RateLimitConfig {
        grants_by_name: Rate::per_hour(NonZeroU32::new(2).unwrap()),
        ..RateLimitConfig::default()
    };
    let setup = HttpSetup::with(options, &limits, AccountsConfig::default()).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let carol = setup.stepped_up(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    let carols = setup.add_account(carol.id, 4, None).await;

    let first = probe(&setup, &alice, id).await;
    let second = probe(&setup, &alice, id).await;
    let third = probe(&setup, &alice, id).await;
    let carols_first = probe(&setup, &carol, carols).await;

    assert_eq!(first.status, 422, "{}", first.text);
    assert_eq!(second.status, 422, "{}", second.text);
    assert_eq!(third.status, 429, "{}", third.text);
    assert_eq!(third.retry_after.as_deref(), Some("1800"));
    assert_eq!(
        carols_first.status, 422,
        "another user's names are their own"
    );
    assert_eq!(
        list(&setup, &alice, id).await.status,
        200,
        "other routes still answer"
    );
    setup.clock.advance(core::time::Duration::from_mins(30));
    // Half an hour later the access token is gone too: a new session.
    let later = setup.session_of(alice.id, Role::Member).await;
    setup.step_up(&later).await;
    assert_eq!(probe(&setup, &later, id).await.status, 422);
}

/// A grant to a name nobody has, as `by`.
async fn probe(setup: &HttpSetup, by: &Person, id: AccountId) -> Seen {
    create(
        setup,
        by,
        id,
        json!({ "username": "nobody", "level": "view" }),
    )
    .await
}

#[sqlx::test(migrations = false)]
async fn the_lists_page_and_spend_the_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let one = Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN);
    let limits = RateLimitConfig {
        per_user: one,
        ..RateLimitConfig::default()
    };
    let setup = HttpSetup::with(options, &limits, AccountsConfig::default()).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup.add_account(alice.id, 2, None).await;
    for n in 3..=5 {
        let user = setup.add_user(n, Role::Member).await;
        setup.add_grant(id, user, GrantLevel::View, alice.id).await;
    }

    let first = setup
        .api(
            Method::GET,
            &format!("/accounts/{id}/grants?limit=2"),
            &alice,
            None,
        )
        .await;
    let limited = list(&setup, &alice, id).await;
    setup.clock.advance(core::time::Duration::from_secs(1));
    let next = first.body["next"].as_str().unwrap();
    let rest = setup
        .api(
            Method::GET,
            &format!("/accounts/{id}/grants?limit=2&before={next}"),
            &alice,
            None,
        )
        .await;
    setup.clock.advance(core::time::Duration::from_secs(1));
    let bad = setup
        .api(
            Method::GET,
            &format!("/accounts/{id}/grants?limit=0"),
            &alice,
            None,
        )
        .await;

    assert_eq!(first.status, 200, "{}", first.text);
    let names = |seen: &Seen| {
        seen.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["user"]["username"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&first), ["user5", "user4"]);
    assert_eq!(limited.status, 429, "{}", limited.text);
    assert_eq!(names(&rest), ["user3"]);
    assert_eq!(bad.status, 422, "{}", bad.text);
}

#[sqlx::test(migrations = false)]
async fn the_writes_re_check_the_actor_and_the_grantee_in_their_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let manager = setup.person(2, Role::Member).await;
    let bob = setup.person(3, Role::Member).await;
    let gone = setup.person(4, Role::Member).await;
    let id = setup.add_account(alice.id, 5, None).await;
    setup
        .add_grant(id, bob.id, GrantLevel::View, alice.id)
        .await;
    setup.set_disabled(gone.id, true).await;
    let grants = service(&setup);
    let name = |text: &str| fleet_core::value::Username::try_from(text).unwrap();
    let unknown = AccountId::new_v7(at(T0), [99; 10]).unwrap();

    // The handler saw a Manage grant the manager no longer holds.
    let created = grants
        .create_by_name(&manager.acting(), id, &name("user3"), GrantLevel::View)
        .await;
    let changed = grants
        .set_level(&manager.acting(), id, bob.id, GrantLevel::Control)
        .await;
    let revoked = grants.revoke(&manager.acting(), id, bob.id).await;
    // A session gone, and an account gone.
    let without_session = grants
        .create_by_name(&gone.acting(), id, &name("user3"), GrantLevel::View)
        .await;
    let changed_gone = grants
        .set_level(&gone.acting(), id, bob.id, GrantLevel::Control)
        .await;
    let revoked_gone = grants.revoke(&gone.acting(), id, bob.id).await;
    let no_account = grants
        .create_by_name(&alice.acting(), unknown, &name("user3"), GrantLevel::View)
        .await;
    let change_no_account = grants
        .set_level(&alice.acting(), unknown, bob.id, GrantLevel::View)
        .await;
    let revoke_no_account = grants.revoke(&alice.acting(), unknown, bob.id).await;

    assert!(
        matches!(created, Err(CreateGrantError::Denied(AuthzError::NotFound))),
        "{created:?}"
    );
    assert!(
        matches!(changed, Err(ChangeGrantError::Denied(AuthzError::NotFound))),
        "{changed:?}"
    );
    assert!(
        matches!(revoked, Err(RevokeGrantError::Denied(AuthzError::NotFound))),
        "{revoked:?}"
    );
    assert!(matches!(
        without_session,
        Err(CreateGrantError::SessionGone)
    ));
    assert!(matches!(changed_gone, Err(ChangeGrantError::SessionGone)));
    assert!(matches!(revoked_gone, Err(RevokeGrantError::SessionGone)));
    assert!(matches!(no_account, Err(CreateGrantError::NotFound)));
    assert!(matches!(change_no_account, Err(ChangeGrantError::NotFound)));
    assert!(matches!(revoke_no_account, Err(RevokeGrantError::NotFound)));
    assert_eq!(
        setup.grant_of(id, bob.id).await.unwrap().level,
        GrantLevel::View
    );
}

#[sqlx::test(migrations = false)]
async fn each_grant_write_tells_the_sockets_its_bot_after_its_commit_and_a_refused_one_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    let bot = setup.add_bot(id, 3).await;
    let heard = Arc::new(Heard::new(&setup.db));
    let grants = service(&setup).with_live_hints(Arc::clone(&heard) as Arc<dyn LiveHints>);
    let bob_name = fleet_core::value::Username::try_from("user2").unwrap();

    let mut steps = Vec::new();
    grants
        .create_by_name(&alice.acting(), id, &bob_name, GrantLevel::View)
        .await
        .unwrap();
    steps.push(("created", heard.take(), heard.once(&[bot])));
    let again = grants
        .create_by_name(&alice.acting(), id, &bob_name, GrantLevel::View)
        .await;
    steps.push(("created again", heard.take(), Vec::new()));
    grants
        .set_level(&alice.acting(), id, bob.id, GrantLevel::Control)
        .await
        .unwrap();
    steps.push(("changed", heard.take(), heard.once(&[bot])));
    grants.revoke(&alice.acting(), id, bob.id).await.unwrap();
    steps.push(("revoked", heard.take(), heard.once(&[bot])));
    let revoked_again = grants.revoke(&alice.acting(), id, bob.id).await;
    steps.push(("revoked again", heard.take(), Vec::new()));

    assert!(matches!(again, Err(CreateGrantError::Exists)), "{again:?}");
    assert!(
        matches!(revoked_again, Err(RevokeGrantError::NoGrant)),
        "{revoked_again:?}"
    );
    for (name, heard, expected) in steps {
        assert_eq!(heard, expected, "{name}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_raise_on_a_pending_account_is_refused_in_the_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // No grant can be made on a pending account, but one written before
    // (directly here) can't be raised either.
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(alice.id, 3, Some(AccountStatus::Pending))
        .await;
    setup
        .add_grant(id, bob.id, GrantLevel::View, alice.id)
        .await;

    let raised = service(&setup)
        .set_level(&alice.acting(), id, bob.id, GrantLevel::Manage)
        .await;

    assert!(
        matches!(raised, Err(ChangeGrantError::Pending)),
        "{raised:?}"
    );
    assert_eq!(
        setup.grant_of(id, bob.id).await.unwrap().level,
        GrantLevel::View
    );
}
