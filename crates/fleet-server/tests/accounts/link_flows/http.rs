//! The link routes (Plan.md P9.4, Appendix B, ADR-0018), through serve's
//! own app and the real router: what each answers, who may read a flow,
//! every refusal with its `Retry-After`, the re-checks through the real
//! logout, session and refresh routes, and serve stopping the flows.

use core::num::NonZeroU32;
use core::time::Duration;

use axum::http::Method;
use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, LinkFlowId};
use fleet_server::app::link_flows::LinkFlowStatus;
use fleet_server::cli::serve::{Exit, ShutdownTimeouts, serve};
use fleet_server::config::{AccountsConfig, LinkFlowLimits, Rate, RateLimitConfig};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::msauth::{Failure, PollOutcome, SessionError, XboxRefusal};
use fleet_server::ports::store::Store as _;
use fleet_startup::signals::{ShutdownSignals, Signal};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::{approval, profile_session};
use crate::support::http::{HttpSetup, Person, Seen};
use crate::support::link::USER_CODE;
use crate::support::msauth::{Call, Script, device_flow, gated};
use crate::support::pace::PaceGate;
use crate::support::service::{PROFILE, T0, at, bounded, setup_entry};

/// The flow a start answered.
fn flow_of(seen: &Seen) -> LinkFlowId {
    assert_eq!(seen.status, 201, "{}", seen.text);
    seen.body["flow_id"].as_str().unwrap().parse().unwrap()
}

/// `body` with the flow's and the account's IDs and the user code replaced,
/// for a snapshot.
fn redacted(body: &Value) -> Value {
    let mut body = body.clone();
    for (field, stand_in) in [
        ("flow_id", "[flow]"),
        ("account_id", "[account]"),
        ("user_code", "[user code]"),
    ] {
        if body.get(field).is_some() {
            body[field] = json!(stand_in);
        }
    }
    body
}

/// Scripts the next start; its code expires 15 minutes after T0.
fn script_start(setup: &HttpSetup, code: &str) {
    setup.fake.script_start(Script::Answer(Ok(device_flow(
        code,
        USER_CODE,
        at(T0 + 900_000),
    ))));
}

