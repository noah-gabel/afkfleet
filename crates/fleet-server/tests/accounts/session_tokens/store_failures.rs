//! A rotated refresh token whose store fails: nothing is handed out, the
//! status is left alone, and the token stays in the account's slot. The
//! next request stores it before any refresh. It's discarded only when the
//! row's envelope changed or the row is gone, never because the account's
//! status or its owner changed.

use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, RecordedValue};
use fleet_core::system::RandomError;
use fleet_server::app::session_tokens::SessionTokenError;
use fleet_server::ports::store::StoreError;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::msauth::Script;
use crate::support::service::{
    PROFILE, Setup, T0, at, bounded, json_logs, levels, mc_session, metadata, ms_tokens, profile,
};
use fleet_server::ports::msauth::{Failure, RefreshError};

const KEPT: &str = "an account's rotated refresh token couldn't be stored; it's kept until a later request stores it";

async fn active(setup: &Setup) -> fleet_core::id::AccountId {
    setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "refresh-0")
        .await
}

/// Scripts a refresh to `refresh` and a session `minecraft`.
fn script(setup: &Setup, refresh: &str, minecraft: &str) {
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access", refresh))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        minecraft, PROFILE, "AfkBot1",
    ))));
}

/// A first request whose store fails: `refresh-1` is kept.
async fn keep_refresh_1(setup: &Setup, id: fleet_core::id::AccountId) {
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access", "refresh-1"))));
    setup.store.fail_next_writes(1);
    let answer = bounded(setup.service.session(id, false)).await;
    assert!(
        matches!(answer, Err(SessionTokenError::Store(StoreError::Busy))),
        "{answer:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_failed_store_hands_out_nothing_leaves_the_status_and_logs_one_error(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (logs, guard) = json_logs();

    keep_refresh_1(&setup, id).await;

    drop(guard);
    assert_eq!(setup.stored_token(id).await, "refresh-0");
    assert_eq!(setup.status(id).await, AccountStatus::Active);
    assert_eq!(setup.server_entries().await, 0);
    assert_eq!(setup.sessions_started_with(), Vec::<String>::new());
    assert_eq!(levels(&logs, KEPT), ["ERROR"], "{}", logs.text());
}

#[sqlx::test(migrations = false)]
async fn a_failed_seal_keeps_the_token_too(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access", "refresh-1"))));
    setup.random.fail_next(RandomError::Unavailable);

    let failed = bounded(setup.service.session(id, false)).await;
    script(&setup, "refresh-2", "minecraft-2");
    let next = bounded(setup.service.session(id, false)).await.unwrap();

    assert!(
        matches!(failed, Err(SessionTokenError::Seal(_))),
        "{failed:?}"
    );
    assert_eq!(next.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
    assert_eq!(setup.stored_token(id).await, "refresh-2");
}

#[sqlx::test(migrations = false)]
async fn the_next_request_stores_the_kept_token_before_refreshing_with_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    script(&setup, "refresh-2", "minecraft-2");

    let next = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(next.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
    assert_eq!(setup.stored_token(id).await, "refresh-2");
    let entries = setup.entries(&AuditAction::ACCOUNT_REFRESH).await;
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(
            entry.metadata.entries(),
            metadata(&[("rotated", RecordedValue::Bool(true))])
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_failed_retry_stops_before_any_refresh_and_keeps_the_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    setup.store.fail_next_writes(1);

    let retry = bounded(setup.service.session(id, false)).await;
    script(&setup, "refresh-2", "minecraft-2");
    let next = bounded(setup.service.session(id, false)).await.unwrap();

    assert!(
        matches!(retry, Err(SessionTokenError::Store(StoreError::Busy))),
        "{retry:?}"
    );
    assert_eq!(next.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
    assert_eq!(setup.stored_token(id).await, "refresh-2");
}

#[sqlx::test(migrations = false)]
async fn a_kept_token_waits_while_the_owner_is_disabled_and_is_stored_once_enabled(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    setup.set_disabled(setup.owner, true).await;

    let disabled = bounded(setup.service.session(id, false)).await;
    let calls_while_disabled = setup.fake.calls().len();
    setup.set_disabled(setup.owner, false).await;
    script(&setup, "refresh-2", "minecraft-2");
    let enabled = bounded(setup.service.session(id, false)).await.unwrap();

    assert!(
        matches!(disabled, Err(SessionTokenError::OwnerDisabled)),
        "{disabled:?}"
    );
    assert_eq!(calls_while_disabled, 1);
    assert_eq!(enabled.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
    assert_eq!(setup.stored_token(id).await, "refresh-2");
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_REFRESH).await.len(), 2);
}

#[sqlx::test(migrations = false)]
async fn a_kept_token_waits_while_the_account_isnt_active(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    setup.set_status(id, AccountStatus::Pending).await;

    let pending = bounded(setup.service.session(id, false)).await;
    setup.set_status(id, AccountStatus::Active).await;
    script(&setup, "refresh-2", "minecraft-2");
    let active_again = bounded(setup.service.session(id, false)).await.unwrap();

    assert!(
        matches!(
            pending,
            Err(SessionTokenError::NotActive {
                status: AccountStatus::Pending
            })
        ),
        "{pending:?}"
    );
    assert_eq!(active_again.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
}

#[sqlx::test(migrations = false)]
async fn a_kept_token_is_discarded_when_the_account_was_linked_again(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    setup.relink(id, "relinked").await;
    script(&setup, "refresh-2", "minecraft-2");

    let next = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(next.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "relinked"]);
    assert_eq!(setup.stored_token(id).await, "refresh-2");
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_REFRESH).await.len(), 1);
}

#[sqlx::test(migrations = false)]
async fn a_kept_token_is_discarded_when_the_account_was_deleted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    setup.delete(id).await;

    let deleted = bounded(setup.service.session(id, false)).await;
    let again = setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "linked-again")
        .await;
    script(&setup, "refresh-2", "minecraft-2");
    let next = bounded(setup.service.session(again, false)).await.unwrap();

    assert!(
        matches!(deleted, Err(SessionTokenError::NotFound)),
        "{deleted:?}"
    );
    assert_eq!(again, id);
    assert_eq!(next.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "linked-again"]);
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_REFRESH).await.len(), 1);
}

