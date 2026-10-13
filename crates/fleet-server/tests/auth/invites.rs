//! `POST`, `GET` and `DELETE /invites` (Plan.md P7.9, P7.1, P7.12,
//! ADR-0010, ADR-0016): who may invite whom, the code shown once and stored
//! only as its hash, the cap on pending invites, the list in every state,
//! revoking, and the re-check of each write in its own transaction.

use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::auth::InviteTtl;
use fleet_core::authz::Role;
use fleet_core::id::InviteId;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::tokens::{TokenKind, Tokens as _};
use fleet_testkit::system::SeededRandom;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{
    INVITE_TTL, INVITES, Meanwhile, Options, Person, Setup, T0, at, redacted, setup, setup_with,
};

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

fn names(cast: &Cast) -> Vec<(String, &'static str)> {
    vec![
        (cast.owner.id.to_string(), "owner"),
        (cast.admin.id.to_string(), "admin"),
        (cast.other_admin.id.to_string(), "other_admin"),
        (cast.alice.id.to_string(), "alice"),
        (cast.bob.id.to_string(), "bob"),
    ]
}

fn invite_path(id: InviteId) -> String {
    format!("{INVITES}/{id}")
}

fn metadata(build: &AuditMetadata) -> RecordedMetadata {
    RecordedMetadata::from(build)
}

async fn entries(setup: &Setup, action: &AuditAction) -> usize {
    setup
        .audit()
        .await
        .iter()
        .filter(|entry| &entry.action == action)
        .count()
}

/// How many invites are stored.
async fn stored(setup: &Setup) -> usize {
    setup
        .db
        .invites()
        .list(fleet_server::ports::page::Page {
            before: None,
            limit: fleet_server::ports::page::PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
        .len()
}

async fn create(setup: &Setup, by: &Person, role: &str) -> super::support::Seen {
    setup
        .post(INVITES, Some(&by.token), Some(json!({ "role": role })))
        .await
}

fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

// --- Creating ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_admin_creates_a_member_invite_whose_code_is_shown_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = create(&setup, &cast.admin, "member").await;

    assert_eq!(seen.status, 201, "{}", seen.text);
    let code = seen.body["code"].as_str().unwrap().to_owned();
    let secret = code.strip_prefix("afk_iv_").unwrap();
    assert_eq!(secret.len(), 43, "{code}");
    assert!(secret.bytes().all(is_base64url), "{code}");
    let hash = Sha256Tokens::new(Arc::new(SeededRandom::new(1)))
        .hash_presented(TokenKind::Invite, &code)
        .unwrap();
    let invite = setup
        .db
        .invites()
        .find_by_code_hash(&hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(invite.role, Role::Member);
    assert_eq!(invite.created_by, cast.admin.id);
    assert_eq!(invite.expires_at, at(T0 + INVITE_TTL));
    let mut body = seen.body.clone();
    body["code"] = json!("[code]");
    insta::assert_json_snapshot!("invite_created", redacted(&body, &names(&cast)));
    let listed = setup.get(INVITES, Some(&cast.admin.token)).await;
    assert!(!listed.text.contains(secret), "{}", listed.text);
    let entry = setup.last(&AuditAction::INVITE_CREATE).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(cast.admin.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Invite(invite.id)))
    );
    assert_eq!(
        entry.metadata,
        metadata(
            &AuditMetadata::new()
                .text("role", "member")
                .unwrap()
                .time("expires_at", at(T0 + INVITE_TTL))
                .unwrap()
        )
    );
    assert!(!format!("{:?}", setup.audit().await).contains(secret));
}

#[sqlx::test(migrations = false)]
async fn the_owner_creates_an_admin_invite(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = create(&setup, &cast.owner, "admin").await;

    assert_eq!(seen.status, 201, "{}", seen.text);
    assert_eq!(seen.body["invite"]["role"], "admin");
    assert_eq!(seen.body["invite"]["created_by"]["username"], "owner");
}

