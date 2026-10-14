//! The backoff, on the manual clock: a failed call backs the account off
//! (5 s, doubling to 5 minutes), a 429 pauses every account (30 s, doubling
//! to 10 minutes), and meanwhile only a request that needs Microsoft is
//! refused: a cached session is still handed out.

use core::time::Duration;

use fleet_core::account::AccountStatus;
use fleet_core::id::AccountId;
use fleet_core::system::Clock as _;
use fleet_server::app::session_tokens::SessionTokenError;
use fleet_server::ports::msauth::{Failure, RefreshError, SessionError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::msauth::{Script, gated};
use crate::support::service::{PROFILE, Setup, bounded, mc_session, ms_tokens, profile};

fn unavailable() -> Script<Result<fleet_server::ports::msauth::MicrosoftTokens, RefreshError>> {
    Script::Answer(Err(RefreshError::Failed(Failure::Unavailable {
        status: 503,
    })))
}

fn rate_limited() -> Script<Result<fleet_server::ports::msauth::MicrosoftTokens, RefreshError>> {
    Script::Answer(Err(RefreshError::Failed(Failure::RateLimited)))
}

/// Scripts one good refresh and session for account `n` of `profile(n)`.
fn script_good(setup: &Setup, n: u8) {
    setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
        &format!("access-{n}"),
        &format!("refresh-{n}"),
    ))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        &format!("minecraft-{n}"),
        &profile(n),
        &format!("AfkBot{n}"),
    ))));
}

async fn link(setup: &Setup, n: u8) -> AccountId {
    setup
        .link(
            n,
            &format!("AfkBot{n}"),
            &profile(n),
            AccountStatus::Active,
            &format!("refresh-{n}0"),
        )
        .await
}

/// Asserts `answer` is a backoff until `until`.
fn assert_backs_off(
    answer: &Result<fleet_server::ports::msauth::MinecraftSession, SessionTokenError>,
    until: chrono::DateTime<chrono::Utc>,
) {
    assert!(
        matches!(answer, Err(SessionTokenError::BackingOff { until: u }) if *u == until),
        "expected a backoff until {until}: {answer:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn failures_back_the_account_off_doubling_to_five_minutes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = link(&setup, 1).await;
    let delays = [5, 10, 20, 40, 80, 160, 300, 300];

    for (calls, secs) in (1..).zip(delays) {
        setup.fake.script_refresh(unavailable());
        let failed = bounded(setup.service.session(id, false)).await;
        let until = fleet_core::time::add(setup.clock.now(), Duration::from_secs(secs));
        setup.clock.advance(Duration::from_millis(secs * 1_000 - 1));
        let waiting = bounded(setup.service.session(id, false)).await;
        setup.clock.advance(Duration::from_millis(1));

        assert!(
            matches!(
                failed,
                Err(SessionTokenError::Microsoft(Failure::Unavailable {
                    status: 503
                }))
            ),
            "{failed:?}"
        );
        assert_backs_off(&waiting, until);
        assert_eq!(setup.refreshed_with().len(), calls);
    }
}

#[sqlx::test(migrations = false)]
async fn a_failed_session_backs_the_account_off_too(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "refresh-0")
        .await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-1"))));
    setup
        .fake
        .script_session(Script::Answer(Err(SessionError::Failed(Failure::TimedOut))));
    let start = setup.clock.now();

    let failed = bounded(setup.service.session(id, false)).await;
    let waiting = bounded(setup.service.session(id, false)).await;

    assert!(
        matches!(failed, Err(SessionTokenError::Microsoft(Failure::TimedOut))),
        "{failed:?}"
    );
    assert_backs_off(
        &waiting,
        fleet_core::time::add(start, Duration::from_secs(5)),
    );
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.status(id).await, AccountStatus::Active);
}

#[sqlx::test(migrations = false)]
async fn a_session_ends_the_accounts_backoff(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = link(&setup, 1).await;
    setup.fake.script_refresh(unavailable());
    bounded(setup.service.session(id, false)).await.unwrap_err();
    setup.clock.advance(Duration::from_secs(5));
    script_good(&setup, 1);
    bounded(setup.service.session(id, false)).await.unwrap();
    setup.fake.script_refresh(unavailable());

    let failed = bounded(setup.service.session(id, true)).await;
    let waiting = bounded(setup.service.session(id, true)).await;

    assert!(failed.is_err());
    assert_backs_off(
        &waiting,
        fleet_core::time::add(setup.clock.now(), Duration::from_secs(5)),
    );
}

