//! The managed credentials provider (P10.6; ADR-0019, S3, S62, S63), over
//! the fake link, on paused time: the run's clock and the wait's bound move
//! with tokio's.

use core::num::NonZeroU64;
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_agent::config::{LogConfig, LogFilter, LogFormat};
use fleet_agent::managed::credentials::{
    LinkGeneration, ManagedCredentials, SessionAnswer, Unsent,
};
use fleet_agent::telemetry::layer;
use fleet_core::bot::BotAccount;
use fleet_core::id::{AccountId, BotId};
use fleet_core::mc::{
    CredentialError, SessionCredentialProvider as _, SessionCredentials, SessionRequest,
};
use fleet_core::value::McUuid;
use fleet_proto::control::{SessionDenied, SessionGrant, SessionRequestId};
use fleet_runtime::RuntimeClock;
use fleet_testkit::log_buffer::LogBuffer;
use rstest::rstest;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::task::JoinHandle;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::fakes::FakeLink;

/// The connect timeout the provider keeps tokens past.
const CONNECT: Duration = Duration::from_secs(30);
/// How long a request waits for its answer: the runtime's 30 s.
const BOUND: Duration = Duration::from_secs(30);
/// The profile every granted session here belongs to.
const PROFILE: &str = "0e1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
/// The name every granted session here carries.
const NAME: &str = "SessionName7";

fn anchor() -> DateTime<Utc> {
    "2027-01-15T08:00:00Z".parse().unwrap()
}

fn bot(n: u8) -> BotId {
    BotId::new_v7(anchor(), [n; 10]).unwrap()
}

fn provider(link: &FakeLink) -> Arc<ManagedCredentials<FakeLink>> {
    Arc::new(ManagedCredentials::new(
        link.clone(),
        RuntimeClock::new(anchor()),
        CONNECT,
        BOUND,
    ))
}

fn request(bot: BotId, fresh: bool) -> SessionRequest {
    SessionRequest {
        bot_id: bot,
        account: BotAccount::Online(AccountId::new_v7(anchor(), [7; 10]).unwrap()),
        fresh,
    }
}

/// `provider`'s answer to `request`, on its own task.
fn ask(
    provider: &Arc<ManagedCredentials<FakeLink>>,
    request: SessionRequest,
) -> JoinHandle<Result<SessionCredentials, CredentialError>> {
    let provider = Arc::clone(provider);
    tokio::spawn(async move { provider.session(request).await })
}

fn online(token: &str, expires_at: DateTime<Utc>) -> SessionCredentials {
    SessionCredentials::Online {
        username: NAME.parse().unwrap(),
        uuid: PROFILE.parse::<McUuid>().unwrap().into(),
        access_token: SecretString::from(token),
        expires_at,
    }
}

fn grant(id: SessionRequestId, bot: BotId, token: &str) -> SessionAnswer {
    grant_until(id, bot, token, anchor() + TimeDelta::days(1))
}

fn grant_until(
    id: SessionRequestId,
    bot: BotId,
    token: &str,
    expires_at: DateTime<Utc>,
) -> SessionAnswer {
    SessionAnswer::Grant(SessionGrant {
        id,
        bot_id: bot,
        credentials: online(token, expires_at),
    })
}

fn denial(id: SessionRequestId, bot: BotId, retryable: bool) -> SessionAnswer {
    SessionAnswer::Denied(SessionDenied {
        id,
        bot_id: bot,
        retryable,
    })
}

fn number(n: u64) -> SessionRequestId {
    SessionRequestId::new(NonZeroU64::new(n).unwrap())
}

fn token(credentials: &SessionCredentials) -> String {
    match credentials {
        SessionCredentials::Online { access_token, .. } => access_token.expose_secret().to_owned(),
        SessionCredentials::Offline { .. } => panic!("an online session"),
    }
}

