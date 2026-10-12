//! `POST /auth/register` (Plan.md P7.6, P7.1, P7.12, §7.2, ADR-0016):
//! an invite registers one user, who logs in next and sets up 2FA there;
//! P7.12's invite reuse and expiry over HTTP; every invite it can't use is
//! one 422, and a real one is recorded; the checks run cheapest first, and
//! the transaction decides a race.

use core::num::NonZeroU32;

use fleet_api_types::auth::INVALID_INVITE;
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::authz::Role;
use fleet_core::id::{InviteId, UserId};
use fleet_core::value::Username;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use secrecy::ExposeSecret as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{
    INVITE_TTL, Meanwhile, Options, PASSWORD, REGISTER, Seen, Setup, T0, at, redacted, setup,
    setup_with,
};

/// A strong password for the new user.
const NEW_PASSWORD: &str = "violet staircase humming quietly";

async fn register(setup: &Setup, code: &str, username: &str, password: &str) -> Seen {
    setup
        .post(
            REGISTER,
            None,
            Some(json!({ "invite": code, "username": username, "password": password })),
        )
        .await
}

/// The Owner, who invites.
async fn owner(setup: &Setup) -> UserId {
    setup.add_user(2, "owner", Role::Owner).await
}

async fn user_named(setup: &Setup, name: &str) -> Option<UserId> {
    setup
        .db
        .users()
        .find_by_username(&Username::try_from(name).unwrap())
        .await
        .unwrap()
        .map(|user| user.id)
}

async fn entries(setup: &Setup, action: &AuditAction, outcome: AuditOutcome) -> usize {
    setup
        .audit()
        .await
        .iter()
        .filter(|entry| &entry.action == action && entry.outcome == outcome)
        .count()
}

async fn used_by(setup: &Setup, invite: InviteId) -> Option<UserId> {
    setup
        .db
        .invites()
        .find(invite)
        .await
        .unwrap()
        .unwrap()
        .used_by
}

/// The `user.create` Failure for a real invite that couldn't be used.
async fn assert_recorded_unusable(setup: &Setup, invite: InviteId, reason: &str) {
    let entry = setup
        .audit()
        .await
        .into_iter()
        .find(|entry| {
            entry.action == AuditAction::USER_CREATE && entry.outcome == AuditOutcome::Failure
        })
        .unwrap();
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, Some("192.0.2.10".parse().unwrap()));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Invite(invite)))
    );
    assert_eq!(
        entry.metadata,
        RecordedMetadata::from(&AuditMetadata::new().text("reason", reason).unwrap())
    );
}

fn assert_bad_invite(seen: &Seen) {
    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{ "path": "invite", "message": INVALID_INVITE }])
    );
}

#[sqlx::test(migrations = false)]
async fn an_invite_registers_a_user_who_then_sets_up_2fa_at_login(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Admin, 0).await;

    let seen = register(&setup, code.expose_secret(), "Carol", NEW_PASSWORD).await;

    assert_eq!(seen.status, 201, "{}", seen.text);
    let carol = user_named(&setup, "carol").await.unwrap();
    insta::assert_json_snapshot!(
        "registered",
        redacted(&seen.body, &[(carol.to_string(), "carol")])
    );
    assert!(seen.body.get("access_token").is_none());
    let login = setup.login("carol", NEW_PASSWORD).await;
    assert_eq!(login.status, 200, "{}", login.text);
    assert_eq!(login.body["status"], "mfa_setup_required");
    assert_eq!(used_by(&setup, invite).await, Some(carol));
    let created = setup.last(&AuditAction::USER_CREATE).await;
    assert_eq!(created.outcome, AuditOutcome::Success);
    assert_eq!(created.actor, Some(carol));
    assert_eq!(
        created.target,
        Some(RecordedTarget::Known(AuditTarget::User(carol)))
    );
    assert_eq!(
        created.metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("role", "admin")
                .unwrap()
                .text("via", "invite")
                .unwrap()
                .id("invite_id", invite)
                .unwrap()
        )
    );
    let used = setup.last(&AuditAction::INVITE_USE).await;
    assert_eq!(used.actor, Some(carol));
    assert_eq!(
        used.target,
        Some(RecordedTarget::Known(AuditTarget::Invite(invite)))
    );
    assert_eq!(
        used.metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .id("user_id", carol)
                .unwrap()
                .text("role", "admin")
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn an_invite_registers_once_and_its_reuse_is_refused_and_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Member, 0).await;
    let first = register(&setup, code.expose_secret(), "carol", NEW_PASSWORD).await;
    assert_eq!(first.status, 201, "{}", first.text);

    let again = register(&setup, code.expose_secret(), "dave", NEW_PASSWORD).await;

    assert_bad_invite(&again);
    assert_eq!(user_named(&setup, "dave").await, None);
    assert_recorded_unusable(&setup, invite, "used").await;
}