#[sqlx::test(migrations = false)]
async fn an_admin_cant_invite_an_admin(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = create(&setup, &cast.admin, "admin").await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(stored(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn nobody_invites_an_owner(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = create(&setup, &cast.owner, "owner").await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(stored(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_member_cant_invite(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = create(&setup, &cast.alice, "member").await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(stored(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn an_invite_lives_the_configured_lifetime(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            invite_ttl: InviteTtl::try_new(5).unwrap(),
            ..Options::default()
        },
    )
    .await;
    let cast = cast(&setup).await;

    let seen = create(&setup, &cast.admin, "member").await;

    assert_eq!(seen.status, 201, "{}", seen.text);
    assert_eq!(
        redacted(&seen.body["invite"]["expires_at"], &[]),
        json!("[T0+18000s]")
    );
}

#[sqlx::test(migrations = false)]
async fn the_twenty_first_pending_invite_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    for n in 0..19 {
        setup
            .add_invite(10 + n, cast.admin.id, Role::Member, 0)
            .await;
    }
    let used = setup.add_invite(40, cast.admin.id, Role::Member, 0).await;
    setup.use_invite(used, cast.bob.id).await;
    setup
        .add_invite(41, cast.admin.id, Role::Member, -INVITE_TTL)
        .await;
    setup
        .add_invite(42, cast.other_admin.id, Role::Member, 0)
        .await;

    let twentieth = create(&setup, &cast.admin, "member").await;
    let twenty_first = create(&setup, &cast.admin, "member").await;
    let others = create(&setup, &cast.other_admin, "member").await;

    assert_eq!(twentieth.status, 201, "{}", twentieth.text);
    assert_eq!(twenty_first.status, 409, "{}", twenty_first.text);
    assert_eq!(twenty_first.code(), "conflict");
    assert_eq!(others.status, 201, "{}", others.text);
}

#[sqlx::test(migrations = false)]
async fn a_create_request_of_the_wrong_shape_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let unknown_role = create(&setup, &cast.owner, "guest").await;
    let extra = setup
        .post(
            INVITES,
            Some(&cast.owner.token),
            Some(json!({ "role": "member", "ttl": 1 })),
        )
        .await;

    assert_eq!(unknown_role.status, 400, "{}", unknown_role.text);
    assert_eq!(extra.status, 400, "{}", extra.text);
    assert_eq!(stored(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn creating_spends_the_users_limit(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let cast = cast(&setup).await;
    assert_eq!(create(&setup, &cast.admin, "member").await.status, 201);

    let seen = create(&setup, &cast.admin, "member").await;

    assert_eq!(seen.status, 429, "{}", seen.text);
    assert_eq!(stored(&setup).await, 1);
}

// --- The list ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_list_shows_every_invite_in_its_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let used = setup
        .add_invite(10, cast.admin.id, Role::Member, -3_000)
        .await;
    let revoked = setup
        .add_invite(11, cast.admin.id, Role::Member, -2_000)
        .await;
    setup
        .add_invite(12, cast.other_admin.id, Role::Member, -INVITE_TTL)
        .await;
    setup
        .add_invite(13, cast.owner.id, Role::Admin, -1_000)
        .await;
    setup.use_invite(used, cast.bob.id).await;
    assert_eq!(
        setup
            .delete(&invite_path(revoked), Some(&cast.admin.token))
            .await
            .status,
        204
    );

    let seen = setup.get(INVITES, Some(&cast.other_admin.token)).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let states: Vec<&str> = seen.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["pending", "revoked", "used", "expired"]);
    insta::assert_json_snapshot!("invites_page", redacted(&seen.body, &names(&cast)));
}

#[sqlx::test(migrations = false)]
async fn the_invite_list_pages_with_its_cursor(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let mut invites = Vec::new();
    for n in 0..3_u8 {
        invites.push(
            setup
                .add_invite(10 + n, cast.admin.id, Role::Member, i64::from(n) * 1_000)
                .await,
        );
    }
    let ids = |seen: &Value| -> Vec<String> {
        seen["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    };

    let first = setup
        .get(&format!("{INVITES}?limit=2"), Some(&cast.admin.token))
        .await;
    let next = first.body["next"].as_str().unwrap().to_owned();
    let last = setup
        .get(
            &format!("{INVITES}?before={next}&limit=2"),
            Some(&cast.admin.token),
        )
        .await;

    assert_eq!(
        ids(&first.body),
        [invites[2].to_string(), invites[1].to_string()]
    );
    assert_eq!(ids(&last.body), [invites[0].to_string()]);
    assert!(last.body.get("next").is_none());
}

#[sqlx::test(migrations = false)]
async fn a_member_cant_list_invites(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup.get(INVITES, Some(&cast.alice.token)).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
}

#[sqlx::test(migrations = false)]
async fn a_limit_outside_its_range_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;

    let seen = setup
        .get(&format!("{INVITES}?limit=101"), Some(&cast.admin.token))
        .await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(seen.body["error"]["fields"][0]["path"], "limit");
}

// --- Revoking ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_admin_revokes_a_pending_member_invite(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let invite = setup.add_invite(10, cast.owner.id, Role::Member, 0).await;
    setup.clock.advance(Duration::from_secs(60));

    let seen = setup
        .delete(&invite_path(invite), Some(&cast.admin.token))
        .await;

    assert_eq!(seen.status, 204, "{}", seen.text);
    let stored = setup.db.invites().find(invite).await.unwrap().unwrap();
    assert_eq!(stored.revoked_at, Some(at(T0 + 60_000)));
    let entry = setup.last(&AuditAction::INVITE_REVOKE).await;
    assert_eq!(entry.actor, Some(cast.admin.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Invite(invite)))
    );
    assert_eq!(
        entry.metadata,
        metadata(&AuditMetadata::new().text("role", "member").unwrap())
    );
}

#[sqlx::test(migrations = false)]
async fn revoking_again_or_an_expired_invite_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let invite = setup.add_invite(10, cast.admin.id, Role::Member, 0).await;
    let expired = setup
        .add_invite(11, cast.admin.id, Role::Member, -INVITE_TTL)
        .await;
    assert_eq!(
        setup
            .delete(&invite_path(invite), Some(&cast.admin.token))
            .await
            .status,
        204
    );

    let again = setup
        .delete(&invite_path(invite), Some(&cast.admin.token))
        .await;
    let past = setup
        .delete(&invite_path(expired), Some(&cast.admin.token))
        .await;

    assert_eq!(again.status, 204, "{}", again.text);
    assert_eq!(past.status, 204, "{}", past.text);
    assert_eq!(entries(&setup, &AuditAction::INVITE_REVOKE).await, 1);
    let expired = setup.db.invites().find(expired).await.unwrap().unwrap();
    assert_eq!(expired.revoked_at, None);
}

#[sqlx::test(migrations = false)]
async fn a_used_invite_cant_be_revoked(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let invite = setup.add_invite(10, cast.admin.id, Role::Member, 0).await;
    setup.use_invite(invite, cast.bob.id).await;

    let seen = setup
        .delete(&invite_path(invite), Some(&cast.admin.token))
        .await;

    assert_eq!(seen.status, 409, "{}", seen.text);
    assert_eq!(entries(&setup, &AuditAction::INVITE_REVOKE).await, 0);
}

#[sqlx::test(migrations = false)]
async fn an_admin_cant_revoke_the_owners_admin_invite(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let invite = setup.add_invite(10, cast.owner.id, Role::Admin, 0).await;

    let seen = setup
        .delete(&invite_path(invite), Some(&cast.admin.token))
        .await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    let stored = setup.db.invites().find(invite).await.unwrap().unwrap();
    assert_eq!(stored.revoked_at, None);
}

#[sqlx::test(migrations = false)]
async fn a_member_gets_404_for_any_invite(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let invite = setup.add_invite(10, cast.admin.id, Role::Member, 0).await;
    let unknown = InviteId::new_v7(at(T0), [99; 10]).unwrap();

    let existing = setup
        .delete(&invite_path(invite), Some(&cast.alice.token))
        .await;
    let nothing = setup
        .delete(&invite_path(unknown), Some(&cast.alice.token))
        .await;
    let malformed = setup
        .delete(&format!("{INVITES}/x"), Some(&cast.admin.token))
        .await;

    assert_eq!(existing.status, 404, "{}", existing.text);
    assert_eq!(
        existing.body_without_request_id(),
        nothing.body_without_request_id()
    );
    assert_eq!(malformed.status, 404, "{}", malformed.text);
}

// --- The step-up -------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn creating_an_invite_needs_a_fresh_step_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = unstamped_cast(&setup).await;

    let seen = create(&setup, &cast.owner, "member").await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "step_up_required");
    assert_eq!(stored(&setup).await, 0);
    assert_eq!(entries(&setup, &AuditAction::INVITE_CREATE).await, 0);
}

#[sqlx::test(migrations = false)]
async fn revoking_an_invite_needs_a_fresh_step_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = unstamped_cast(&setup).await;
    let invite = setup.add_invite(10, cast.owner.id, Role::Member, 0).await;

    let seen = setup
        .delete(&invite_path(invite), Some(&cast.owner.token))
        .await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "step_up_required");
    let stored = setup.db.invites().find(invite).await.unwrap().unwrap();
    assert_eq!(stored.revoked_at, None);
    assert_eq!(entries(&setup, &AuditAction::INVITE_REVOKE).await, 0);
}

