//! The service's life: the wait bound only shortens the wait, a shutdown
//! starts no new refresh and waits for the running one, a task that ends
//! without an answer is reported, `Debug` shows nothing secret, and no
//! token, name or UUID reaches any log on any path.

use core::time::Duration;

use fleet_core::account::AccountStatus;
use fleet_server::app::session_tokens::{SessionTokenError, WAIT_BOUND, WaitBoundError};
use fleet_server::ports::msauth::{RefreshError, SessionError};
use fleet_testkit::log_capture;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::msauth::{Script, gated};
use crate::support::service::{
    OTHER_PROFILE, PROFILE, Setup, bounded, mc_session, ms_tokens, poll_once, profile,
};

const INTERRUPTED: &str = "a session-token task ended without an answer";

async fn active(setup: &Setup) -> fleet_core::id::AccountId {
    setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "refresh-0")
        .await
}

fn script_session(setup: &Setup, n: u8) {
    setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
        &format!("access-{n}"),
        &format!("refresh-{n}"),
    ))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        &format!("minecraft-{n}"),
        PROFILE,
        "AfkBot1",
    ))));
}

#[sqlx::test(migrations = false)]
async fn a_wait_bound_may_only_shorten_the_wait(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let service = || setup.service.clone();

    let zero = service().with_wait_bound(Duration::ZERO);
    let longer = service().with_wait_bound(WAIT_BOUND + Duration::from_millis(1));
    let unbounded = service().with_wait_bound(Duration::MAX);
    let shortest = service().with_wait_bound(Duration::from_millis(1));
    let longest = service().with_wait_bound(WAIT_BOUND);

    assert_eq!(zero.err(), Some(WaitBoundError));
    assert_eq!(longer.err(), Some(WaitBoundError));
    assert_eq!(unbounded.err(), Some(WaitBoundError));
    assert!(shortest.is_ok());
    assert!(longest.is_ok());
}

#[sqlx::test(migrations = false)]
async fn after_a_shutdown_only_a_cached_session_is_handed_out(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    script_session(&setup, 1);
    bounded(setup.service.session(id, false)).await.unwrap();

    bounded(setup.service.shutdown()).await;
    let cached = bounded(setup.service.session(id, false)).await.unwrap();
    let fresh = bounded(setup.service.session(id, true)).await;

    assert_eq!(cached.token.expose_secret(), "minecraft-1");
    assert!(
        matches!(fresh, Err(SessionTokenError::ShuttingDown)),
        "{fresh:?}"
    );
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

#[sqlx::test(migrations = false)]
async fn a_shutdown_waits_for_the_running_flight_which_stores_its_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        PROFILE,
        "AfkBot1",
    ))));

    let (answer, waited) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            let mut shutdown = Box::pin(setup.service.shutdown());
            let early = poll_once(&mut shutdown).await.is_some();
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
            shutdown.await;
            early
        })
    })
    .await;

    assert!(!waited, "the shutdown didn't wait for the flight");
    assert_eq!(answer.unwrap().token.expose_secret(), "minecraft-1");
    assert_eq!(setup.stored_token(id).await, "refresh-1");
}

#[sqlx::test(migrations = false)]
async fn a_caller_waiting_for_the_flight_is_refused_at_the_stop(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        PROFILE,
        "AfkBot1",
    ))));

    let (first, waiting) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            let mut waiting = Box::pin(setup.service.session(id, true));
            assert!(poll_once(&mut waiting).await.is_none());
            setup.stop.cancel();
            let answer = waiting.await;
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
            answer
        })
    })
    .await;

    assert!(
        matches!(waiting, Err(SessionTokenError::ShuttingDown)),
        "{waiting:?}"
    );
    assert_eq!(first.unwrap().token.expose_secret(), "minecraft-1");
    assert_eq!(setup.stored_token(id).await, "refresh-1");
}

#[sqlx::test(migrations = false)]
async fn a_task_that_ends_without_an_answer_is_interrupted_and_frees_the_flight(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (logs, guard) = crate::support::service::json_logs();

    // Nothing scripted: the fake panics inside the task.
    let interrupted = bounded(setup.service.session(id, false)).await;
    script_session(&setup, 1);
    let next = bounded(setup.service.session(id, false)).await;

    drop(guard);
    assert!(
        matches!(interrupted, Err(SessionTokenError::Interrupted)),
        "{interrupted:?}"
    );
    assert_eq!(next.unwrap().token.expose_secret(), "minecraft-1");
    assert_eq!(
        crate::support::service::levels(&logs, INTERRUPTED),
        ["ERROR"],
        "{}",
        logs.text()
    );
}

