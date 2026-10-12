//! `GET /me/sessions` and `DELETE /me/sessions/{id}` (Plan.md P7.6,
//! ADR-0016): the user's live sessions, newest first, the current one
//! marked; ending another one; the current one only through logout; and
//! someone else's, an ended or an unknown session is 404.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::audit::{AuditAction, AuditTarget, RecordedTarget};
use fleet_core::authz::Role;
use fleet_core::id::SessionId;
use fleet_core::system::Clock as _;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{LOGOUT, ME_SESSIONS, Options, Setup, T0, at, redacted, setup, setup_with};

/// 31 days, past the absolute lifetime, in milliseconds.
const LONG_AGO: i64 = -31 * 24 * 3_600_000;

fn session_path(id: SessionId) -> String {
    format!("{ME_SESSIONS}/{id}")
}

fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

/// Revokes `id` directly.
async fn revoke(setup: &Setup, id: SessionId) {
    let mut tx = setup.db.write().await.unwrap();
    assert!(tx.sessions().revoke(id, setup.clock.now()).await.unwrap());
    tx.commit(super::support::setup_entry()).await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_live_sessions_are_listed_newest_first_with_the_current_one_marked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (old, _) = setup.session_since(setup.alice, LONG_AGO).await;
    setup.clock.advance(Duration::from_secs(1));
    let (first, _) = setup.session().await;
    setup.clock.advance(Duration::from_secs(1));
    let (ended, _) = setup.session().await;
    revoke(&setup, ended).await;
    setup.clock.advance(Duration::from_secs(1));
    let (current, token) = setup.session().await;
    let bob = setup.add_user(5, "bob", Role::Member).await;
    setup.session_of(bob).await;

    let seen = setup.get(ME_SESSIONS, Some(&token)).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(ids(&seen.body), [current.to_string(), first.to_string()]);
    assert!(!seen.text.contains(&old.to_string()));
    insta::assert_json_snapshot!(
        "sessions_page",
        redacted(
            &seen.body,
            &[
                (current.to_string(), "current"),
                (first.to_string(), "first")
            ]
        )
    );
}

#[sqlx::test(migrations = false)]
async fn the_sessions_page_with_their_cursor(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let mut sessions = Vec::new();
    for _ in 0..3 {
        sessions.push(setup.session().await);
        setup.clock.advance(Duration::from_secs(1));
    }
    let token = &sessions[2].1;

    let first = setup
        .get(&format!("{ME_SESSIONS}?limit=2"), Some(token))
        .await;
    let next = first.body["next"].as_str().unwrap().to_owned();
    let last = setup
        .get(&format!("{ME_SESSIONS}?before={next}&limit=2"), Some(token))
        .await;

    assert_eq!(
        ids(&first.body),
        [sessions[2].0.to_string(), sessions[1].0.to_string()]
    );
    assert_eq!(ids(&last.body), [sessions[0].0.to_string()]);
    assert!(last.body.get("next").is_none());
}

#[sqlx::test(migrations = false)]
async fn another_session_is_ended(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (other, other_token) = setup.session().await;
    let (current, token) = setup.session().await;

    let seen = setup.delete(&session_path(other), Some(&token)).await;

    assert_eq!(seen.status, 204, "{}", seen.text);
    assert!(!setup.live(other).await);
    assert!(setup.live(current).await);
    assert_eq!(
        setup.post(LOGOUT, Some(&other_token), None).await.status,
        401
    );
    let entry = setup.last(&AuditAction::SESSION_REVOKE).await;
    assert_eq!(entry.actor, Some(setup.alice));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Session(other)))
    );
}

#[sqlx::test(migrations = false)]
async fn the_current_session_ends_only_through_logout(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (current, token) = setup.session().await;

    let seen = setup.delete(&session_path(current), Some(&token)).await;

    assert_eq!(seen.status, 409, "{}", seen.text);
    assert_eq!(seen.code(), "conflict");
    assert!(setup.live(current).await);
}

#[sqlx::test(migrations = false)]
async fn someone_elses_or_an_ended_session_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let bob = setup.add_user(5, "bob", Role::Member).await;
    let (bobs, _) = setup.session_of(bob).await;
    let (ended, _) = setup.session().await;
    revoke(&setup, ended).await;
    let (expired, _) = setup.session_since(setup.alice, LONG_AGO).await;
    let unknown = SessionId::new_v7(at(T0), [99; 10]).unwrap();
    let (_, token) = setup.session().await;

    let answers = [
        setup.delete(&session_path(bobs), Some(&token)).await,
        setup.delete(&session_path(ended), Some(&token)).await,
        setup.delete(&session_path(expired), Some(&token)).await,
        setup.delete(&session_path(unknown), Some(&token)).await,
        setup
            .delete(&format!("{ME_SESSIONS}/not-an-id"), Some(&token))
            .await,
    ];

    for seen in &answers {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(
            seen.body_without_request_id(),
            answers[0].body_without_request_id()
        );
    }
    assert!(setup.live(bobs).await);
}

#[sqlx::test(migrations = false)]
async fn a_limit_outside_its_range_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (_, token) = setup.session().await;

    let seen = setup
        .get(&format!("{ME_SESSIONS}?limit=0"), Some(&token))
        .await;

    assert_eq!(seen.status, 422, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn the_session_routes_spend_the_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
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
    let (other, _) = setup.session().await;
    let (_, token) = setup.session().await;
    assert_eq!(setup.get(ME_SESSIONS, Some(&token)).await.status, 200);

    let seen = setup.delete(&session_path(other), Some(&token)).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
    assert!(setup.live(other).await);
}