#[sqlx::test(migrations = false)]
async fn a_429_pauses_every_account_doubling_to_ten_minutes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let limited = link(&setup, 1).await;
    let other = link(&setup, 2).await;
    let delays = [30, 60, 120, 240, 480, 600, 600];

    for (calls, secs) in (1..).zip(delays) {
        setup.fake.script_refresh(rate_limited());
        let failed = bounded(setup.service.session(limited, false)).await;
        let until = fleet_core::time::add(setup.clock.now(), Duration::from_secs(secs));
        let paused = bounded(setup.service.session(other, false)).await;
        setup.clock.advance(Duration::from_secs(secs));

        assert!(
            matches!(
                failed,
                Err(SessionTokenError::Microsoft(Failure::RateLimited))
            ),
            "{failed:?}"
        );
        assert_backs_off(&paused, until);
        assert_eq!(setup.refreshed_with().len(), calls);
    }
}

#[sqlx::test(migrations = false)]
async fn during_a_global_pause_another_account_still_gets_its_cached_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let limited = link(&setup, 1).await;
    let cached = link(&setup, 2).await;
    script_good(&setup, 2);
    bounded(setup.service.session(cached, false)).await.unwrap();
    setup.fake.script_refresh(rate_limited());
    bounded(setup.service.session(limited, false))
        .await
        .unwrap_err();
    let calls = setup.fake.calls().len();

    let still_cached = bounded(setup.service.session(cached, false)).await.unwrap();
    let fresh = bounded(setup.service.session(cached, true)).await;

    assert_eq!(still_cached.token.expose_secret(), "minecraft-2");
    assert_backs_off(
        &fresh,
        fleet_core::time::add(setup.clock.now(), Duration::from_secs(30)),
    );
    assert_eq!(setup.fake.calls().len(), calls);
}

#[sqlx::test(migrations = false)]
async fn during_its_own_backoff_an_account_gets_its_cached_session_but_not_a_fresh_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = link(&setup, 1).await;
    script_good(&setup, 1);
    bounded(setup.service.session(id, false)).await.unwrap();
    setup.fake.script_refresh(unavailable());
    bounded(setup.service.session(id, true)).await.unwrap_err();

    let cached = bounded(setup.service.session(id, false)).await.unwrap();
    let fresh = bounded(setup.service.session(id, true)).await;

    assert_eq!(cached.token.expose_secret(), "minecraft-1");
    assert_backs_off(
        &fresh,
        fleet_core::time::add(setup.clock.now(), Duration::from_secs(5)),
    );
    assert_eq!(setup.refreshed_with().len(), 2);
}

#[sqlx::test(migrations = false)]
async fn a_session_during_a_pause_starts_the_pauses_run_over_but_lets_it_run_out(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let succeeding = link(&setup, 1).await;
    let limited = link(&setup, 2).await;
    let waiting = link(&setup, 3).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    setup.fake.script_refresh(rate_limited());
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        &profile(1),
        "AfkBot1",
    ))));
    let start = setup.clock.now();

    let (succeeded, limited_answer) = bounded(async {
        tokio::join!(setup.service.session(succeeding, false), async {
            gate.entered().await;
            let answer = setup.service.session(limited, false).await;
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
            answer
        })
    })
    .await;
    let still_paused = bounded(setup.service.session(waiting, false)).await;
    setup.clock.advance(Duration::from_secs(30));
    setup.fake.script_refresh(rate_limited());
    bounded(setup.service.session(waiting, false))
        .await
        .unwrap_err();
    let restarted = bounded(setup.service.session(limited, true)).await;

    succeeded.unwrap();
    assert!(limited_answer.is_err());
    assert_backs_off(
        &still_paused,
        fleet_core::time::add(start, Duration::from_secs(30)),
    );
    assert_backs_off(
        &restarted,
        fleet_core::time::add(setup.clock.now(), Duration::from_secs(30)),
    );
}