// --- The re-check in the transaction --------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_admin_demoted_meanwhile_cant_invite(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    setup
        .hooked
        .queue(Meanwhile::SetRole(cast.admin.id, Role::Member));

    let seen = create(&setup, &cast.admin, "member").await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(stored(&setup).await, 0);
    assert_eq!(entries(&setup, &AuditAction::INVITE_CREATE).await, 0);
}

#[sqlx::test(migrations = false)]
async fn an_invite_used_meanwhile_cant_be_revoked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let invite = setup.add_invite(10, cast.admin.id, Role::Member, 0).await;
    setup
        .hooked
        .queue(Meanwhile::UseInvite(invite, cast.bob.id));

    let seen = setup
        .delete(&invite_path(invite), Some(&cast.admin.token))
        .await;

    assert_eq!(seen.status, 409, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn a_session_revoked_meanwhile_cant_revoke(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let cast = cast(&setup).await;
    let invite = setup.add_invite(10, cast.admin.id, Role::Member, 0).await;
    setup
        .hooked
        .queue(Meanwhile::RevokeSession(cast.admin.session));

    let seen = setup
        .delete(&invite_path(invite), Some(&cast.admin.token))
        .await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    let stored = setup.db.invites().find(invite).await.unwrap().unwrap();
    assert_eq!(stored.revoked_at, None);
}