/// `task`'s answer, which must come without time passing.
async fn now(
    task: JoinHandle<Result<SessionCredentials, CredentialError>>,
) -> Result<SessionCredentials, CredentialError> {
    let started = tokio::time::Instant::now();
    let answer = task.await.unwrap();
    assert_eq!(started.elapsed(), Duration::ZERO, "it answered at once");
    answer
}

/// Whether `task` still waits after everything else ran.
async fn waiting(task: &JoinHandle<Result<SessionCredentials, CredentialError>>) -> bool {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    !task.is_finished()
}

/// The provider holding a grant of `token` for `bot`, handed out once.
async fn holding(
    link: &FakeLink,
    bot: BotId,
    token_text: &str,
) -> Arc<ManagedCredentials<FakeLink>> {
    let provider = provider(link);
    let first = ask(&provider, request(bot, false));
    let sent = link.wait_sent(1).await;
    provider.answer(grant(sent.id, bot, token_text));
    assert_eq!(token(&now(first).await.unwrap()), token_text);
    provider
}

const RETRYABLE: CredentialError = CredentialError { retryable: true };

/// The `warn` for a granted token that expires within the connect timeout.
const CLOCK_SKEW: &str = "a granted session expires within the connect timeout; the agent's or the server's clock may be off";

/// Captures this thread's events as the agent's JSON lines, `debug` and up,
/// until the guard drops; the asking tasks run on this thread too.
fn capture_logs() -> (LogBuffer, DefaultGuard) {
    let capture = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: LogFilter::try_from("debug").unwrap(),
    };
    let subscriber = tracing_subscriber::registry().with(layer(&config, false, capture.clone()));
    (capture, tracing::subscriber::set_default(subscriber))
}

// --- Correlation (S3) ------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn each_answer_goes_to_the_request_with_its_number_and_bot() {
    let link = FakeLink::default();
    let provider = provider(&link);
    let first = ask(&provider, request(bot(1), false));
    let one = link.wait_sent(1).await;
    let second = ask(&provider, request(bot(2), true));
    let two = link.wait_sent(2).await;

    provider.answer(grant(two.id, bot(2), "token-2"));
    provider.answer(grant(one.id, bot(1), "token-1"));

    assert_eq!(token(&now(first).await.unwrap()), "token-1");
    assert_eq!(token(&now(second).await.unwrap()), "token-2");
    assert_eq!(
        (one.id.get().get(), one.bot_id, one.fresh),
        (1, bot(1), false)
    );
    assert_eq!(
        (two.id.get().get(), two.bot_id, two.fresh),
        (2, bot(2), true)
    );
}

#[tokio::test(start_paused = true)]
async fn an_answer_for_another_bot_is_dropped_and_its_request_keeps_waiting() {
    let link = FakeLink::default();
    let provider = provider(&link);
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(1).await;

    provider.answer(grant(sent.id, bot(2), "someone-elses"));
    let still = waiting(&asked).await;
    provider.answer(grant(sent.id, bot(1), "its-own"));

    assert!(still, "a mismatched answer answers nothing");
    assert_eq!(token(&now(asked).await.unwrap()), "its-own");
}

/// A late grant never answers a later request (S3): the actor dropped the
/// first, then asked again, fresh.
#[tokio::test(start_paused = true)]
async fn an_answer_to_a_dropped_request_answers_nothing_later() {
    let link = FakeLink::default();
    let provider = provider(&link);
    let dropped = ask(&provider, request(bot(1), false));
    let first = link.wait_sent(1).await;
    dropped.abort();
    let _ = dropped.await;
    let forgotten = format!("{provider:?}");
    let again = ask(&provider, request(bot(1), true));
    let second = link.wait_sent(2).await;

    provider.answer(grant(first.id, bot(1), "late"));
    let still = waiting(&again).await;
    provider.answer(grant(second.id, bot(1), "after-the-rejection"));

    assert!(
        forgotten.contains("waiting: 0"),
        "a dropped request waits no more: {forgotten}"
    );
    assert!(still, "the late grant answered nothing");
    assert_eq!(token(&now(again).await.unwrap()), "after-the-rejection");
}

