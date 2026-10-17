//! The session grants over the real port (P10.6; ADR-0019, S57–S60): the
//! reconciler places the bots, the test's client asks for their sessions,
//! and the session-token service is scripted (R46).

use core::time::Duration;

use fleet_core::id::BotId;
use fleet_core::mc::SessionCredentials;
use fleet_proto::control::{Assignment, ServerMessage, SessionDenied, SessionGrant};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::grpc::control::{ControlTimings, GrantBounds};
use secrecy::ExposeSecret as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{
    Agent, BOUND, Call, Port, Settings, TOKEN, minecraft_session, profile, session_request, start,
};

fn ids(assignments: &[Assignment]) -> Vec<BotId> {
    assignments
        .iter()
        .map(|assignment| assignment.spec.id)
        .collect()
}

/// The reconciler with a stream's grant queue of `bounds`.
fn with_grants(bounds: GrantBounds) -> Settings {
    let mut timings = ControlTimings::new(fleet_server::config::Heartbeats::default());
    timings.grants = bounds;
    Settings {
        timings,
        ..Settings::reconciling()
    }
}

/// `agent`'s welcomed stream, once it was given exactly `bots`.
async fn given(port: &Port, agent: &Agent, bots: &[BotId]) -> Call {
    let (mut call, _) = port.welcomed(agent).await;
    let mut given = ids(&call.reconciled().await);
    given.sort();
    let mut expected = bots.to_vec();
    expected.sort();
    assert_eq!(given, expected);
    call
}

async fn granted(call: &mut Call) -> SessionGrant {
    match tokio::time::timeout(BOUND, call.next()).await.unwrap() {
        Ok(Some(ServerMessage::SessionGrant(grant))) => grant,
        other => panic!("a grant: {other:?}"),
    }
}

async fn denied(call: &mut Call) -> SessionDenied {
    match tokio::time::timeout(BOUND, call.next()).await.unwrap() {
        Ok(Some(ServerMessage::SessionDenied(denied))) => denied,
        other => panic!("a denial: {other:?}"),
    }
}

#[sqlx::test(migrations = false)]
async fn a_held_bots_grants_arrive_with_their_number_and_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let offline = port.bot(1).await;
    let (account, online) = port.online_bot(2).await;
    let mut call = given(&port, &agent, &[offline, online]).await;

    call.send(session_request(1, offline, false)).await;
    let first = granted(&mut call).await;
    call.send(session_request(2, online, false)).await;
    let second = granted(&mut call).await;

    assert_eq!(first.id.get().get(), 1);
    assert_eq!(first.bot_id, offline);
    assert!(
        matches!(&first.credentials, SessionCredentials::Offline { username } if username.as_str() == "AfkBot1"),
        "an offline account's grant is its name: {:?}",
        first.credentials
    );
    assert_eq!(second.id.get().get(), 2);
    assert_eq!(second.bot_id, online);
    let SessionCredentials::Online {
        access_token,
        username,
        ..
    } = &second.credentials
    else {
        panic!("an online session");
    };
    assert_eq!(access_token.expose_secret(), TOKEN);
    assert_eq!(username.as_str(), "AfkOnline1");
    assert_eq!(
        port.minecraft.calls(),
        [(account, false)],
        "the account from the assignment, once"
    );
    port.stop().await;
}

/// A later stream of the same agent numbers its requests from 1 again: a
/// grant the server worked out for an earlier stream never reaches it, and
/// its own request gets its own answer (S57).
#[sqlx::test(migrations = false)]
async fn a_grant_never_reaches_a_later_stream_of_the_same_agent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut first = given(&port, &agent, &[bot]).await;
    let gate = port.minecraft.gated();
    first.send(session_request(7, bot, false)).await;
    port.minecraft.called(1).await;

    first.close();
    port.disconnect(&agent).await;
    let mut later = given(&port, &agent, &[bot]).await;
    // The earlier stream's session arrives now.
    let _ = gate.send(Ok(minecraft_session(
        "old-token",
        &profile(1),
        "AfkOnline1",
    )));
    later.send(session_request(7, bot, false)).await;
    let answer = granted(&mut later).await;

    assert_eq!(answer.id.get().get(), 7);
    let SessionCredentials::Online { access_token, .. } = &answer.credentials else {
        panic!("an online session");
    };
    assert_eq!(
        access_token.expose_secret(),
        TOKEN,
        "its own answer, not the earlier stream's"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_streams_waiting_and_running_requests_die_with_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        with_grants(GrantBounds {
            cap: 10,
            workers: 1,
        }),
    )
    .await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, one) = port.online_bot(1).await;
    let (_, two) = port.online_bot(2).await;
    let mut call = given(&port, &agent, &[one, two]).await;
    let gate = port.minecraft.gated();
    call.send(session_request(1, one, false)).await;
    port.minecraft.called(1).await;
    call.send(session_request(2, two, false)).await;

    call.close();
    port.disconnect(&agent).await;

    assert!(
        gate.is_closed(),
        "the running request's wait was dropped with its stream"
    );
    assert_eq!(
        port.minecraft.calls().len(),
        1,
        "the waiting request never ran"
    );
    port.stop().await;
}

