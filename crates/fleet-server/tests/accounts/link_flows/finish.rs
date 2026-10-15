//! After Microsoft's approval (ADR-0018): the Minecraft session checks the
//! game and the profile, and a passing failure, of Microsoft or of the
//! database, doesn't throw the approval away for 5 minutes.

use core::time::Duration;

use fleet_core::account::RefusalReason;
use fleet_core::audit::AuditAction;
use fleet_core::authz::Role;
use fleet_server::app::link_flows::{FINISH_BACKOFF, FINISH_WINDOW, LinkFlowStatus};
use fleet_server::ports::audit::AuditRecord;
use fleet_server::ports::msauth::{
    Endpoint, Failure, PollOutcome, Rule, SessionError, XboxRefusal,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{approval, profile_session};
use crate::support::link::LinkSetup;
use crate::support::msauth::{Script, session, tokens};
use crate::support::service::{DAY, T0, at, json_logs, levels, profile};

const SESSION_RETRY: &str = "a link flow couldn't start the Minecraft session; it tries again";
const SESSION_FAILED: &str = "a link flow couldn't start the Minecraft session, and the flow ended";
const BUSY: &str = "the database was busy storing a link flow's account; the flow tries again";
const COMMIT_FAILED: &str = "a link flow couldn't store the account, and the flow ended";

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn session_fails(setup: &LinkSetup, failure: &Failure, times: usize) {
    for _ in 0..times {
        setup
            .fake
            .script_session(Script::Answer(Err(SessionError::Failed(failure.clone()))));
    }
}

#[test]
fn the_window_is_5_minutes_and_the_backoff_5_s_to_a_minute() {
    assert_eq!(FINISH_WINDOW, Duration::from_mins(5));
    assert_eq!(FINISH_BACKOFF.first(), secs(5));
    assert_eq!(FINISH_BACKOFF.max(), Duration::from_mins(1));
}

#[sqlx::test(migrations = false)]
async fn passing_session_failures_are_retried_with_the_backoff_then_link(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let failures = [
        Failure::Unreachable,
        Failure::TimedOut,
        Failure::RateLimited,
        Failure::Unavailable { status: 503 },
    ];

    for (n, failure) in (1_u8..).zip(failures) {
        let user = setup.person(n, Role::Member).await;
        let from = setup.pace.waits().len();
        let sessions = setup.sessions_started();
        setup.fake.script_poll(Script::Answer(Ok(approval(n))));
        session_fails(&setup, &failure, 4);
        setup.fake.script_session(Script::Answer(Ok(session(
            &format!("minecraft-{n}"),
            &profile(n),
            &format!("AfkBot{n}"),
            at(T0 + DAY),
        ))));

        let flow = setup.started(&user, &format!("device-code-{n}")).await;

        assert!(
            matches!(setup.ended(flow).await, LinkFlowStatus::Linked { .. }),
            "{failure:?}"
        );
        assert_eq!(
            setup.pace.waits().split_off(from),
            [secs(5), secs(5), secs(10), secs(20), secs(40)],
            "{failure:?}"
        );
        assert_eq!(setup.sessions_started() - sessions, 5, "{failure:?}");
    }
    assert_eq!(levels(&logs, SESSION_RETRY), ["WARN"; 16]);
}

#[sqlx::test(migrations = false)]
async fn retries_stop_5_minutes_after_the_approval(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    session_fails(&setup, &Failure::TimedOut, 8);

    let flow = setup.started(&alice, "device-code-1").await;

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Failed);
    // 5 + 10 + 20 + 40 + 60 + 60 + 60 = 255 s; one more minute would end at
    // 315 s, past the window.
    assert_eq!(
        setup.pace.waits(),
        [
            secs(5),
            secs(5),
            secs(10),
            secs(20),
            secs(40),
            secs(60),
            secs(60),
            secs(60)
        ]
    );
    assert_eq!(setup.sessions_started(), 8);
    assert_eq!(levels(&logs, SESSION_FAILED), ["ERROR"]);
    assert_eq!(setup.owned(alice.actor.id).await, 0);
}