#[tokio::test(start_paused = true)]
async fn a_request_waits_at_most_its_bound_and_a_later_answer_answers_nothing() {
    let link = FakeLink::default();
    let provider = provider(&link);
    let asked = ask(&provider, request(bot(1), false));
    let first = link.wait_sent(1).await;

    let started = tokio::time::Instant::now();
    let answer = asked.await.unwrap();
    let waited = started.elapsed();
    let left = format!("{provider:?}");
    provider.answer(grant(first.id, bot(1), "too-late"));
    let again = ask(&provider, request(bot(1), false));
    let second = link.wait_sent(2).await;
    let still = waiting(&again).await;
    provider.answer(grant(second.id, bot(1), "in-time"));

    assert_eq!(answer.unwrap_err(), RETRYABLE);
    assert_eq!(waited, BOUND);
    assert!(
        left.contains("waiting: 0"),
        "nothing waits past its bound: {left}"
    );
    assert!(still, "the late answer answered nothing");
    assert_eq!(token(&now(again).await.unwrap()), "in-time");
}

#[tokio::test(start_paused = true)]
async fn request_numbers_count_from_one_and_are_never_reused() {
    let link = FakeLink::default();
    let provider = provider(&link);

    for n in 1..=3_usize {
        let asked = ask(&provider, request(bot(1), false));
        let sent = link.wait_sent(n).await;
        if n == 2 {
            asked.abort();
            let _ = asked.await;
        } else {
            provider.answer(grant(sent.id, bot(1), "token"));
            asked.await.unwrap().unwrap();
        }
    }

    let numbers: Vec<u64> = link.sent().iter().map(|sent| sent.id.get().get()).collect();
    assert_eq!(numbers, [1, 2, 3]);
}

#[rstest]
#[case::retryable(true)]
#[case::lasting(false)]
#[tokio::test(start_paused = true)]
async fn a_denial_answers_with_its_retryable_flag(#[case] retryable: bool) {
    let link = FakeLink::default();
    let provider = provider(&link);
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(1).await;

    provider.answer(denial(sent.id, bot(1), retryable));

    assert_eq!(now(asked).await.unwrap_err(), CredentialError { retryable });
}

// --- The link (S62) --------------------------------------------------------------

/// A full queue fails at once even with a kept grant: only a link with no
/// stream stands it in (S63).
#[rstest]
#[case::no_stream_and_nothing_kept(Unsent::NoStream, false)]
#[case::queue_full_with_a_kept_grant(Unsent::QueueFull, true)]
#[tokio::test(start_paused = true)]
async fn a_request_the_link_cant_send_fails_at_once_retryable(
    #[case] unsent: Unsent,
    #[case] kept: bool,
) {
    let link = FakeLink::default();
    let provider = if kept {
        holding(&link, bot(1), "kept").await
    } else {
        provider(&link)
    };
    link.answer(Err(unsent));

    let answer = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(answer.unwrap_err(), RETRYABLE);
}

#[tokio::test(start_paused = true)]
async fn a_streams_end_fails_its_waiting_requests_at_once_and_spares_a_newer_streams() {
    let link = FakeLink::default();
    let provider = provider(&link);
    let old = ask(&provider, request(bot(1), false));
    link.wait_sent(1).await;
    link.answer(Ok(LinkGeneration::new(2)));
    let new = ask(&provider, request(bot(2), false));
    let sent = link.wait_sent(2).await;

    provider.link_ended(LinkGeneration::new(1));
    let failed = now(old).await;
    let still = waiting(&new).await;
    provider.answer(grant(sent.id, bot(2), "newer"));

    assert_eq!(failed.unwrap_err(), RETRYABLE);
    assert!(still, "a newer stream's request still waits");
    assert_eq!(token(&now(new).await.unwrap()), "newer");
}

#[tokio::test(start_paused = true)]
async fn a_request_sent_on_a_stream_that_already_ended_fails_at_once() {
    let link = FakeLink::default();
    let provider = provider(&link);
    provider.link_ended(LinkGeneration::new(1));

    let answer = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(answer.unwrap_err(), RETRYABLE);
}

