//! The poller's waits and Microsoft's answers to its polls (ADR-0018):
//! one poll per interval, slower after `SlowDown` or a 429, and the code's
//! expiry on the clock.

use core::time::Duration;

use fleet_core::audit::AuditAction;
use fleet_core::authz::Role;
use fleet_server::app::link_flows::LinkFlowStatus;
use fleet_server::ports::audit::AuditRecord;
use fleet_server::ports::msauth::{Endpoint, Failure, PollOutcome, Rule};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{approval, profile_session};
use crate::support::link::{LinkSetup, USER_CODE, default_interval};
use crate::support::msauth::{Script, device_flow};
use crate::support::service::{T0, at, json_logs, levels};

const ENDED: &str = "a link flow ended";
const POLL_RETRY: &str = "a link flow's poll failed; it polls again after its interval";
const POLL_FAILED: &str = "a link flow's poll failed, and the flow ended";

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn poll(setup: &LinkSetup, answer: Result<PollOutcome, Failure>) {
    setup.fake.script_poll(Script::Answer(answer));
}

/// The waits since the first `from`.
fn waits_since(setup: &LinkSetup, from: usize) -> Vec<Duration> {
    setup.pace.waits().split_off(from)
}

#[sqlx::test(migrations = false)]
async fn pending_then_approved_links_after_one_interval_each(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    poll(&setup, Ok(PollOutcome::Pending));
    poll(&setup, Ok(approval(1)));
    setup
        .fake
        .script_session(Script::Answer(Ok(profile_session())));

    let flow = setup.started(&alice, "device-code-1").await;

    assert!(matches!(
        setup.ended(flow).await,
        LinkFlowStatus::Linked { .. }
    ));
    assert_eq!(setup.pace.waits(), [default_interval(), default_interval()]);
    assert_eq!(setup.polled_with(), ["device-code-1", "device-code-1"]);
    assert_eq!(default_interval(), secs(5));
}

#[sqlx::test(migrations = false)]
async fn a_slow_down_and_a_rate_limit_each_lengthen_the_interval_by_5_s(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    poll(&setup, Ok(PollOutcome::SlowDown));
    poll(&setup, Err(Failure::RateLimited));
    poll(&setup, Ok(PollOutcome::Pending));
    poll(&setup, Ok(PollOutcome::Declined));

    let flow = setup.started(&alice, "device-code-1").await;

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Declined);
    assert_eq!(setup.pace.waits(), [secs(5), secs(10), secs(15), secs(15)]);
}

#[sqlx::test(migrations = false)]
async fn a_passing_failure_waits_one_interval_and_polls_again(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let failures = [
        Failure::Unreachable,
        Failure::TimedOut,
        Failure::Unavailable { status: 502 },
    ];

    for (n, failure) in (1_u8..).zip(failures) {
        let user = setup.person(n, Role::Member).await;
        let from = setup.pace.waits().len();
        poll(&setup, Err(failure.clone()));
        poll(&setup, Ok(PollOutcome::Declined));

        let flow = setup.started(&user, &format!("device-code-{n}")).await;

        assert_eq!(
            setup.ended(flow).await,
            LinkFlowStatus::Declined,
            "{failure:?}"
        );
        assert_eq!(waits_since(&setup, from), [secs(5), secs(5)], "{failure:?}");
    }
    assert_eq!(levels(&logs, POLL_RETRY), ["DEBUG"; 3]);
}

#[sqlx::test(migrations = false)]
async fn a_failure_that_wont_pass_ends_the_flow_at_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let failures = [
        Failure::Certificate,
        Failure::Unexpected {
            endpoint: Endpoint::Poll,
            rule: Rule::NotJson,
        },
    ];

    for (n, failure) in (1_u8..).zip(failures) {
        let user = setup.person(n, Role::Member).await;
        let from = setup.pace.waits().len();
        poll(&setup, Err(failure.clone()));

        let flow = setup.started(&user, &format!("device-code-{n}")).await;

        assert_eq!(
            setup.ended(flow).await,
            LinkFlowStatus::Failed,
            "{failure:?}"
        );
        assert_eq!(waits_since(&setup, from), [secs(5)], "{failure:?}");
    }
    assert_eq!(levels(&logs, POLL_FAILED), ["ERROR"; 2]);
}

#[sqlx::test(migrations = false)]
async fn declined_and_expired_end_with_one_line_each_and_no_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = json_logs();
    let setup = LinkSetup::new(options).await;
    let answers = [
        (PollOutcome::Declined, LinkFlowStatus::Declined),
        (PollOutcome::Expired, LinkFlowStatus::Expired),
    ];

    let mut flows = Vec::new();
    for (n, (answer, status)) in (1_u8..).zip(answers) {
        let user = setup.person(n, Role::Member).await;
        poll(&setup, Ok(answer));

        let flow = setup.started(&user, &format!("device-code-{n}")).await;

        assert_eq!(setup.ended(flow).await, status);
        flows.push((flow, user, status));
    }
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_LINK).await,
        Vec::<AuditRecord>::new()
    );
    let ended = logs.lines_with(ENDED).unwrap();
    assert_eq!(ended.len(), 2);
    for (line, (flow, user, status)) in ended.iter().zip(flows) {
        assert_eq!(line["level"], "INFO");
        assert_eq!(line["outcome"], status.as_str());
        assert_eq!(line["flow_id"], flow.to_string());
        assert_eq!(line["user_id"], user.actor.id.to_string());
    }
}

#[sqlx::test(migrations = false)]
async fn the_codes_expiry_on_the_clock_ends_the_flow_without_another_poll(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup.script_flow(device_flow("device-code-1", USER_CODE, at(T0 + 12_000)));
    poll(&setup, Ok(PollOutcome::Pending));
    poll(&setup, Ok(PollOutcome::Pending));

    let flow = setup.start(&alice).await.unwrap().id;

    assert_eq!(setup.ended(flow).await, LinkFlowStatus::Expired);
    assert_eq!(setup.polled_with().len(), 2);
    assert_eq!(setup.pace.waits(), [secs(5), secs(5), secs(5)]);
}