/// The flow `id`'s status once it ended; fails the test after 10 seconds.
async fn ended(setup: &HttpSetup, id: LinkFlowId) -> LinkFlowStatus {
    bounded(async {
        loop {
            match setup.link_flows.status(id).map(|view| view.status) {
                Some(LinkFlowStatus::Waiting { .. } | LinkFlowStatus::Finishing) | None => {}
                Some(status) => return status,
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

/// Starts a flow for `by` whose poller waits forever.
async fn held(setup: &HttpSetup, by: &Person, code: &str) -> LinkFlowId {
    let mut wait = setup.pace.hold_next();
    script_start(setup, code);
    let flow = flow_of(&setup.start(by).await);
    // Once the poller is in that wait, no later flow's wait can take the hold.
    wait.entered().await;
    flow
}

/// Adds account `n` of `owner` for `profile`, active.
async fn add_account(setup: &HttpSetup, owner: &Person, n: u8, profile: &str) {
    let mut tx = setup.db.write().await.unwrap();
    tx.accounts()
        .insert(&McAccount {
            id: AccountId::new_v7(at(T0), [n; 10]).unwrap(),
            owner: owner.id,
            name: format!("Owned{n}").parse().unwrap(),
            created_at: at(T0),
            credential: AccountCredential::Microsoft {
                uuid: profile.parse().unwrap(),
                refresh_envelope: vec![n; 61],
                status: AccountStatus::Active,
            },
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

fn limits(total: u32, per_day: u32) -> RateLimitConfig {
    RateLimitConfig {
        link_flows: LinkFlowLimits {
            total: NonZeroU32::new(total).unwrap(),
            per_user_per_day: NonZeroU32::new(per_day).unwrap(),
        },
        ..RateLimitConfig::default()
    }
}

fn quota(max: u32) -> AccountsConfig {
    AccountsConfig {
        max_per_member: NonZeroU32::new(max).unwrap(),
        allow_offline: false,
    }
}

// --- What the routes answer -------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_start_answers_201_with_the_code_and_the_flow_reads_waiting(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    drop(setup.pace.hold_next());
    script_start(&setup, "device-code-1");

    let started = setup.start(&alice).await;
    let flow = flow_of(&started);
    let waiting = setup.status(&alice, &flow.to_string()).await;

    assert_eq!(started.body["user_code"], USER_CODE);
    insta::assert_json_snapshot!("link_started", redacted(&started.body));
    assert_eq!(waiting.status, 200);
    insta::assert_json_snapshot!("link_waiting", redacted(&waiting.body));
    assert!(!waiting.text.contains(USER_CODE), "{}", waiting.text);
}

#[sqlx::test(migrations = false)]
async fn the_flow_reads_finishing_then_linked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (session, mut gate) = gated();
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup.fake.script_session(session);
    script_start(&setup, "device-code-1");
    let flow = flow_of(&setup.start(&alice).await);
    gate.entered().await;

    let finishing = setup.status(&alice, &flow.to_string()).await;
    gate.release(Ok(profile_session()));
    assert!(matches!(
        ended(&setup, flow).await,
        LinkFlowStatus::Linked { .. }
    ));
    let linked = setup.status(&alice, &flow.to_string()).await;

    insta::assert_json_snapshot!("link_finishing", redacted(&finishing.body));
    insta::assert_json_snapshot!("link_linked", redacted(&linked.body));
    assert!(!linked.text.contains(USER_CODE), "{}", linked.text);
}

#[sqlx::test(migrations = false)]
async fn a_refused_flow_reads_its_reason(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup
        .fake
        .script_session(Script::Answer(Err(SessionError::Xbox(
            XboxRefusal::ChildAccount,
        ))));
    script_start(&setup, "device-code-1");
    let flow = flow_of(&setup.start(&alice).await);
    assert_eq!(
        ended(&setup, flow).await,
        LinkFlowStatus::Refused(RefusalReason::XboxChildAccount)
    );

    let refused = setup.status(&alice, &flow.to_string()).await;

    insta::assert_json_snapshot!("link_refused", redacted(&refused.body));
}

#[sqlx::test(migrations = false)]
async fn every_other_end_reads_its_state_alone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, RateLimitConfig::default(), quota(1)).await;
    let owner = setup.person(9, Role::Owner).await;
    add_account(&setup, &owner, 9, PROFILE).await;

    // Declined, expired and failed: Microsoft's answers to the poll.
    for (n, answer, state) in [
        (1_u8, Ok(PollOutcome::Declined), "declined"),
        (2, Ok(PollOutcome::Expired), "expired"),
        (3, Err(Failure::Certificate), "failed"),
    ] {
        let user = setup.person(n, Role::Member).await;
        setup.fake.script_poll(Script::Answer(answer));
        script_start(&setup, &format!("device-code-{n}"));
        let flow = flow_of(&setup.start(&user).await);
        ended(&setup, flow).await;

        let seen = setup.status(&user, &flow.to_string()).await;

        assert_eq!(seen.body, json!({ "status": state }));
    }
    // Taken: the Owner linked the profile already.
    let taker = setup.person(4, Role::Member).await;
    setup.fake.script_poll(Script::Answer(Ok(approval(4))));
    setup
        .fake
        .script_session(Script::Answer(Ok(profile_session())));
    script_start(&setup, "device-code-4");
    let flow = flow_of(&setup.start(&taker).await);
    assert_eq!(ended(&setup, flow).await, LinkFlowStatus::Taken);
    let taken = setup.status(&taker, &flow.to_string()).await;
    assert_eq!(taken.body, json!({ "status": "taken" }));
    // Over the quota: an account arrived while the flow waited.
    let late = setup.person(5, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    setup.fake.script_poll(Script::Answer(Ok(approval(5))));
    setup
        .fake
        .script_session(Script::Answer(Ok(profile_session())));
    script_start(&setup, "device-code-5");
    let flow = flow_of(&setup.start(&late).await);
    wait.entered().await;
    add_account(&setup, &late, 5, "0a0b0c0d-0e0f-4a1b-8c2d-3e4f5a6b7c8d").await;
    wait.release();
    assert_eq!(ended(&setup, flow).await, LinkFlowStatus::OverQuota);
    let over = setup.status(&late, &flow.to_string()).await;
    assert_eq!(over.body, json!({ "status": "over_quota" }));
}

#[sqlx::test(migrations = false)]
async fn every_flow_the_caller_cant_read_is_the_same_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let owner = setup.person(2, Role::Owner).await;
    let replaced = held(&setup, &alice, "device-code-1").await;
    let current = held(&setup, &alice, "device-code-2").await;
    let finished = {
        let bob = setup.person(3, Role::Member).await;
        setup
            .fake
            .script_poll(Script::Answer(Ok(PollOutcome::Declined)));
        script_start(&setup, "device-code-3");
        let flow = flow_of(&setup.start(&bob).await);
        ended(&setup, flow).await;
        (bob, flow)
    };
    let unknown = LinkFlowId::new_v7(at(T0), [7; 10]).unwrap();

    let answers = [
        setup.status(&owner, &current.to_string()).await,
        setup.status(&alice, &unknown.to_string()).await,
        setup.status(&alice, "not-a-flow").await,
        setup.status(&alice, &replaced.to_string()).await,
    ];
    setup.clock.advance(Duration::from_mins(15));
    // The access tokens expired with it: a fresh session reads.
    let bob = setup.session_of(finished.0.id, Role::Member).await;
    let alice = setup.session_of(alice.id, Role::Member).await;
    let purged = setup.status(&bob, &finished.1.to_string()).await;

    for seen in answers.iter().chain([&purged]) {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(
            seen.body_without_request_id(),
            answers[0].body_without_request_id()
        );
    }
    assert_eq!(setup.status(&alice, &current.to_string()).await.status, 200);
}

// --- The refusals ------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_member_at_the_quota_gets_409_without_asking_microsoft(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, RateLimitConfig::default(), quota(1)).await;
    let alice = setup.person(1, Role::Member).await;
    add_account(&setup, &alice, 1, PROFILE).await;

    let refused = setup.start(&alice).await;

    assert_eq!(refused.status, 409, "{}", refused.text);
    assert_eq!(refused.code(), "conflict");
    assert_eq!(setup.fake.calls(), Vec::<Call>::new());
}

#[sqlx::test(migrations = false)]
async fn the_daily_cap_answers_429_until_a_start_is_a_day_old(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, limits(3, 1), AccountsConfig::default()).await;
    let alice = setup.person(1, Role::Member).await;
    held(&setup, &alice, "device-code-1").await;

    let refused = setup.start(&alice).await;

    assert_eq!(refused.status, 429, "{}", refused.text);
    assert_eq!(refused.code(), "rate_limited");
    assert_eq!(refused.retry_after.as_deref(), Some("86400"));
}

#[sqlx::test(migrations = false)]
async fn a_finishing_flow_answers_429_with_5_s(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (session, mut gate) = gated();
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup.fake.script_session(session);
    script_start(&setup, "device-code-1");
    flow_of(&setup.start(&alice).await);
    gate.entered().await;

    let refused = setup.start(&alice).await;

    assert_eq!(refused.status, 429, "{}", refused.text);
    assert_eq!(refused.code(), "rate_limited");
    assert_eq!(refused.retry_after.as_deref(), Some("5"));
    drop(gate);
}

#[sqlx::test(migrations = false)]
async fn every_place_taken_answers_503_with_30_s(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, limits(1, 10), AccountsConfig::default()).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    held(&setup, &alice, "device-code-1").await;

    let refused = setup.start(&bob).await;

    assert_eq!(refused.status, 503, "{}", refused.text);
    assert_eq!(refused.code(), "busy");
    assert_eq!(refused.retry_after.as_deref(), Some("30"));
}

#[sqlx::test(migrations = false)]
async fn microsoft_away_is_503_with_30_s_and_a_broken_setup_500(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    setup
        .fake
        .script_start(Script::Answer(Err(Failure::Unavailable { status: 503 })));
    setup
        .fake
        .script_start(Script::Answer(Err(Failure::Certificate)));

    let away = setup.start(&alice).await;
    let broken = setup.start(&alice).await;

    assert_eq!(away.status, 503, "{}", away.text);
    assert_eq!(away.code(), "busy");
    assert_eq!(away.retry_after.as_deref(), Some("30"));
    assert_eq!(broken.status, 500, "{}", broken.text);
    assert_eq!(broken.code(), "internal");
}

#[sqlx::test(migrations = false)]
async fn a_start_while_the_server_stops_answers_503_with_1_s(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    bounded(setup.link_flows.shutdown()).await;

    let refused = setup.start(&alice).await;

    assert_eq!(refused.status, 503, "{}", refused.text);
    assert_eq!(refused.code(), "busy");
    assert_eq!(refused.retry_after.as_deref(), Some("1"));
    assert_eq!(setup.fake.calls().len(), 0);
}

#[sqlx::test(migrations = false)]
async fn the_link_routes_spend_the_users_request_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let limits = RateLimitConfig {
        per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
        ..RateLimitConfig::default()
    };
    let setup = HttpSetup::with(options, limits, AccountsConfig::default()).await;
    let alice = setup.person(1, Role::Member).await;
    let flow = held(&setup, &alice, "device-code-1").await;

    let refused = setup.status(&alice, &flow.to_string()).await;
    let start_refused = setup.start(&alice).await;

    assert_eq!(refused.status, 429, "{}", refused.text);
    assert_eq!(start_refused.status, 429, "{}", start_refused.text);
    assert_eq!(setup.fake.calls().len(), 1);
}