// --- Expiry (ADR-0013) -----------------------------------------------------------

#[rstest]
#[case::at_the_connect_timeout(TimeDelta::seconds(30), false)]
#[case::a_millisecond_later(TimeDelta::milliseconds(30_001), true)]
#[case::expired(TimeDelta::seconds(-1), false)]
#[tokio::test(start_paused = true)]
async fn a_token_that_expires_within_the_connect_timeout_is_never_handed_out(
    #[case] left: TimeDelta,
    #[case] handed_out: bool,
) {
    let (capture, _guard) = capture_logs();
    let link = FakeLink::default();
    let provider = provider(&link);
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(1).await;

    provider.answer(grant_until(sent.id, bot(1), "token", anchor() + left));

    let answer = now(asked).await;
    let warned = capture
        .lines_with(CLOCK_SKEW)
        .unwrap()
        .into_iter()
        .filter(|line| line["level"] == "WARN" && line["bot_id"] == bot(1).to_string())
        .count();
    if handed_out {
        assert_eq!(token(&answer.unwrap()), "token");
        assert_eq!(warned, 0, "{}", capture.text());
    } else {
        assert_eq!(answer.unwrap_err(), RETRYABLE);
        assert_eq!(warned, 1, "the clocks are named: {}", capture.text());
    }
}

#[tokio::test(start_paused = true)]
async fn an_offline_grant_is_handed_out_as_it_is() {
    let link = FakeLink::default();
    let provider = provider(&link);
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(1).await;

    provider.answer(SessionAnswer::Grant(SessionGrant {
        id: sent.id,
        bot_id: bot(1),
        credentials: SessionCredentials::Offline {
            username: "AfkBot1".parse().unwrap(),
        },
    }));

    assert!(matches!(
        now(asked).await.unwrap(),
        SessionCredentials::Offline { username } if username.as_str() == "AfkBot1"
    ));
}

// --- Kept grants (S63) -----------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn without_a_stream_the_kept_grant_answers_a_request_that_isnt_fresh() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "kept").await;
    link.answer(Err(Unsent::NoStream));

    let answer = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(token(&answer.unwrap()), "kept");
}

/// Each unsent request says whether it was fresh, so a refused login's
/// requests can be followed through an outage (ADR-0020, S1).
#[rstest]
#[case::fresh(true)]
#[case::not_fresh(false)]
#[tokio::test(start_paused = true)]
async fn an_unsent_request_says_whether_it_was_fresh(#[case] fresh: bool) {
    let (capture, _guard) = capture_logs();
    let link = FakeLink::default();
    let provider = provider(&link);
    link.answer(Err(Unsent::NoStream));

    let answer = now(ask(&provider, request(bot(1), fresh))).await;

    assert_eq!(answer.unwrap_err(), RETRYABLE);
    let lines = capture
        .lines_with("a session request couldn't be sent")
        .unwrap();
    assert_eq!(lines.len(), 1, "{}", capture.text());
    assert_eq!(lines[0]["fresh"], fresh, "{}", capture.text());
}

#[tokio::test(start_paused = true)]
async fn with_a_stream_the_server_is_always_asked() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "kept").await;

    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(2).await;
    let still = waiting(&asked).await;
    provider.answer(grant(sent.id, bot(1), "asked-again"));

    assert!(still, "not answered from the kept grant");
    assert_eq!(token(&now(asked).await.unwrap()), "asked-again");
}

#[tokio::test(start_paused = true)]
async fn a_request_waiting_when_its_stream_ends_never_gets_the_kept_grant() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "kept").await;
    let asked = ask(&provider, request(bot(1), false));
    link.wait_sent(2).await;

    provider.link_ended(LinkGeneration::new(1));

    assert_eq!(now(asked).await.unwrap_err(), RETRYABLE);
}