#[sqlx::test(migrations = false)]
async fn an_invite_is_refused_from_its_expiry_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (expired, expired_code) = setup
        .invite_code(10, owner, Role::Member, -INVITE_TTL)
        .await;
    let (_, last_code) = setup
        .invite_code(11, owner, Role::Member, -INVITE_TTL + 1)
        .await;

    let at_expiry = register(&setup, expired_code.expose_secret(), "carol", NEW_PASSWORD).await;
    let one_millisecond_left =
        register(&setup, last_code.expose_secret(), "dave", NEW_PASSWORD).await;

    assert_bad_invite(&at_expiry);
    assert_recorded_unusable(&setup, expired, "expired").await;
    assert_eq!(
        one_millisecond_left.status, 201,
        "{}",
        one_millisecond_left.text
    );
}

#[sqlx::test(migrations = false)]
async fn a_revoked_invite_is_refused_and_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Member, 0).await;
    let mut tx = setup.db.write().await.unwrap();
    assert!(tx.invites().revoke(invite, at(T0)).await.unwrap());
    tx.commit(super::support::setup_entry()).await.unwrap();

    let seen = register(&setup, code.expose_secret(), "carol", NEW_PASSWORD).await;

    assert_bad_invite(&seen);
    assert_recorded_unusable(&setup, invite, "revoked").await;
}

#[sqlx::test(migrations = false)]
async fn an_invite_whose_creator_lost_the_right_is_refused_and_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.add_user(3, "admin", Role::Admin).await;
    let (invite, code) = setup.invite_code(10, admin, Role::Member, 0).await;
    let mut tx = setup.db.write().await.unwrap();
    assert!(tx.users().set_disabled(admin, true).await.unwrap());
    tx.commit(super::support::setup_entry()).await.unwrap();

    let seen = register(&setup, code.expose_secret(), "carol", NEW_PASSWORD).await;

    assert_bad_invite(&seen);
    assert_recorded_unusable(&setup, invite, "creator_unauthorized").await;
    assert_eq!(used_by(&setup, invite).await, None);
}

#[sqlx::test(migrations = false)]
async fn a_creator_demoted_meanwhile_leaves_the_invite_unusable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.add_user(3, "admin", Role::Admin).await;
    let (invite, code) = setup.invite_code(10, admin, Role::Member, 0).await;
    setup.hooked.queue(Meanwhile::SetRole(admin, Role::Member));

    let seen = register(&setup, code.expose_secret(), "carol", NEW_PASSWORD).await;

    assert_bad_invite(&seen);
    assert_recorded_unusable(&setup, invite, "creator_unauthorized").await;
    assert_eq!(user_named(&setup, "carol").await, None);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_or_malformed_code_is_the_same_422_and_never_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    owner(&setup).await;
    let unknown = format!("afk_iv_{}", "A".repeat(43));
    let long = format!("afk_iv_{}", "A".repeat(100));

    let answers = [
        register(&setup, &unknown, "carol", NEW_PASSWORD).await,
        register(&setup, "afk_st_AAAA", "carol", NEW_PASSWORD).await,
        register(&setup, "", "carol", NEW_PASSWORD).await,
        register(&setup, &long, "carol", NEW_PASSWORD).await,
    ];

    for seen in &answers {
        assert_bad_invite(seen);
        assert_eq!(
            seen.body_without_request_id(),
            answers[0].body_without_request_id()
        );
    }
    assert_eq!(
        entries(&setup, &AuditAction::USER_CREATE, AuditOutcome::Failure).await,
        0
    );
}

#[sqlx::test(migrations = false)]
async fn a_taken_username_is_refused_and_the_invite_stays_pending(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Member, 0).await;

    let seen = register(&setup, code.expose_secret(), "ALICE", NEW_PASSWORD).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{ "path": "username", "message": "is taken" }])
    );
    assert_eq!(used_by(&setup, invite).await, None);
    assert_eq!(
        entries(&setup, &AuditAction::USER_CREATE, AuditOutcome::Failure).await,
        0
    );
}

