//! `POST /agents/enrollment-tokens` (Plan.md P10.3, Appendix B, ADR-0019):
//! the Owner's token in its bundle, shown once and stored only as its hash;
//! everyone else refused; the step-up, the cap, the user's limit, the
//! switched-off control plane, and the re-check in the transaction.

use core::num::NonZeroU32;
use std::sync::Arc;

use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::authz::Role;
use fleet_core::enrollment::{EnrollmentBundle, MAX_PENDING_TOKENS};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::tokens::{TokenKind, Tokens as _};
use fleet_testkit::system::SeededRandom;
use secrecy::ExposeSecret as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{
    CA_FINGERPRINT, ENROLLMENT_TOKENS, Meanwhile, Options, Person, Setup, T0, at, redacted,
    setup_with,
};

/// The token's lifetime, in milliseconds.
const TTL: i64 = 15 * 60_000;

/// A server with the control plane on.
async fn on(options: SqliteConnectOptions) -> Setup {
    setup_with(
        options,
        Options {
            control_plane: true,
            ..Options::default()
        },
    )
    .await
}

async fn create(setup: &Setup, by: &Person) -> super::support::Seen {
    setup.post(ENROLLMENT_TOKENS, Some(&by.token), None).await
}

/// How many tokens are pending now.
async fn pending(setup: &Setup) -> u32 {
    let mut tx = setup.db.write().await.unwrap();
    tx.enrollment_tokens().count_pending(at(T0)).await.unwrap()
}

async fn created_entries(setup: &Setup) -> usize {
    setup
        .audit()
        .await
        .iter()
        .filter(|entry| entry.action == AuditAction::ENROLLMENT_TOKEN_CREATE)
        .count()
}

/// The Owner, stepped up now.
async fn owner(setup: &Setup) -> Person {
    let owner = setup.person(2, "owner", Role::Owner).await;
    setup.stamp(owner.session).await;
    owner
}

#[sqlx::test(migrations = false)]
async fn the_owner_creates_a_token_whose_bundle_is_shown_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options).await;
    let owner = owner(&setup).await;

    let seen = create(&setup, &owner).await;

    assert_eq!(seen.status, 201, "{}", seen.text);
    let text = seen.body["bundle"].as_str().unwrap().to_owned();
    let bundle = EnrollmentBundle::try_from(text.as_str()).unwrap();
    assert_eq!(bundle.ca_fingerprint(), CA_FINGERPRINT);
    assert_eq!(
        seen.body["ca_fingerprint"],
        json!(CA_FINGERPRINT.to_string())
    );
    let hash = Sha256Tokens::new(Arc::new(SeededRandom::new(1)))
        .hash_presented(TokenKind::Enrollment, bundle.token().expose_secret())
        .unwrap();
    let stored = setup
        .db
        .enrollment_tokens()
        .find_by_hash(&hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.body["id"], json!(stored.id.to_string()));
    assert_eq!(stored.created_by, Some(owner.id));
    assert_eq!(stored.expires_at, at(T0 + TTL));
    let mut body = seen.body.clone();
    body["bundle"] = json!("[bundle]");
    insta::assert_json_snapshot!(
        "enrollment_token_created",
        redacted(&body, &[(owner.id.to_string(), "owner")])
    );
    let entry = setup.last(&AuditAction::ENROLLMENT_TOKEN_CREATE).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(owner.id));
    assert_eq!(entry.ip, Some("192.0.2.10".parse().unwrap()));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::EnrollmentToken(
            stored.id
        )))
    );
    assert_eq!(
        entry.metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .time("expires_at", at(T0 + TTL))
                .unwrap()
                .text("via", "http")
                .unwrap()
        )
    );
    let token = bundle.token().expose_secret().to_owned();
    assert!(!format!("{:?}", setup.audit().await).contains(&token));
}

#[sqlx::test(migrations = false)]
async fn only_the_owner_creates_a_token(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = on(options).await;
    let admin = setup.person(3, "admin", Role::Admin).await;
    let member = setup.person(4, "member", Role::Member).await;
    setup.stamp(admin.session).await;
    setup.stamp(member.session).await;

    let by_admin = create(&setup, &admin).await;
    let by_member = create(&setup, &member).await;

    for seen in [&by_admin, &by_member] {
        assert_eq!(seen.status, 403, "{}", seen.text);
        assert_eq!(seen.code(), "forbidden");
    }
    assert_eq!(pending(&setup).await, 0);
    assert_eq!(created_entries(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn without_credentials_it_is_401(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = on(options).await;

    let seen = setup.post(ENROLLMENT_TOKENS, None, None).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(pending(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn creating_a_token_needs_a_fresh_step_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options).await;
    let owner = setup.person(2, "owner", Role::Owner).await;

    let seen = create(&setup, &owner).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "step_up_required");
    assert_eq!(pending(&setup).await, 0);
    assert_eq!(created_entries(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn the_twenty_first_pending_token_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options).await;
    let owner = owner(&setup).await;
    for _ in 1..MAX_PENDING_TOKENS {
        assert_eq!(create(&setup, &owner).await.status, 201);
    }

    let twentieth = create(&setup, &owner).await;
    let twenty_first = create(&setup, &owner).await;

    assert_eq!(twentieth.status, 201, "{}", twentieth.text);
    assert_eq!(twenty_first.status, 409, "{}", twenty_first.text);
    assert_eq!(twenty_first.code(), "conflict");
    assert_eq!(pending(&setup).await, MAX_PENDING_TOKENS);
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
            control_plane: true,
            ..Options::default()
        },
    )
    .await;
    let owner = owner(&setup).await;
    assert_eq!(create(&setup, &owner).await.status, 201);

    let seen = create(&setup, &owner).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
    assert_eq!(pending(&setup).await, 1);
}

#[sqlx::test(migrations = false)]
async fn with_the_control_plane_off_the_route_is_404_after_the_step_up_check(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, Options::default()).await;
    let owner = setup.person(2, "owner", Role::Owner).await;
    let unstamped = create(&setup, &owner).await;
    setup.stamp(owner.session).await;

    let seen = create(&setup, &owner).await;

    assert_eq!(unstamped.status, 403, "{}", unstamped.text);
    assert_eq!(unstamped.code(), "step_up_required");
    assert_eq!(seen.status, 404, "{}", seen.text);
    assert_eq!(seen.code(), "not_found");
    assert_eq!(pending(&setup).await, 0);
    assert_eq!(created_entries(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_session_revoked_meanwhile_creates_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options).await;
    let owner = owner(&setup).await;
    setup.hooked.queue(Meanwhile::RevokeSession(owner.session));

    let seen = create(&setup, &owner).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(pending(&setup).await, 0);
    assert_eq!(created_entries(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn an_owner_demoted_meanwhile_creates_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options).await;
    let owner = owner(&setup).await;
    setup
        .hooked
        .queue(Meanwhile::SetRole(owner.id, Role::Admin));

    let seen = create(&setup, &owner).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(pending(&setup).await, 0);
}