/// The limit is spent only once the stream is found holding the bot, so an
/// agent can't spend the limit of a bot another agent runs (S58).
#[sqlx::test(migrations = false)]
async fn a_fresh_request_for_a_bot_the_stream_doesnt_hold_spends_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let settings = Settings {
        limits: RateLimitConfig {
            fresh_sessions: Rate::per_hour(core::num::NonZeroU32::MIN),
            ..RateLimitConfig::default()
        },
        ..Settings::ungated()
    };
    let port = start(options, settings).await;
    let agent = port.enroll(1, "agent-1").await;
    let other = port.enroll(2, "agent-2").await;
    let (_, held) = port.online_bot(1).await;
    let (_, elsewhere) = port.online_bot(2).await;
    // Room for one bot, so the other waits for the second agent.
    let mut call = port.welcomed_with(&agent, 1, Vec::new()).await;
    assert_eq!(ids(&call.reconciled().await), [held]);
    let mut second = given(&port, &other, &[elsewhere]).await;

    // The first agent asks fresh for the second agent's bot.
    call.send(session_request(1, elsewhere, true)).await;
    let refused = denied(&mut call).await;
    second.send(session_request(1, elsewhere, true)).await;
    let granted_there = granted(&mut second).await;

    assert!(refused.retryable);
    assert_eq!(
        granted_there.bot_id, elsewhere,
        "its own limit is untouched"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn past_its_burst_a_fresh_request_is_denied_and_never_reaches_the_token_service(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let settings = Settings {
        limits: RateLimitConfig {
            fresh_sessions: Rate::per_hour(core::num::NonZeroU32::MIN),
            ..RateLimitConfig::default()
        },
        ..Settings::reconciling()
    };
    let port = start(options, settings).await;
    let agent = port.enroll(1, "agent-1").await;
    let (account, bot) = port.online_bot(1).await;
    let mut call = given(&port, &agent, &[bot]).await;

    call.send(session_request(1, bot, true)).await;
    let first = granted(&mut call).await;
    call.send(session_request(2, bot, true)).await;
    let second = denied(&mut call).await;
    call.send(session_request(3, bot, false)).await;
    let third = granted(&mut call).await;

    assert_eq!(first.id.get().get(), 1);
    assert_eq!(second.id.get().get(), 2);
    assert!(second.retryable, "a passing limit never fails the bot");
    assert_eq!(third.id.get().get(), 3, "a request that isn't fresh passes");
    assert_eq!(port.minecraft.calls(), [(account, true), (account, false)]);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn with_the_grant_queue_full_one_more_bot_is_denied_retryable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, with_grants(GrantBounds { cap: 1, workers: 1 })).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, one) = port.online_bot(1).await;
    let (_, two) = port.online_bot(2).await;
    let mut call = given(&port, &agent, &[one, two]).await;
    let gate = port.minecraft.gated();
    call.send(session_request(1, one, false)).await;
    port.minecraft.called(1).await;

    call.send(session_request(2, two, false)).await;
    let refused = denied(&mut call).await;
    gate.send(Ok(minecraft_session(TOKEN, &profile(1), "AfkOnline1")))
        .unwrap();
    let answer = granted(&mut call).await;

    assert_eq!(refused.id.get().get(), 2);
    assert_eq!(refused.bot_id, two);
    assert!(refused.retryable);
    assert_eq!(answer.id.get().get(), 1);
    assert_eq!(port.registry.take_refused_reports()[0].sessions_denied, 1);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_replaced_request_is_never_answered(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        with_grants(GrantBounds {
            cap: 10,
            workers: 1,
        }),
    )
    .await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = given(&port, &agent, &[bot]).await;
    let gate = port.minecraft.gated();
    call.send(session_request(1, bot, false)).await;
    port.minecraft.called(1).await;

    call.send(session_request(2, bot, false)).await;
    let answer = granted(&mut call).await;
    let late = gate.send(Ok(minecraft_session("late", &profile(1), "AfkOnline1")));
    // Nothing for the first request comes after: the next answer is this.
    call.send(session_request(3, bot, false)).await;
    let next = tokio::time::timeout(Duration::from_secs(5), call.next()).await;

    assert_eq!(answer.id.get().get(), 2, "the newer request's answer");
    assert!(late.is_err(), "the older request's wait was dropped");
    assert!(
        matches!(&next, Ok(Ok(Some(ServerMessage::SessionGrant(grant)))) if grant.id.get().get() == 3),
        "{next:?}"
    );
    port.stop().await;
}