#[tokio::test(start_paused = true)]
async fn a_fresh_request_wipes_the_kept_grant() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "kept").await;
    link.answer(Err(Unsent::NoStream));

    let fresh = now(ask(&provider, request(bot(1), true))).await;
    let after = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(
        fresh.unwrap_err(),
        RETRYABLE,
        "a rejected token isn't reused"
    );
    assert_eq!(after.unwrap_err(), RETRYABLE, "it's gone");
}

#[tokio::test(start_paused = true)]
async fn a_denial_wipes_the_kept_grant() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "kept").await;
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(2).await;
    provider.answer(denial(sent.id, bot(1), true));
    let _ = now(asked).await;
    link.answer(Err(Unsent::NoStream));

    let after = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(after.unwrap_err(), RETRYABLE);
}

/// Any denial for a bot is the server's latest word on it (S63), even one
/// nobody waits for.
#[tokio::test(start_paused = true)]
async fn a_denial_nobody_waits_for_still_wipes_its_bots_kept_grant() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "kept").await;

    provider.answer(denial(number(99), bot(1), true));
    link.answer(Err(Unsent::NoStream));
    let after = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(after.unwrap_err(), RETRYABLE);
}

/// A grant that comes after its request was dropped is never kept (S3,
/// S63): the grant handed out before it stands in.
#[tokio::test(start_paused = true)]
async fn a_late_answer_is_never_kept() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "handed-out").await;
    let dropped = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(2).await;
    dropped.abort();
    let _ = dropped.await;

    provider.answer(grant(sent.id, bot(1), "late"));
    link.answer(Err(Unsent::NoStream));
    let answer = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(token(&answer.unwrap()), "handed-out");
}

#[tokio::test(start_paused = true)]
async fn forget_wipes_one_bots_kept_grant_and_forget_all_every_one() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "one").await;
    let second = ask(&provider, request(bot(2), false));
    let sent = link.wait_sent(2).await;
    provider.answer(grant(sent.id, bot(2), "two"));
    second.await.unwrap().unwrap();
    link.answer(Err(Unsent::NoStream));

    provider.forget(bot(1));
    let one = now(ask(&provider, request(bot(1), false))).await;
    let two = now(ask(&provider, request(bot(2), false))).await;
    provider.forget_all();
    let two_after = now(ask(&provider, request(bot(2), false))).await;

    assert_eq!(one.unwrap_err(), RETRYABLE);
    assert_eq!(token(&two.unwrap()), "two");
    assert_eq!(two_after.unwrap_err(), RETRYABLE);
}

/// What wipes a bot's kept grant (S63).
#[derive(Debug, Clone, Copy)]
enum Wipe {
    Forget,
    ForgetAll,
    /// A denial for the bot, of a request nobody waits for.
    Denial,
}

/// A wipe between a grant's answer and its hand-out wins (S63): the
/// stream's reader can hand over a grant and then handle a `Reject` before
/// the actor's task runs again.
#[rstest]
#[case::forget(Wipe::Forget)]
#[case::forget_all(Wipe::ForgetAll)]
#[case::a_denial(Wipe::Denial)]
#[tokio::test(start_paused = true)]
async fn a_wipe_after_a_grant_is_answered_and_before_it_is_handed_out_wins(#[case] wipe: Wipe) {
    let link = FakeLink::default();
    let provider = provider(&link);
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(1).await;

    // In one go, as the stream's reader would: the asking task runs only
    // when this one waits.
    provider.answer(grant(sent.id, bot(1), "in-flight"));
    match wipe {
        Wipe::Forget => provider.forget(bot(1)),
        Wipe::ForgetAll => provider.forget_all(),
        Wipe::Denial => provider.answer(denial(number(99), bot(1), true)),
    }
    let handed_out = now(asked).await;
    link.answer(Err(Unsent::NoStream));
    let after = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(
        token(&handed_out.unwrap()),
        "in-flight",
        "its request still gets it"
    );
    assert_eq!(after.unwrap_err(), RETRYABLE, "but it isn't kept");
}