#[sqlx::test(migrations = false)]
async fn a_weak_password_is_refused_with_its_hints(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Member, 0).await;

    let seen = register(&setup, code.expose_secret(), "carol", "password1234").await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    let fields = seen.body["error"]["fields"].as_array().unwrap();
    assert!(fields.len() >= 2, "{fields:?}");
    assert_eq!(
        fields[0],
        json!({ "path": "password", "message": "is too weak" })
    );
    assert!(fields.iter().all(|field| field["path"] == "password"));
    assert!(!seen.text.contains("password1234"));
    insta::assert_json_snapshot!("register_weak", seen.body_without_request_id());
    assert_eq!(used_by(&setup, invite).await, None);
    assert_eq!(user_named(&setup, "carol").await, None);
}

#[sqlx::test(migrations = false)]
async fn a_password_breaking_its_rules_is_refused_before_anything(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (_, code) = setup.invite_code(10, owner, Role::Member, 0).await;

    let seen = register(&setup, code.expose_secret(), "carol", "short").await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{ "path": "password", "message": "a password has at least 12 characters" }])
    );
}

#[sqlx::test(migrations = false)]
async fn a_name_taken_right_before_the_write_is_422_and_the_invite_stays_pending(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Member, 0).await;
    setup.hooked.queue(Meanwhile::AddUser("carol"));

    let seen = register(&setup, code.expose_secret(), "carol", NEW_PASSWORD).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{ "path": "username", "message": "is taken" }])
    );
    assert_eq!(used_by(&setup, invite).await, None);
    assert_eq!(
        entries(&setup, &AuditAction::INVITE_USE, AuditOutcome::Success).await,
        0
    );
    let carol = setup
        .db
        .users()
        .find_by_username(&Username::try_from("carol").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(carol.id, UserId::new_v7(at(T0), [0xEE; 10]).unwrap());
}

#[sqlx::test(migrations = false)]
async fn an_invite_used_meanwhile_is_refused_and_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Member, 0).await;
    setup
        .hooked
        .queue(Meanwhile::UseInvite(invite, setup.alice));

    let seen = register(&setup, code.expose_secret(), "carol", NEW_PASSWORD).await;

    assert_bad_invite(&seen);
    assert_recorded_unusable(&setup, invite, "used").await;
    assert_eq!(user_named(&setup, "carol").await, None);
}

#[sqlx::test(migrations = false)]
async fn two_registrations_with_one_invite_create_one_user(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = owner(&setup).await;
    let (invite, code) = setup.invite_code(10, owner, Role::Member, 0).await;

    let (carol, dave) = tokio::join!(
        register(&setup, code.expose_secret(), "carol", NEW_PASSWORD),
        register(&setup, code.expose_secret(), "dave", NEW_PASSWORD),
    );

    let mut statuses = [carol.status, dave.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 422], "{} {}", carol.text, dave.text);
    let registered = usize::from(user_named(&setup, "carol").await.is_some())
        + usize::from(user_named(&setup, "dave").await.is_some());
    assert_eq!(registered, 1);
    assert!(used_by(&setup, invite).await.is_some());
    assert_eq!(
        entries(&setup, &AuditAction::INVITE_USE, AuditOutcome::Success).await,
        1
    );
    assert_recorded_unusable(&setup, invite, "used").await;
}

#[sqlx::test(migrations = false)]
async fn a_registration_of_the_wrong_shape_is_400(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let seen = setup
        .post(
            REGISTER,
            None,
            Some(json!({ "invite": "x", "username": "carol", "password": PASSWORD, "role": "admin" })),
        )
        .await;

    assert_eq!(seen.status, 400, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn registering_spends_the_register_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                register: Rate::per_hour(NonZeroU32::MIN),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let owner = owner(&setup).await;
    let (_, code) = setup.invite_code(10, owner, Role::Member, 0).await;
    let unknown = format!("afk_iv_{}", "A".repeat(43));
    assert_bad_invite(&register(&setup, &unknown, "carol", NEW_PASSWORD).await);

    let seen = register(&setup, code.expose_secret(), "carol", NEW_PASSWORD).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
    assert!(seen.headers.contains_key("retry-after"));
    assert_eq!(user_named(&setup, "carol").await, None);
}