#[sqlx::test(migrations = false)]
async fn the_service_debugs_without_a_token_a_name_or_a_uuid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    script_session(&setup, 1);
    bounded(setup.service.session(id, false)).await.unwrap();

    let debug = format!("{:?} {:#?}", setup.service, setup.service);

    assert!(debug.contains("SessionTokenService"), "{debug}");
    for secret in ["minecraft-1", "refresh-1", "access-1", "AfkBot1", PROFILE] {
        assert!(!debug.contains(secret), "{secret} in {debug}");
    }
}

/// Every path that handles a secret, with a capture of every log line at
/// every level from every target: none of the invented tokens, names or
/// UUIDs shows up. The count of the flight's lines proves the capture works;
/// a leak was shown red by a `debug` line with the token (ADR-0018, group
/// C).
#[sqlx::test(migrations = false)]
async fn no_token_name_or_uuid_reaches_any_log_on_any_path(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = Setup::new(options).await;
    let renamed = setup
        .link(
            1,
            "OldName_1",
            &profile(1),
            AccountStatus::Active,
            "refresh-0-a",
        )
        .await;
    let reauth = setup
        .link(
            2,
            "Reauth_2",
            &profile(2),
            AccountStatus::Active,
            "refresh-0-b",
        )
        .await;
    let refused = setup
        .link(
            3,
            "Refused_3",
            &profile(3),
            AccountStatus::Active,
            "refresh-0-c",
        )
        .await;
    let mismatch = setup
        .link(
            4,
            "Mismatch_4",
            PROFILE,
            AccountStatus::Active,
            "refresh-0-d",
        )
        .await;
    let kept = setup
        .link(
            5,
            "Kept_5",
            &profile(5),
            AccountStatus::Active,
            "refresh-0-e",
        )
        .await;
    let bad = setup
        .link_with_envelope(
            6,
            "Bad_6",
            &profile(6),
            AccountStatus::Active,
            setup.seal(Setup::account_id(6), b"bad token-text"),
        )
        .await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1-a", "refresh-1-a"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1-a",
        &profile(1),
        "NewName_1",
    ))));
    setup
        .fake
        .script_refresh(Script::Answer(Err(RefreshError::InvalidGrant)));
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1-c", "refresh-1-c"))));
    setup
        .fake
        .script_session(Script::Answer(Err(SessionError::NotOwned)));
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1-d", "refresh-1-d"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1-d",
        OTHER_PROFILE,
        "Mismatch_4",
    ))));
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1-e", "refresh-1-e"))));
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-2-e", "refresh-2-e"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-2-e",
        &profile(5),
        "Kept_5",
    ))));

    bounded(setup.service.session(renamed, false))
        .await
        .unwrap();
    bounded(setup.service.session(reauth, false))
        .await
        .unwrap_err();
    bounded(setup.service.session(refused, false))
        .await
        .unwrap_err();
    bounded(setup.service.session(mismatch, false))
        .await
        .unwrap_err();
    setup.store.fail_next_writes(1);
    bounded(setup.service.session(kept, false))
        .await
        .unwrap_err();
    bounded(setup.service.session(kept, false)).await.unwrap();
    bounded(setup.service.session(bad, false))
        .await
        .unwrap_err();

    assert!(log_capture::count("fleet_server::app::session_tokens::flight", "account_id") >= 5);
    let mut secrets = vec![
        "refresh-0-a",
        "refresh-1-a",
        "access-1-a",
        "minecraft-1-a",
        "refresh-0-b",
        "refresh-0-c",
        "refresh-1-c",
        "access-1-c",
        "refresh-0-d",
        "refresh-1-d",
        "access-1-d",
        "minecraft-1-d",
        "refresh-0-e",
        "refresh-1-e",
        "access-1-e",
        "refresh-2-e",
        "access-2-e",
        "minecraft-2-e",
        "token-text",
        "OldName_1",
        "NewName_1",
        "Reauth_2",
        "Refused_3",
        "Mismatch_4",
        "Kept_5",
        "Bad_6",
    ];
    let uuids: Vec<String> = [
        profile(1),
        profile(3),
        profile(5),
        PROFILE.to_owned(),
        OTHER_PROFILE.to_owned(),
    ]
    .into_iter()
    .flat_map(|uuid| [uuid.replace('-', ""), uuid])
    .collect();
    secrets.extend(uuids.iter().map(String::as_str));
    for secret in secrets {
        log_capture::check_absent(secret).unwrap();
    }
}

/// The stop is checked again right before a refresh is sent: a flight that
/// stores a kept token meanwhile stores it, then sends nothing.
#[sqlx::test(migrations = false)]
async fn a_flight_stopped_while_it_stores_a_kept_token_sends_no_refresh(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-1"))));
    setup.store.fail_next_writes(1);
    bounded(setup.service.session(id, false)).await.unwrap_err();
    let mut gate = setup.store.gate_next_write();

    let (answer, ()) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            setup.stop.cancel();
            gate.release();
        })
    })
    .await;

    assert!(
        matches!(answer, Err(SessionTokenError::ShuttingDown)),
        "{answer:?}"
    );
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}