#[tokio::test(start_paused = true)]
async fn a_kept_grant_within_the_connect_timeout_of_its_expiry_isnt_handed_out() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "kept").await;
    link.answer(Err(Unsent::NoStream));

    // A day's token, a day less the connect timeout later.
    tokio::time::advance(Duration::from_hours(24).checked_sub(CONNECT).unwrap()).await;
    let answer = now(ask(&provider, request(bot(1), false))).await;

    assert_eq!(answer.unwrap_err(), RETRYABLE);
}

// --- What it shows -----------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn its_debug_form_shows_how_many_requests_wait_and_nothing_else() {
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "token-in-debug").await;
    let _asked = ask(&provider, request(bot(1), false));
    link.wait_sent(2).await;

    let shown = format!("{provider:?}");

    assert!(shown.contains("waiting: 1"), "{shown}");
    for secret in ["token-in-debug", NAME, PROFILE] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

/// An answer as the stream read it prints its session's kind and expiry,
/// never its name or UUID (the threat model).
#[rstest]
#[case::online(grant(number(1), bot(1), "token-in-debug"), "Online { expires_at")]
#[case::offline(SessionAnswer::Grant(SessionGrant {
    id: number(1),
    bot_id: bot(1),
    credentials: SessionCredentials::Offline { username: NAME.parse().unwrap() },
}), "Offline")]
fn a_session_answers_debug_form_shows_no_name_or_uuid(
    #[case] answer: SessionAnswer,
    #[case] kind: &str,
) {
    let plain = format!("{answer:?}");
    let pretty = format!("{answer:#?}");

    for shown in [&plain, &pretty] {
        for secret in ["token-in-debug", NAME, PROFILE, &PROFILE.replace('-', "")] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
    assert!(plain.contains(kind), "{plain}");
}

/// The token, the name and the UUID never reach a log line, of any level
/// or target, on any path (S7): `log_capture` records everything.
#[tokio::test(start_paused = true)]
async fn no_log_line_holds_a_token_a_name_or_a_uuid() {
    fleet_testkit::log_capture::install().unwrap();
    let link = FakeLink::default();
    let provider = holding(&link, bot(1), "token-handed-out").await;
    // A late grant, one for another bot, one about to expire, a denial.
    let dropped = ask(&provider, request(bot(1), false));
    let late = link.wait_sent(2).await;
    dropped.abort();
    let _ = dropped.await;
    provider.answer(grant(late.id, bot(1), "token-late"));
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(3).await;
    provider.answer(grant(sent.id, bot(2), "token-wrong-bot"));
    provider.answer(grant_until(sent.id, bot(1), "token-expiring", anchor()));
    let _ = asked.await;
    let asked = ask(&provider, request(bot(1), false));
    let sent = link.wait_sent(4).await;
    provider.answer(denial(sent.id, bot(1), true));
    let _ = asked.await;
    // A stream's end, and a kept grant handed out with no stream.
    let asked = ask(&provider, request(bot(2), false));
    link.wait_sent(5).await;
    provider.link_ended(LinkGeneration::new(1));
    link.answer(Ok(LinkGeneration::new(2)));
    let _ = asked.await;
    let asked = ask(&provider, request(bot(3), false));
    let sent = link.wait_sent(6).await;
    provider.answer(grant(sent.id, bot(3), "token-kept"));
    let _ = asked.await;
    link.answer(Err(Unsent::NoStream));
    let kept = ask(&provider, request(bot(3), false)).await.unwrap();
    assert_eq!(
        token(&kept.unwrap()),
        "token-kept",
        "the kept grant, with no stream"
    );

    for secret in [
        "token-handed-out",
        "token-late",
        "token-wrong-bot",
        "token-expiring",
        "token-kept",
        NAME,
        PROFILE,
        &PROFILE.replace('-', ""),
    ] {
        fleet_testkit::log_capture::check_absent(secret).unwrap();
    }
    assert!(
        fleet_testkit::log_capture::count(
            "fleet_agent::managed::credentials",
            "a session request was sent"
        ) >= 6,
        "the capture records the provider's lines"
    );
}