/// A kept token is stored even during a backoff, since that calls no
/// Microsoft; the backoff still answers the request then.
#[sqlx::test(migrations = false)]
async fn a_kept_token_is_stored_during_a_pause_which_still_answers_the_request(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let limited = setup
        .link(
            2,
            "AfkBot2",
            &profile(2),
            AccountStatus::Active,
            "refresh-b",
        )
        .await;
    keep_refresh_1(&setup, id).await;
    setup
        .fake
        .script_refresh(Script::Answer(Err(RefreshError::Failed(
            Failure::RateLimited,
        ))));
    bounded(setup.service.session(limited, false))
        .await
        .unwrap_err();

    let paused = bounded(setup.service.session(id, false)).await;

    assert!(
        matches!(paused, Err(SessionTokenError::BackingOff { until }) if until == at(T0 + 30_000)),
        "{paused:?}"
    );
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-b"]);
}

/// The kept token's store compares only the envelope: a status changed
/// meanwhile doesn't lose the token, it only answers the request.
#[sqlx::test(migrations = false)]
async fn a_kept_token_is_stored_even_when_the_status_changed_just_before(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    let mut gate = setup.store.gate_next_write();

    let (answer, ()) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            setup.set_status(id, AccountStatus::Pending).await;
            gate.release();
        })
    })
    .await;

    assert!(
        matches!(
            answer,
            Err(SessionTokenError::NotActive {
                status: AccountStatus::Pending
            })
        ),
        "{answer:?}"
    );
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

#[sqlx::test(migrations = false)]
async fn a_kept_token_whose_account_is_deleted_just_before_its_store_is_discarded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    keep_refresh_1(&setup, id).await;
    let mut gate = setup.store.gate_next_write();

    let (answer, ()) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            setup.delete(id).await;
            gate.release();
        })
    })
    .await;
    setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "linked-again")
        .await;
    script(&setup, "refresh-2", "minecraft-2");
    let next = bounded(setup.service.session(id, false)).await.unwrap();

    assert!(
        matches!(answer, Err(SessionTokenError::NotFound)),
        "{answer:?}"
    );
    assert_eq!(next.token.expose_secret(), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "linked-again"]);
}