#[sqlx::test(migrations = false)]
async fn retries_never_pass_the_access_tokens_expiry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    // Approved after the first 5 s wait; the access token lives 30 s more.
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Authorized(tokens(
            "access-1",
            "refresh-1",
            at(T0 + 35_000),
        )))));
    session_fails(&setup, &Failure::Unavailable { status: 503 }, 3);

    let flow = setup.started(&alice, "device-code-1").await;

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Failed);
    assert_eq!(setup.pace.waits(), [secs(5), secs(5), secs(10)]);
    assert_eq!(setup.sessions_started(), 3);
}

#[sqlx::test(migrations = false)]
async fn a_session_failure_that_wont_pass_ends_the_flow_at_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let failures = [
        Failure::Certificate,
        Failure::Unexpected {
            endpoint: Endpoint::Profile,
            rule: Rule::NotJson,
        },
    ];

    for (n, failure) in (1_u8..).zip(failures) {
        let user = setup.person(n, Role::Member).await;
        setup.fake.script_poll(Script::Answer(Ok(approval(n))));
        session_fails(&setup, &failure, 1);

        let flow = setup.started(&user, &format!("device-code-{n}")).await;

        assert_eq!(
            setup.ended(flow).await,
            LinkFlowStatus::Failed,
            "{failure:?}"
        );
    }
    assert_eq!(setup.sessions_started(), 2);
    assert_eq!(levels(&logs, SESSION_FAILED), ["ERROR"; 2]);
    assert_eq!(levels(&logs, SESSION_RETRY), Vec::<String>::new());
}

#[sqlx::test(migrations = false)]
async fn every_minecraft_refusal_ends_refused_with_its_reason(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let refusals = [
        (SessionError::NotOwned, RefusalReason::NotOwned),
        (SessionError::NoProfile, RefusalReason::NoProfile),
        (
            SessionError::Xbox(XboxRefusal::Banned),
            RefusalReason::XboxBanned,
        ),
        (
            SessionError::Xbox(XboxRefusal::NoXboxProfile),
            RefusalReason::XboxNoXboxProfile,
        ),
        (
            SessionError::Xbox(XboxRefusal::CountryUnavailable),
            RefusalReason::XboxCountryUnavailable,
        ),
        (
            SessionError::Xbox(XboxRefusal::AdultVerification),
            RefusalReason::XboxAdultVerification,
        ),
        (
            SessionError::Xbox(XboxRefusal::ChildAccount),
            RefusalReason::XboxChildAccount,
        ),
        (
            SessionError::Xbox(XboxRefusal::Other {
                xerr: 2_148_916_262,
            }),
            RefusalReason::XboxOther,
        ),
    ];

    for (n, (refusal, reason)) in (1_u8..).zip(refusals) {
        let user = setup.person(n, Role::Member).await;
        setup.fake.script_poll(Script::Answer(Ok(approval(n))));
        setup.fake.script_session(Script::Answer(Err(refusal)));

        let flow = setup.started(&user, &format!("device-code-{n}")).await;

        assert_eq!(setup.ended(flow).await, LinkFlowStatus::Refused(reason));
        assert_eq!(setup.owned(user.actor.id).await, 0);
    }
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_LINK).await,
        Vec::<AuditRecord>::new()
    );
}

#[sqlx::test(migrations = false)]
async fn a_busy_database_at_the_commit_is_retried(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup
        .fake
        .script_session(Script::Answer(Ok(profile_session())));
    let mut wait = setup.pace.hold_next();
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    setup.store.fail_next_writes(2);

    wait.release();

    assert!(matches!(
        setup.ended(flow).await,
        LinkFlowStatus::Linked { .. }
    ));
    assert_eq!(setup.pace.waits(), [secs(5), secs(5), secs(10)]);
    assert_eq!(levels(&logs, BUSY), ["WARN"; 2]);
    assert_eq!(setup.owned(alice.actor.id).await, 1);
}

#[sqlx::test(migrations = false)]
async fn a_database_busy_past_the_window_fails_the_flow(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup
        .fake
        .script_session(Script::Answer(Ok(profile_session())));
    let mut wait = setup.pace.hold_next();
    let flow = setup.started(&alice, "device-code-1").await;
    wait.entered().await;
    setup.store.fail_next_writes(100);

    wait.release();

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Failed);
    assert_eq!(levels(&logs, COMMIT_FAILED), ["ERROR"]);
    assert_eq!(levels(&logs, BUSY).len(), 7);
    assert_eq!(setup.owned(alice.actor.id).await, 0);
}
