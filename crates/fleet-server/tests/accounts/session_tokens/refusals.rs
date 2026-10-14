//! Who gets no session: only an active Microsoft account whose owner isn't
//! disabled gets one, and a refusal never asks Microsoft. A cached session
//! is withheld too once the account stopped being usable.

use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_server::app::session_tokens::SessionTokenError;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::msauth::Script;
use crate::support::service::{PROFILE, Setup, bounded, mc_session, ms_tokens, profile};
use fleet_server::ports::store::StoreError;

#[sqlx::test(migrations = false)]
async fn an_account_that_cant_be_used_is_refused_without_asking_microsoft(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let refused = AccountStatus::Refused(RefusalReason::NotOwned);
    let cases = [
        (
            setup
                .link(1, "Pending1", &profile(1), AccountStatus::Pending, "r-1")
                .await,
            "pending",
        ),
        (
            setup
                .link(
                    2,
                    "Reauth2",
                    &profile(2),
                    AccountStatus::ReauthRequired,
                    "r-2",
                )
                .await,
            "reauth_required",
        ),
        (
            setup.link(3, "Refused3", &profile(3), refused, "r-3").await,
            "refused",
        ),
        (setup.link_offline(4, "Offline4").await, "offline"),
        (Setup::account_id(9), "missing"),
    ];

    for (id, case) in cases {
        for fresh in [false, true] {
            let answer = bounded(setup.service.session(id, fresh)).await;

            let expected = match (case, &answer) {
                ("pending", Err(SessionTokenError::NotActive { status })) => {
                    *status == AccountStatus::Pending
                }
                ("reauth_required", Err(SessionTokenError::NotActive { status })) => {
                    *status == AccountStatus::ReauthRequired
                }
                ("refused", Err(SessionTokenError::NotActive { status })) => *status == refused,
                ("offline", Err(SessionTokenError::Offline))
                | ("missing", Err(SessionTokenError::NotFound)) => true,
                _ => false,
            };
            assert!(expected, "{case}, fresh {fresh}: {answer:?}");
        }
    }
    assert_eq!(setup.fake.calls(), []);
    assert_eq!(setup.server_entries().await, 0);
}

#[sqlx::test(migrations = false)]
async fn an_account_whose_owner_is_disabled_is_refused_without_asking_microsoft(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "refresh-0")
        .await;
    setup.set_disabled(setup.owner, true).await;

    let answer = bounded(setup.service.session(id, false)).await;

    assert!(
        matches!(answer, Err(SessionTokenError::OwnerDisabled)),
        "{answer:?}"
    );
    assert_eq!(setup.fake.calls(), []);
    assert_eq!(setup.status(id).await, AccountStatus::Active);
}

#[sqlx::test(migrations = false)]
async fn a_cached_session_is_withheld_once_the_account_or_its_owner_stops_being_usable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let first = setup
        .link(
            1,
            "AfkBot1",
            &profile(1),
            AccountStatus::Active,
            "refresh-a",
        )
        .await;
    let second = setup
        .link(
            2,
            "AfkBot2",
            &profile(2),
            AccountStatus::Active,
            "refresh-b",
        )
        .await;
    for (n, name) in [(1, "AfkBot1"), (2, "AfkBot2")] {
        setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
            &format!("access-{n}"),
            &format!("refresh-{n}"),
        ))));
        setup.fake.script_session(Script::Answer(Ok(mc_session(
            &format!("minecraft-{n}"),
            &profile(n),
            name,
        ))));
    }
    bounded(setup.service.session(first, false)).await.unwrap();
    bounded(setup.service.session(second, false)).await.unwrap();

    setup.set_status(first, AccountStatus::Pending).await;
    let pending = bounded(setup.service.session(first, false)).await;
    setup.set_disabled(setup.owner, true).await;
    let disabled = bounded(setup.service.session(second, false)).await;

    assert!(
        matches!(
            pending,
            Err(SessionTokenError::NotActive {
                status: AccountStatus::Pending
            })
        ),
        "{pending:?}"
    );
    assert!(
        matches!(disabled, Err(SessionTokenError::OwnerDisabled)),
        "{disabled:?}"
    );
    assert_eq!(setup.refreshed_with().len(), 2);
}

/// A database failure isn't a refusal: it hands out nothing, but the cached
/// session survives it.
#[sqlx::test(migrations = false)]
async fn a_database_failure_hands_out_nothing_but_keeps_the_cached_session(
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
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        PROFILE,
        "AfkBot1",
    ))));
    bounded(setup.service.session(id, false)).await.unwrap();

    setup.set_raw_status(id, "frozen").await;
    let failed = bounded(setup.service.session(id, false)).await;
    setup.set_raw_status(id, "active").await;
    let cached = bounded(setup.service.session(id, false)).await.unwrap();

    assert!(
        matches!(
            failed,
            Err(SessionTokenError::Store(StoreError::Corrupt { .. }))
        ),
        "{failed:?}"
    );
    assert_eq!(cached.token.expose_secret(), "minecraft-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

/// The flight reads the account again once it holds the lock: an account
/// that stopped being usable after the request's first read is refused
/// without a call.
#[sqlx::test(migrations = false)]
async fn a_flight_reads_the_account_again_and_refuses_what_changed_since_the_request_began(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let pending = setup
        .link(
            1,
            "AfkBot1",
            &profile(1),
            AccountStatus::Active,
            "refresh-a",
        )
        .await;
    let deleted = setup
        .link(
            2,
            "AfkBot2",
            &profile(2),
            AccountStatus::Active,
            "refresh-b",
        )
        .await;

    let mut answers = Vec::new();
    for id in [pending, deleted] {
        // The request's own read passes; the flight's waits.
        let mut gate = setup.store.gate_account_read_after(1);
        let (answer, ()) = bounded(async {
            tokio::join!(setup.service.session(id, false), async {
                gate.entered().await;
                if id == pending {
                    setup.set_status(id, AccountStatus::Pending).await;
                } else {
                    setup.delete(id).await;
                }
                gate.release();
            })
        })
        .await;
        answers.push(answer);
    }

    assert!(
        matches!(
            answers.as_slice(),
            [
                Err(SessionTokenError::NotActive {
                    status: AccountStatus::Pending
                }),
                Err(SessionTokenError::NotFound),
            ]
        ),
        "{answers:?}"
    );
    assert_eq!(setup.fake.calls(), []);
}
