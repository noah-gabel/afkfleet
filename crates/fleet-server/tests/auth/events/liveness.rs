//! An open socket doesn't outlive its session (ADR-0020, S31, R8, R18):
//! every commit wakes it to check its session again, so a logout, a
//! revocation, a password change, a 2FA replacement, a role change or a
//! disabling closes it with 4001 at once, and a check that fails closes it
//! with 1013; and it closes, without a read, when the access token it
//! opened with expires.

use core::time::Duration;

use fleet_api_types::events::SocketClose;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::system::Clock as _;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::{LOGOUT, ME_PASSWORD, setup};
use super::{Served, closed, ready, said};

/// A strong new password.
const NEW_PASSWORD: &str = "violet staircase humming quietly";

#[sqlx::test(migrations = false)]
async fn logging_out_closes_the_socket_with_4001(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;

    let out = setup.post(LOGOUT, Some(&alice.token), None).await;

    assert_eq!(out.status, 204, "{}", out.text);
    assert_eq!(
        closed(&mut socket).await,
        said(SocketClose::Unauthenticated)
    );
}

#[sqlx::test(migrations = false)]
async fn ending_the_session_from_another_closes_its_socket(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let (_, other) = setup.session().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;

    let ended = setup
        .delete(
            &format!("/api/v1/me/sessions/{}", alice.session),
            Some(&other),
        )
        .await;

    assert_eq!(ended.status, 204, "{}", ended.text);
    assert_eq!(
        closed(&mut socket).await,
        said(SocketClose::Unauthenticated)
    );
}

#[sqlx::test(migrations = false)]
async fn a_password_change_closes_the_other_sessions_sockets_and_keeps_its_own(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let (_, other) = setup.session().await;
    let served = Served::start(&setup).await;
    let mut own = ready(&setup, &served, &alice.token).await;
    let mut others = ready(&setup, &served, &other).await;
    setup.stamp(alice.session).await;

    let changed = setup
        .post(
            ME_PASSWORD,
            Some(&alice.token),
            Some(json!({ "new_password": NEW_PASSWORD })),
        )
        .await;

    assert_eq!(changed.status, 204, "{}", changed.text);
    assert_eq!(
        closed(&mut others).await,
        said(SocketClose::Unauthenticated)
    );
    setup.live_events.sockets().shutdown().await;
    assert_eq!(
        closed(&mut own).await,
        said(SocketClose::ShuttingDown),
        "the changing session's socket stayed open until the shutdown"
    );
}

#[sqlx::test(migrations = false)]
async fn replacing_the_authenticator_closes_the_other_sessions_sockets(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let (_, other) = setup.session().await;
    let served = Served::start(&setup).await;
    let mut others = ready(&setup, &served, &other).await;

    // What a replacement's confirm commits: every other session revoked,
    // with its entry (the HTTP flow itself is tested in tests/auth/mfa).
    let mut tx = setup.db.write().await.unwrap();
    tx.sessions()
        .revoke_all_for_user(alice.id, Some(alice.session), setup.clock.now())
        .await
        .unwrap();
    tx.commit(NewAuditEntry::new(
        setup.clock.now(),
        AuditAction::try_from("test.mfa_replace").unwrap(),
        AuditOutcome::Success,
    ))
    .await
    .unwrap();

    assert_eq!(
        closed(&mut others).await,
        said(SocketClose::Unauthenticated)
    );
}

#[sqlx::test(migrations = false)]
async fn a_session_that_cant_be_checked_after_a_commit_closes_the_socket_with_1013(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;
    setup.hooked.fail_socket_checks();

    // Any commit wakes the socket to check its session.
    let tx = setup.db.write().await.unwrap();
    tx.commit(NewAuditEntry::new(
        setup.clock.now(),
        AuditAction::try_from("test.any_commit").unwrap(),
        AuditOutcome::Success,
    ))
    .await
    .unwrap();

    assert_eq!(closed(&mut socket).await, said(SocketClose::Busy));
}

#[sqlx::test(migrations = false)]
async fn disabling_the_user_closes_their_socket(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let owner = setup.person(2, "AfkOwner", Role::Owner).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;
    setup.stamp(owner.session).await;

    let disabled = setup
        .patch(
            &format!("/api/v1/users/{}", alice.id),
            Some(&owner.token),
            json!({ "disabled": true }),
        )
        .await;

    assert_eq!(disabled.status, 200, "{}", disabled.text);
    assert_eq!(
        closed(&mut socket).await,
        said(SocketClose::Unauthenticated)
    );
}

#[sqlx::test(migrations = false)]
async fn a_role_change_closes_the_users_socket(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let owner = setup.person(2, "AfkOwner", Role::Owner).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;
    setup.stamp(owner.session).await;

    let promoted = setup
        .patch(
            &format!("/api/v1/users/{}", alice.id),
            Some(&owner.token),
            json!({ "role": "admin" }),
        )
        .await;

    assert_eq!(promoted.status, 200, "{}", promoted.text);
    assert_eq!(
        closed(&mut socket).await,
        said(SocketClose::Unauthenticated)
    );
}

#[sqlx::test(migrations = false)]
async fn a_socket_closes_when_the_access_token_it_opened_with_expires(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    // 200 ms of the access token's 15 minutes are left at the connect.
    setup
        .clock
        .advance(Duration::from_millis(15 * 60 * 1000 - 200));
    let started = tokio::time::Instant::now();
    let mut socket = ready(&setup, &served, &alice.token).await;

    let close = closed(&mut socket).await;

    assert_eq!(close, said(SocketClose::Unauthenticated));
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "not before"
    );
}
