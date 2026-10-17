//! `GET /users` and `PATCH /users/{id}` (Plan.md P7.9, P7.12, ADR-0010,
//! ADR-0016): the admins' list and the one change they make, P7.12's
//! escalation and disabling over HTTP, and the re-check of a change in its
//! own transaction.

use core::num::NonZeroU32;

use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use fleet_core::authz::Actor;
use fleet_server::app::users::{ActingUser, UserChange, UserService};
use fleet_server::config::Argon2Params;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::ports::reconcile::{Change, Reconcile};

use super::support::{
    INVITE_TTL, LOGOUT, Meanwhile, Options, PASSWORD, Person, Setup, T0, USERS, redacted, setup,
    setup_with,
};

/// The fleet every test starts with: the Owner, two Admins and two
/// Members, Alice among them, each with a live session.
struct Cast {
    owner: Person,
    admin: Person,
    other_admin: Person,
    alice: Person,
    bob: Person,
}

/// The cast, none of them stepped up.
async fn unstamped_cast(setup: &Setup) -> Cast {
    Cast {
        owner: setup.person(2, "owner", Role::Owner).await,
        admin: setup.person(3, "admin", Role::Admin).await,
        other_admin: setup.person(4, "other-admin", Role::Admin).await,
        alice: setup.alice_person().await,
        bob: setup.person(5, "bob", Role::Member).await,
    }
}

/// The cast, every session stepped up now: the writes are step-up routes,
/// so a test of what `authorize()` decides starts past the step-up check.
/// Each test's later clock moves stay well inside the 5 minutes.
async fn cast(setup: &Setup) -> Cast {
    let cast = unstamped_cast(setup).await;
    for person in [
        &cast.owner,
        &cast.admin,
        &cast.other_admin,
        &cast.alice,
        &cast.bob,
    ] {
        setup.stamp(person.session).await;
    }
    cast
}

/// The cast's IDs by name, for redacting a snapshot.
fn names(cast: &Cast) -> Vec<(String, &'static str)> {
    vec![
        (cast.owner.id.to_string(), "owner"),
        (cast.admin.id.to_string(), "admin"),
        (cast.other_admin.id.to_string(), "other_admin"),
        (cast.alice.id.to_string(), "alice"),
        (cast.bob.id.to_string(), "bob"),
    ]
}

fn user_path(id: UserId) -> String {
    format!("{USERS}/{id}")
}

/// The role a user has now.
async fn role(setup: &Setup, id: UserId) -> Role {
    setup.db.users().get(id).await.unwrap().unwrap().role
}

/// Whether a user is disabled now.
async fn disabled(setup: &Setup, id: UserId) -> bool {
    setup.db.users().get(id).await.unwrap().unwrap().disabled
}

/// How many entries of `action` the log holds.
async fn entries(setup: &Setup, action: &AuditAction) -> usize {
    setup
        .audit()
        .await
        .iter()
        .filter(|entry| &entry.action == action)
        .count()
}

fn metadata(build: &AuditMetadata) -> RecordedMetadata {
    RecordedMetadata::from(build)
}

/// A server whose per-user limit allows one request.
async fn one_request_each(options: SqliteConnectOptions) -> Setup {
    setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await
}

/// The hints a service sent.
#[derive(Debug, Default)]
struct Hinted(Mutex<Vec<Change>>);

impl Reconcile for Hinted {
    fn changed(&self, change: Change) {
        self.0.lock().unwrap().push(change);
    }
}

impl Hinted {
    fn take(&self) -> Vec<Change> {
        core::mem::take(&mut *self.0.lock().unwrap())
    }
}

