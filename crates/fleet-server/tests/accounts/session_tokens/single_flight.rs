//! One refresh per account at a time: concurrent callers share it, callers
//! queued behind a failure don't call again, and a caller that's dropped or
//! times out never loses the rotated token. Other accounts don't wait.

use core::time::Duration;

use fleet_core::account::AccountStatus;
use fleet_server::app::session_tokens::SessionTokenError;
use fleet_server::ports::msauth::{Failure, RefreshError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::msauth::{Script, gated};
use crate::support::service::{PROFILE, Setup, T0, at, bounded, mc_session, ms_tokens, profile};

/// A wait bound short enough for real time.
const SHORT: Duration = Duration::from_millis(300);

async fn active(setup: &Setup) -> fleet_core::id::AccountId {
    setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "refresh-0")
        .await
}

fn script_minecraft(setup: &Setup) {
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        PROFILE,
        "AfkBot1",
    ))));
}

#[sqlx::test(migrations = false)]
async fn concurrent_requests_share_one_refresh(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    script_minecraft(&setup);

    let (a, b, c, ()) = bounded(async {
        tokio::join!(
            setup.service.session(id, false),
            setup.service.session(id, false),
            setup.service.session(id, false),
            async {
                gate.entered().await;
                gate.release(Ok(ms_tokens("access-1", "refresh-1")));
            }
        )
    })
    .await;

    for answer in [a, b, c] {
        assert_eq!(answer.unwrap().token.expose_secret(), "minecraft-1");
    }
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
    assert_eq!(setup.sessions_started_with(), ["access-1"]);
}

#[sqlx::test(migrations = false)]
async fn callers_queued_behind_a_failed_flight_back_off_without_calling(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);

    let (a, b, c, ()) = bounded(async {
        tokio::join!(
            setup.service.session(id, false),
            setup.service.session(id, false),
            setup.service.session(id, false),
            async {
                gate.entered().await;
                gate.release(Err(RefreshError::Failed(Failure::Unavailable {
                    status: 503,
                })));
            }
        )
    })
    .await;

    let mut answers = [a, b, c].map(|answer| answer.unwrap_err());
    answers.sort_by_key(|error| matches!(error, SessionTokenError::BackingOff { .. }));
    assert!(
        matches!(
            answers[0],
            SessionTokenError::Microsoft(Failure::Unavailable { status: 503 })
        ),
        "{answers:?}"
    );
    for queued in &answers[1..] {
        assert!(
            matches!(queued, SessionTokenError::BackingOff { until } if *until == at(T0 + 5_000)),
            "{answers:?}"
        );
    }
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

#[sqlx::test(migrations = false)]
async fn a_dropped_caller_still_stores_the_rotated_token_and_the_next_caller_gets_its_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    script_minecraft(&setup);
    let mut dropped = Box::pin(setup.service.session(id, false));

    bounded(async {
        tokio::select! {
            biased;
            answer = &mut dropped => panic!("answered before the refresh: {answer:?}"),
            () = gate.entered() => {}
        }
    })
    .await;
    drop(dropped);
    gate.release(Ok(ms_tokens("access-1", "refresh-1")));
    let next = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(next.token.expose_secret(), "minecraft-1");
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
    assert_eq!(setup.sessions_started_with(), ["access-1"]);
}

#[sqlx::test(migrations = false)]
async fn a_caller_past_the_wait_bound_times_out_while_its_flight_finishes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    script_minecraft(&setup);
    let short = setup.service.clone().with_wait_bound(SHORT).unwrap();

    let (timed_out, ()) = bounded(async {
        tokio::join!(short.session(id, false), async {
            gate.entered().await;
        })
    })
    .await;
    gate.release(Ok(ms_tokens("access-1", "refresh-1")));
    let next = bounded(setup.service.session(id, false)).await.unwrap();

    assert!(
        matches!(timed_out, Err(SessionTokenError::TimedOut)),
        "{timed_out:?}"
    );
    assert_eq!(next.token.expose_secret(), "minecraft-1");
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

#[sqlx::test(migrations = false)]
async fn a_caller_that_cant_get_the_flight_within_the_wait_bound_is_in_flight(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    script_minecraft(&setup);
    let short = setup.service.clone().with_wait_bound(SHORT).unwrap();

    let (first, waiting) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            let waiting = short.session(id, true).await;
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
            waiting
        })
    })
    .await;

    assert!(
        matches!(waiting, Err(SessionTokenError::InFlight)),
        "{waiting:?}"
    );
    assert_eq!(first.unwrap().token.expose_secret(), "minecraft-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

#[sqlx::test(migrations = false)]
async fn another_account_doesnt_wait_for_a_flight(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let held = setup
        .link(
            1,
            "AfkBot1",
            &profile(1),
            AccountStatus::Active,
            "refresh-a",
        )
        .await;
    let other = setup
        .link(
            2,
            "AfkBot2",
            &profile(2),
            AccountStatus::Active,
            "refresh-b",
        )
        .await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-2", "refresh-2"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-2",
        &profile(2),
        "AfkBot2",
    ))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        &profile(1),
        "AfkBot1",
    ))));

    let (first, second) = bounded(async {
        tokio::join!(setup.service.session(held, false), async {
            gate.entered().await;
            let second = setup.service.session(other, false).await;
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
            second
        })
    })
    .await;

    assert_eq!(second.unwrap().token.expose_secret(), "minecraft-2");
    assert_eq!(first.unwrap().token.expose_secret(), "minecraft-1");
    assert_eq!(setup.refreshed_with(), ["refresh-a", "refresh-b"]);
}