// --- The commit's re-checks, through the real routes --------------------------------

/// Starts a flow for `by` held in its first wait, with an approval and a
/// session of `PROFILE` scripted behind it.
async fn waiting_to_link(setup: &HttpSetup, by: &Person) -> (LinkFlowId, PaceGate) {
    let mut wait = setup.pace.hold_next();
    setup.fake.script_poll(Script::Answer(Ok(approval(1))));
    setup
        .fake
        .script_session(Script::Answer(Ok(profile_session())));
    script_start(setup, "device-code-1");
    let flow = flow_of(&setup.start(by).await);
    wait.entered().await;
    (flow, wait)
}

/// How many accounts `owner` owns.
async fn owned(setup: &HttpSetup, owner: &Person) -> u32 {
    setup.db.accounts().count_owned_by(owner.id).await.unwrap()
}

#[sqlx::test(migrations = false)]
async fn a_logout_during_the_wait_fails_the_flow(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (flow, wait) = waiting_to_link(&setup, &alice).await;

    let logout = setup
        .request(
            Method::POST,
            "/api/v1/auth/logout",
            Some(&alice.access),
            None,
        )
        .await;
    wait.release();

    assert_eq!(logout.status, 204, "{}", logout.text);
    assert_eq!(ended(&setup, flow).await, LinkFlowStatus::Failed);
    assert_eq!(owned(&setup, &alice).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_session_ended_from_another_session_fails_the_flow(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let other = setup.session_of(alice.id, Role::Member).await;
    let (flow, wait) = waiting_to_link(&setup, &alice).await;

    let revoked = setup
        .request(
            Method::DELETE,
            &format!("/api/v1/me/sessions/{}", alice.session),
            Some(&other.access),
            None,
        )
        .await;
    wait.release();

    assert_eq!(revoked.status, 204, "{}", revoked.text);
    assert_eq!(ended(&setup, flow).await, LinkFlowStatus::Failed);
    assert_eq!(owned(&setup, &alice).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_refresh_during_the_wait_still_links(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let (flow, wait) = waiting_to_link(&setup, &alice).await;

    let refreshed = setup
        .request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            Some(json!({ "refresh_token": alice.refresh.expose_secret() })),
        )
        .await;
    wait.release();

    assert_eq!(refreshed.status, 200, "{}", refreshed.text);
    assert!(matches!(
        ended(&setup, flow).await,
        LinkFlowStatus::Linked { .. }
    ));
    assert_eq!(owned(&setup, &alice).await, 1);
}

// --- serve -----------------------------------------------------------------------

/// Delivers the signals the test sends.
struct ChannelSignals(mpsc::Receiver<Signal>);

impl ShutdownSignals for ChannelSignals {
    async fn recv(&mut self) -> Signal {
        match self.0.recv().await {
            Some(signal) => signal,
            None => core::future::pending().await,
        }
    }
}

#[sqlx::test(migrations = false)]
async fn serve_stops_every_link_flow_at_the_signal(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let mut setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut wait = setup.pace.hold_next();
    script_start(&setup, "device-code-serve");
    let started = bounded(setup.link_flows.start(&alice.acting()))
        .await
        .unwrap();
    wait.entered().await;
    let app = setup.app.take().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, receiver) = mpsc::channel(1);
    let timeouts = ShutdownTimeouts {
        drain: Duration::from_secs(1),
        close: Duration::from_secs(2),
    };

    stop.send(Signal::Terminate).await.unwrap();
    let exit = bounded(serve(
        listener,
        None,
        app,
        &setup.db,
        ChannelSignals(receiver),
        timeouts,
    ))
    .await;

    assert_eq!(exit, Exit::Stopped);
    assert_eq!(
        setup.link_flows.status(started.id).map(|view| view.status),
        Some(LinkFlowStatus::Failed)
    );
    assert_eq!(setup.link_flows.flows().active(), 0);
    drop(wait);
}