// --- The list ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_admin_lists_every_user_newest_first_with_their_facts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    setup.enroll_alice().await;

    let seen = setup.get(USERS, Some(&cast.admin.token)).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(seen.body["items"].as_array().unwrap().len(), 5);
    assert!(seen.body.get("next").is_none());
    insta::assert_json_snapshot!("users_page", redacted(&seen.body, &names(&cast)));
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_with_its_cursor(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let ids = |seen: &Value| -> Vec<String> {
        seen["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    };

    let first = setup
        .get(&format!("{USERS}?limit=2"), Some(&cast.owner.token))
        .await;
    let next = first.body["next"].as_str().unwrap().to_owned();
    let second = setup
        .get(
            &format!("{USERS}?before={next}&limit=2"),
            Some(&cast.owner.token),
        )
        .await;
    let next = second.body["next"].as_str().unwrap().to_owned();
    let last = setup
        .get(
            &format!("{USERS}?before={next}&limit=2"),
            Some(&cast.owner.token),
        )
        .await;

    // Alice's ID is minted at T0, the others' a minute before.
    assert_eq!(
        ids(&first.body),
        [cast.alice.id.to_string(), cast.bob.id.to_string()]
    );
    assert_eq!(
        ids(&second.body),
        [cast.other_admin.id.to_string(), cast.admin.id.to_string()]
    );
    assert_eq!(ids(&last.body), [cast.owner.id.to_string()]);
    assert!(last.body.get("next").is_none());
}

#[sqlx::test(migrations = false)]
async fn a_member_cant_list_users(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup.get(USERS, Some(&cast.alice.token)).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
}

#[sqlx::test(migrations = false)]
async fn a_bad_query_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let out_of_range = setup
        .get(&format!("{USERS}?limit=0"), Some(&cast.admin.token))
        .await;
    let not_a_number = setup
        .get(&format!("{USERS}?limit=ten"), Some(&cast.admin.token))
        .await;
    let not_v7 = setup
        .get(
            &format!("{USERS}?before=550e8400-e29b-41d4-a716-446655440000"),
            Some(&cast.admin.token),
        )
        .await;

    assert_eq!(out_of_range.status, 422, "{}", out_of_range.text);
    assert_eq!(out_of_range.body["error"]["fields"][0]["path"], "limit");
    assert_eq!(not_a_number.status, 400, "{}", not_a_number.text);
    assert_eq!(not_v7.status, 400, "{}", not_v7.text);
}

#[sqlx::test(migrations = false)]
async fn the_list_spends_the_users_limit(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = one_request_each(options).await;
    let cast = cast(&setup).await;
    assert_eq!(setup.get(USERS, Some(&cast.admin.token)).await.status, 200);

    let seen = setup.get(USERS, Some(&cast.admin.token)).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
}

// --- Roles -------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_owner_promotes_a_member_whose_sessions_all_end(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let (second_session, second_token) = setup.session().await;

    let seen = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.owner.token),
            json!({ "role": "admin" }),
        )
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    insta::assert_json_snapshot!("user_patched", redacted(&seen.body, &names(&cast)));
    assert_eq!(role(&setup, cast.alice.id).await, Role::Admin);
    assert!(!setup.live(cast.alice.session).await);
    assert!(!setup.live(second_session).await);
    assert_eq!(
        setup.post(LOGOUT, Some(&second_token), None).await.status,
        401
    );
    assert!(setup.live(cast.bob.session).await);
    let entry = setup.last(&AuditAction::USER_SET_ROLE).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(cast.owner.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(cast.alice.id)))
    );
    assert_eq!(entry.ip, Some("192.0.2.10".parse().unwrap()));
    assert_eq!(
        entry.metadata,
        metadata(
            &AuditMetadata::new()
                .text("role", "admin")
                .unwrap()
                .text("previous_role", "member")
                .unwrap()
                .int("sessions_revoked", 2)
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn the_owner_demotes_an_admin_whose_pending_invites_are_revoked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let first = setup.add_invite(10, cast.admin.id, Role::Member, 0).await;
    let second = setup
        .add_invite(11, cast.admin.id, Role::Member, 1_000)
        .await;
    let used = setup.add_invite(12, cast.admin.id, Role::Member, 0).await;
    let expired = setup
        .add_invite(13, cast.admin.id, Role::Member, -INVITE_TTL)
        .await;
    let others = setup.add_invite(14, cast.owner.id, Role::Admin, 0).await;
    setup.use_invite(used, cast.bob.id).await;

    let seen = setup
        .patch(
            &user_path(cast.admin.id),
            Some(&cast.owner.token),
            json!({ "role": "member" }),
        )
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(seen.body["role"], "member");
    let revoked: Vec<_> = setup
        .audit()
        .await
        .into_iter()
        .filter(|entry| entry.action == AuditAction::INVITE_REVOKE)
        .collect();
    assert_eq!(revoked.len(), 2);
    for invite in [first, second] {
        let entry = revoked
            .iter()
            .find(|entry| entry.target == Some(RecordedTarget::Known(AuditTarget::Invite(invite))))
            .unwrap();
        assert_eq!(entry.actor, Some(cast.owner.id));
        assert_eq!(
            entry.metadata,
            metadata(
                &AuditMetadata::new()
                    .text("role", "member")
                    .unwrap()
                    .text("reason", "creator_demoted")
                    .unwrap()
            )
        );
    }
    for (invite, revoked) in [
        (first, true),
        (second, true),
        (used, false),
        (expired, false),
        (others, false),
    ] {
        let stored = setup.db.invites().find(invite).await.unwrap().unwrap();
        assert_eq!(stored.revoked_at.is_some(), revoked, "{invite}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_member_cant_make_themselves_an_admin(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.alice.token),
            json!({ "role": "admin" }),
        )
        .await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(role(&setup, cast.alice.id).await, Role::Member);
    assert!(setup.live(cast.alice.session).await);
    assert_eq!(entries(&setup, &AuditAction::USER_SET_ROLE).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_member_gets_404_for_any_other_user(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let unknown = UserId::new_v7(super::support::at(T0), [99; 10]).unwrap();

    let other = setup
        .patch(
            &user_path(cast.bob.id),
            Some(&cast.alice.token),
            json!({ "disabled": true }),
        )
        .await;
    let nobody = setup
        .patch(
            &user_path(unknown),
            Some(&cast.alice.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(other.status, 404, "{}", other.text);
    assert_eq!(
        other.body_without_request_id(),
        nobody.body_without_request_id()
    );
    assert!(!disabled(&setup, cast.bob.id).await);
}

#[sqlx::test(migrations = false)]
async fn an_admin_cant_change_any_role(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let promote = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.admin.token),
            json!({ "role": "admin" }),
        )
        .await;
    let unchanged = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.admin.token),
            json!({ "role": "member" }),
        )
        .await;
    let themselves = setup
        .patch(
            &user_path(cast.admin.id),
            Some(&cast.admin.token),
            json!({ "role": "member" }),
        )
        .await;

    for seen in [promote, unchanged, themselves] {
        assert_eq!(seen.status, 403, "{}", seen.text);
        assert_eq!(seen.code(), "forbidden");
    }
    assert_eq!(role(&setup, cast.alice.id).await, Role::Member);
    assert_eq!(role(&setup, cast.admin.id).await, Role::Admin);
}

#[sqlx::test(migrations = false)]
async fn an_admin_cant_change_the_owner(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let disable = setup
        .patch(
            &user_path(cast.owner.id),
            Some(&cast.admin.token),
            json!({ "disabled": true }),
        )
        .await;
    let demote = setup
        .patch(
            &user_path(cast.owner.id),
            Some(&cast.admin.token),
            json!({ "role": "member" }),
        )
        .await;

    assert_eq!(disable.status, 403, "{}", disable.text);
    assert_eq!(disable.code(), "forbidden");
    assert_eq!(demote.status, 403, "{}", demote.text);
    assert_eq!(demote.code(), "forbidden");
    assert!(!disabled(&setup, cast.owner.id).await);
    assert_eq!(role(&setup, cast.owner.id).await, Role::Owner);
    assert!(setup.live(cast.owner.session).await);
}

#[sqlx::test(migrations = false)]
async fn an_admin_cant_disable_another_admin(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup
        .patch(
            &user_path(cast.other_admin.id),
            Some(&cast.admin.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert!(!disabled(&setup, cast.other_admin.id).await);
}

#[sqlx::test(migrations = false)]
async fn nobody_makes_another_owner(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup
        .patch(
            &user_path(cast.admin.id),
            Some(&cast.owner.token),
            json!({ "role": "owner" }),
        )
        .await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(role(&setup, cast.admin.id).await, Role::Admin);
}

#[sqlx::test(migrations = false)]
async fn the_owner_cant_change_themselves(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let disable = setup
        .patch(
            &user_path(cast.owner.id),
            Some(&cast.owner.token),
            json!({ "disabled": true }),
        )
        .await;
    let demote = setup
        .patch(
            &user_path(cast.owner.id),
            Some(&cast.owner.token),
            json!({ "role": "admin" }),
        )
        .await;

    assert_eq!(disable.status, 403, "{}", disable.text);
    assert_eq!(disable.code(), "forbidden");
    assert_eq!(demote.status, 403, "{}", demote.text);
    assert_eq!(demote.code(), "forbidden");
    assert_eq!(role(&setup, cast.owner.id).await, Role::Owner);
}

// --- Disabling ---------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_admin_disables_a_member_who_is_refused_from_then_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.admin.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(seen.body["disabled"], true);
    assert!(disabled(&setup, cast.alice.id).await);
    assert!(!setup.live(cast.alice.session).await);
    let next = setup.post(LOGOUT, Some(&cast.alice.token), None).await;
    assert_eq!(next.status, 401, "{}", next.text);
    let login = setup.login("alice", PASSWORD).await;
    assert_eq!(login.status, 401, "{}", login.text);
    assert_eq!(login.code(), "invalid_credentials");
    let entry = setup.last(&AuditAction::USER_SET_DISABLED).await;
    assert_eq!(entry.actor, Some(cast.admin.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(cast.alice.id)))
    );
    assert_eq!(
        entry.metadata,
        metadata(
            &AuditMetadata::new()
                .flag("disabled", true)
                .unwrap()
                .int("sessions_revoked", 1)
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn disabling_an_admin_revokes_their_pending_invites(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let pending = setup.add_invite(10, cast.admin.id, Role::Member, 0).await;

    let seen = setup
        .patch(
            &user_path(cast.admin.id),
            Some(&cast.owner.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let entry = setup.last(&AuditAction::INVITE_REVOKE).await;
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Invite(pending)))
    );
    assert_eq!(
        entry.metadata,
        metadata(
            &AuditMetadata::new()
                .text("role", "member")
                .unwrap()
                .text("reason", "creator_disabled")
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn disabling_or_enabling_a_user_hints_the_reconciler_and_nothing_else_does(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let hinted = Arc::new(Hinted::default());
    let passwords = Argon2Passwords::new(Argon2Params::default(), Arc::new(setup.random.clone()))
        .await
        .unwrap();
    let users = UserService::new(
        Arc::new(setup.clock.clone()),
        Arc::new(setup.random.clone()),
        Arc::new(setup.db.clone()),
        Arc::new(passwords),
    )
    .with_reconcile(Arc::clone(&hinted) as Arc<dyn Reconcile>);
    let owner = ActingUser {
        actor: Actor {
            id: cast.owner.id,
            role: Role::Owner,
        },
        session_id: cast.owner.session,
        ip: IpAddr::from([192, 0, 2, 1]),
    };

    users
        .set(&owner, cast.bob.id, UserChange::Disabled(true))
        .await
        .unwrap();
    let disabled = hinted.take();
    users
        .set(&owner, cast.bob.id, UserChange::Disabled(true))
        .await
        .unwrap();
    users
        .set(&owner, cast.alice.id, UserChange::Role(Role::Admin))
        .await
        .unwrap();
    let unchanged_and_role = hinted.take();
    users
        .set(&owner, cast.bob.id, UserChange::Disabled(false))
        .await
        .unwrap();

    assert_eq!(disabled, [Change::Store]);
    assert_eq!(unchanged_and_role, []);
    assert_eq!(hinted.take(), [Change::Store]);
}

#[sqlx::test(migrations = false)]
async fn an_enabled_user_logs_in_again(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let path = user_path(cast.alice.id);
    let off = setup
        .patch(&path, Some(&cast.admin.token), json!({ "disabled": true }))
        .await;
    assert_eq!(off.status, 200, "{}", off.text);

    let on = setup
        .patch(&path, Some(&cast.admin.token), json!({ "disabled": false }))
        .await;

    assert_eq!(on.status, 200, "{}", on.text);
    assert_eq!(on.body["disabled"], false);
    assert_eq!(setup.login("alice", PASSWORD).await.status, 200);
    assert_eq!(
        setup.last(&AuditAction::USER_SET_DISABLED).await.metadata,
        metadata(
            &AuditMetadata::new()
                .flag("disabled", false)
                .unwrap()
                .int("sessions_revoked", 0)
                .unwrap()
        )
    );
    assert!(!setup.live(cast.alice.session).await);
}

#[sqlx::test(migrations = false)]
async fn an_unchanged_value_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let path = user_path(cast.alice.id);

    let same_role = setup
        .patch(&path, Some(&cast.owner.token), json!({ "role": "member" }))
        .await;
    let same_state = setup
        .patch(&path, Some(&cast.admin.token), json!({ "disabled": false }))
        .await;

    assert_eq!(same_role.status, 200, "{}", same_role.text);
    assert_eq!(same_state.status, 200, "{}", same_state.text);
    assert!(setup.live(cast.alice.session).await);
    assert_eq!(entries(&setup, &AuditAction::USER_SET_ROLE).await, 0);
    assert_eq!(entries(&setup, &AuditAction::USER_SET_DISABLED).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_patch_holds_exactly_one_change(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let path = user_path(cast.alice.id);

    let none = setup.patch(&path, Some(&cast.owner.token), json!({})).await;
    let both = setup
        .patch(
            &path,
            Some(&cast.owner.token),
            json!({ "role": "admin", "disabled": true }),
        )
        .await;
    let unknown_field = setup
        .patch(
            &path,
            Some(&cast.owner.token),
            json!({ "disabled": true, "name": "x" }),
        )
        .await;
    let unknown_role = setup
        .patch(&path, Some(&cast.owner.token), json!({ "role": "root" }))
        .await;

    for seen in [&none, &both] {
        assert_eq!(seen.status, 422, "{}", seen.text);
        assert_eq!(
            seen.body["error"]["fields"],
            json!([{ "path": "", "message": "must hold exactly one of `role` and `disabled`" }])
        );
    }
    assert_eq!(unknown_field.status, 400, "{}", unknown_field.text);
    assert_eq!(unknown_role.status, 400, "{}", unknown_role.text);
    assert_eq!(role(&setup, cast.alice.id).await, Role::Member);
}

#[sqlx::test(migrations = false)]
async fn a_malformed_id_is_not_found(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup
        .patch(
            &format!("{USERS}/not-an-id"),
            Some(&cast.owner.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(seen.status, 404, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn a_change_spends_the_users_limit(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = one_request_each(options).await;
    let cast = cast(&setup).await;
    let path = user_path(cast.alice.id);
    let first = setup
        .patch(&path, Some(&cast.owner.token), json!({ "disabled": false }))
        .await;
    assert_eq!(first.status, 200, "{}", first.text);

    let seen = setup
        .patch(&path, Some(&cast.owner.token), json!({ "disabled": true }))
        .await;

    assert_eq!(seen.status, 429, "{}", seen.text);
    assert!(!disabled(&setup, cast.alice.id).await);
}

// --- The step-up -------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_user_change_without_a_fresh_step_up_is_refused_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = unstamped_cast(&setup).await;

    let promoting = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.owner.token),
            json!({ "role": "admin" }),
        )
        .await;
    let disabling = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.admin.token),
            json!({ "disabled": true }),
        )
        .await;

    for seen in [&promoting, &disabling] {
        assert_eq!(seen.status, 403, "{}", seen.text);
        assert_eq!(seen.code(), "step_up_required");
    }
    assert_eq!(role(&setup, cast.alice.id).await, Role::Member);
    assert!(!disabled(&setup, cast.alice.id).await);
    assert!(setup.live(cast.alice.session).await);
    assert_eq!(entries(&setup, &AuditAction::USER_SET_ROLE).await, 0);
    assert_eq!(entries(&setup, &AuditAction::USER_SET_DISABLED).await, 0);
}

// --- The re-check in the transaction --------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_admin_demoted_meanwhile_cant_disable_a_member(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    setup
        .hooked
        .queue(Meanwhile::SetRole(cast.admin.id, Role::Member));

    let seen = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.admin.token),
            json!({ "disabled": true }),
        )
        .await;

    // authorize() on the fresh role: a Member can't even see another user.
    assert_eq!(seen.status, 404, "{}", seen.text);
    assert!(!disabled(&setup, cast.alice.id).await);
    assert!(setup.live(cast.alice.session).await);
    assert_eq!(entries(&setup, &AuditAction::USER_SET_DISABLED).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_member_promoted_meanwhile_cant_be_disabled_by_an_admin(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    setup
        .hooked
        .queue(Meanwhile::SetRole(cast.alice.id, Role::Admin));

    let seen = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.admin.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert!(!disabled(&setup, cast.alice.id).await);
}

#[sqlx::test(migrations = false)]
async fn an_actor_disabled_or_logged_out_meanwhile_gets_401(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let path = user_path(cast.alice.id);
    setup.hooked.queue(Meanwhile::DisableUser(cast.admin.id));
    let disabled_actor = setup
        .patch(&path, Some(&cast.admin.token), json!({ "disabled": true }))
        .await;
    setup
        .hooked
        .queue(Meanwhile::RevokeSession(cast.owner.session));
    let revoked_session = setup
        .patch(&path, Some(&cast.owner.token), json!({ "disabled": true }))
        .await;

    assert_eq!(disabled_actor.status, 401, "{}", disabled_actor.text);
    assert_eq!(revoked_session.status, 401, "{}", revoked_session.text);
    assert!(!disabled(&setup, cast.alice.id).await);
}

#[sqlx::test(migrations = false)]
async fn whether_a_value_changes_is_decided_on_the_fresh_row(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    setup.hooked.queue(Meanwhile::DisableUser(cast.alice.id));

    let seen = setup
        .patch(
            &user_path(cast.alice.id),
            Some(&cast.owner.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(seen.body["disabled"], true);
    assert_eq!(entries(&setup, &AuditAction::USER_SET_DISABLED).await, 0);
    // Only a change revokes: the raw disable above left the session alone.
    assert!(setup.live(cast.alice.session).await);
}
