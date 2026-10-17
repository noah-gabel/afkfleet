//! The session grants' checks (P10.6; ADR-0019, S59–S61): the grant service
//! called directly with a live stream's generation, over the port's real
//! database and registry, its answer read by the test's client. The
//! session-token service is scripted (R46). The sweep waits an hour, so a
//! denial is what the stream sees, never its end.

use core::num::NonZeroU64;
use core::time::Duration;

use fleet_core::account::AccountStatus;
use fleet_core::bot::DesiredRunState;
use fleet_core::id::BotId;
use fleet_core::mc::SessionCredentials;
use fleet_core::system::Clock as _;
use fleet_proto::control::{ServerMessage, SessionDenied, SessionGrant};
use fleet_server::app::session_tokens::SessionTokenError;
use fleet_server::config::{LogConfig, LogFormat};
use fleet_server::ports::session_grants::{GrantRequest, SessionGrants as _};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use secrecy::ExposeSecret as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tracing_subscriber::layer::SubscriberExt as _;

use super::support::{
    Agent, BOUND, Call, Port, Settings, TOKEN, minecraft_session, profile, start,
};

/// The reconciler, with a sweep that never comes during a test.
fn settings() -> Settings {
    Settings {
        sweep: Duration::from_hours(1),
        ..Settings::reconciling()
    }
}

fn request(number: u64, bot: BotId, fresh: bool) -> GrantRequest {
    GrantRequest {
        number: NonZeroU64::new(number).unwrap(),
        bot,
        fresh,
    }
}

/// `agent`'s stream, given `bot`.
async fn holding(port: &Port, agent: &Agent, bot: BotId) -> Call {
    let (mut call, _) = port.welcomed(agent).await;
    let given = call.reconciled().await;
    assert_eq!(given.len(), 1);
    assert_eq!(given[0].spec.id, bot);
    call
}

/// The next session answer on `call`, past anything the reconciler sent
/// meanwhile.
async fn answer(call: &mut Call) -> ServerMessage {
    loop {
        match tokio::time::timeout(BOUND, call.next()).await.unwrap() {
            Ok(Some(
                message @ (ServerMessage::SessionGrant(_) | ServerMessage::SessionDenied(_)),
            )) => return message,
            Ok(Some(_reconciled)) => {}
            other => panic!("a session answer: {other:?}"),
        }
    }
}

async fn granted(call: &mut Call) -> SessionGrant {
    match answer(call).await {
        ServerMessage::SessionGrant(grant) => grant,
        other => panic!("a grant: {other:?}"),
    }
}

async fn denied(call: &mut Call) -> SessionDenied {
    match answer(call).await {
        ServerMessage::SessionDenied(denied) => denied,
        other => panic!("a denial: {other:?}"),
    }
}

#[sqlx::test(migrations = false)]
async fn the_account_comes_from_the_assignment_and_fresh_reaches_the_token_service(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (account, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);

    port.grants(true).grant(stream, request(5, bot, true)).await;
    let grant = granted(&mut call).await;

    assert_eq!(grant.id.get().get(), 5);
    assert_eq!(grant.bot_id, bot);
    let SessionCredentials::Online {
        access_token,
        expires_at,
        ..
    } = &grant.credentials
    else {
        panic!("an online session");
    };
    assert_eq!(access_token.expose_secret(), TOKEN);
    assert!(*expires_at > port.clock.now());
    assert_eq!(port.minecraft.calls(), [(account, true)]);
    port.stop().await;
}

/// The agent's own permission (S27, S59): a disabled agent's request is
/// denied, retryable, and the token service is never asked.
#[sqlx::test(migrations = false)]
async fn a_disabled_agent_gets_a_retryable_denial_and_no_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);

    port.disable(&agent).await;
    port.grants(true)
        .grant(stream, request(1, bot, false))
        .await;

    assert!(denied(&mut call).await.retryable);
    assert_eq!(port.minecraft.calls(), [], "no session was asked for");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_revoked_certificate_gets_a_retryable_denial_and_no_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);

    port.revoke(&agent).await;
    port.grants(true)
        .grant(stream, request(1, bot, false))
        .await;

    assert!(denied(&mut call).await.retryable);
    assert_eq!(port.minecraft.calls(), [], "no session was asked for");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_expired_certificate_gets_a_retryable_denial_and_no_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);

    port.clock.set(agent.not_after);
    port.grants(true)
        .grant(stream, request(1, bot, false))
        .await;

    assert!(denied(&mut call).await.retryable);
    assert_eq!(port.minecraft.calls(), []);
    port.stop().await;
}

/// The bot's row is the account's only source and must name the asking
/// agent too (S3, S59): the host's `agent release` changes rows without
/// telling the registry.
#[sqlx::test(migrations = false)]
async fn a_bot_whose_row_no_longer_names_the_agent_gets_a_retryable_denial_and_no_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);

    port.detach(bot, &agent).await;
    port.grants(true)
        .grant(stream, request(1, bot, false))
        .await;

    assert!(denied(&mut call).await.retryable);
    assert_eq!(port.minecraft.calls(), []);
    port.stop().await;
}

/// fleet-core's `placeable`, as the reconciler judges it (S59): each reason
/// a bot may not run is a retryable denial, and no session is asked for.
#[sqlx::test(migrations = false)]
async fn a_bot_that_may_not_run_gets_a_retryable_denial_and_no_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (account, online) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, online).await;
    let stream = port.grant_stream(&agent, online);
    let grants = port.grants(true);

    port.set_desired(online, DesiredRunState::Stopped).await;
    grants.grant(stream, request(1, online, false)).await;
    let stopped = denied(&mut call).await;
    port.set_desired(online, DesiredRunState::Running).await;
    port.set_status(account, AccountStatus::ReauthRequired)
        .await;
    grants.grant(stream, request(2, online, false)).await;
    let reauth = denied(&mut call).await;
    port.set_status(account, AccountStatus::Pending).await;
    grants.grant(stream, request(3, online, false)).await;
    let pending = denied(&mut call).await;
    port.set_status(account, AccountStatus::Active).await;
    port.set_owner_disabled(account, true).await;
    grants.grant(stream, request(4, online, false)).await;
    let owner = denied(&mut call).await;

    for (denial, number) in [(stopped, 1), (reauth, 2), (pending, 3), (owner, 4)] {
        assert_eq!(denial.id.get().get(), number);
        assert!(
            denial.retryable,
            "{number}: a passing problem never fails the bot"
        );
    }
    assert_eq!(port.minecraft.calls(), []);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_offline_accounts_bot_gets_its_name_only_while_offline_accounts_are_allowed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);

    port.grants(true)
        .grant(stream, request(1, bot, false))
        .await;
    let allowed = granted(&mut call).await;
    port.grants(false)
        .grant(stream, request(2, bot, false))
        .await;
    let refused = denied(&mut call).await;

    assert!(
        matches!(&allowed.credentials, SessionCredentials::Offline { username } if username.as_str() == "AfkBot1"),
        "{:?}",
        allowed.credentials
    );
    assert!(refused.retryable);
    assert_eq!(port.minecraft.calls(), [], "an offline account asks nobody");
    port.stop().await;
}

/// The permission is checked again once the session is in hand (S59): what
/// changed during the wait gives no grant.
#[sqlx::test(migrations = false)]
async fn what_changes_while_the_session_is_minted_gives_no_grant(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (account, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);
    let grants = port.grants(true);

    let mut answers = Vec::new();
    for change in 1..=4_u8 {
        let gate = port.minecraft.gated();
        let asked = grants.grant(stream, request(u64::from(change), bot, false));
        let meanwhile = async {
            port.minecraft.called(usize::from(change)).await;
            match change {
                1 => port.set_desired(bot, DesiredRunState::Stopped).await,
                2 => port.detach(bot, &agent).await,
                3 => port.set_owner_disabled(account, true).await,
                _ => port.disable(&agent).await,
            }
            gate.send(Ok(minecraft_session(TOKEN, &profile(1), "AfkOnline1")))
                .unwrap();
        };
        tokio::join!(asked, meanwhile);
        answers.push(denied(&mut call).await);
        // The bot as it was, for the next change.
        match change {
            1 => port.set_desired(bot, DesiredRunState::Running).await,
            2 => port.assign(bot, &agent).await,
            3 => port.set_owner_disabled(account, false).await,
            _ => {}
        }
    }

    assert!(answers.iter().all(|denied| denied.retryable), "{answers:?}");
    assert_eq!(port.minecraft.calls().len(), 4);
    assert!(
        port.minecraft
            .calls()
            .iter()
            .all(|(asked, _)| *asked == account)
    );
    port.stop().await;
}

/// The asking stream's own hold, read before the token service: a request
/// from a stream that no longer holds the bot assigned (its agent's later
/// stream does now) never reaches the token service, fresh or not, so a
/// bot released while its request waited never costs a refresh.
#[sqlx::test(migrations = false)]
async fn a_stream_that_no_longer_holds_the_bot_never_reaches_the_token_service(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (account, bot) = port.online_bot(1).await;
    let mut earlier = holding(&port, &agent, bot).await;
    let stale = port.grant_stream(&agent, bot);
    earlier.close();
    port.disconnect(&agent).await;
    let mut later = holding(&port, &agent, bot).await;
    let grants = port.grants(true);

    grants.grant(stale, request(1, bot, false)).await;
    grants.grant(stale, request(2, bot, true)).await;
    grants
        .grant(port.grant_stream(&agent, bot), request(3, bot, false))
        .await;
    let grant = granted(&mut later).await;

    assert_eq!(
        grant.id.get().get(),
        3,
        "nothing for the earlier stream's requests"
    );
    assert_eq!(
        port.minecraft.calls(),
        [(account, false)],
        "only the later stream's request reached the token service"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_lasting_refusal_from_the_token_service_fails_the_bot_and_a_disabled_owner_doesnt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);
    port.minecraft.answer(Err(SessionTokenError::NotActive {
        status: AccountStatus::ReauthRequired,
    }));
    port.minecraft.answer(Err(SessionTokenError::OwnerDisabled));
    port.minecraft
        .answer(Err(SessionTokenError::ChangedMeanwhile));

    let grants = port.grants(true);
    grants.grant(stream, request(1, bot, false)).await;
    let reauth = denied(&mut call).await;
    grants.grant(stream, request(2, bot, false)).await;
    let owner = denied(&mut call).await;
    grants.grant(stream, request(3, bot, false)).await;
    let changed = denied(&mut call).await;

    assert!(
        !reauth.retryable,
        "reauth_required needs a human (§6 row 5)"
    );
    assert!(owner.retryable, "U14");
    assert!(changed.retryable);
    port.stop().await;
}

/// A grant changes no row, so it writes no audit entry; nor does a denial
/// (S61).
#[sqlx::test(migrations = false)]
async fn neither_a_grant_nor_a_denial_writes_an_audit_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);
    let before = port.audit().await.len();
    port.minecraft.answer(Err(SessionTokenError::Microsoft(
        fleet_server::ports::msauth::Failure::Unreachable,
    )));

    let grants = port.grants(true);
    grants.grant(stream, request(1, bot, false)).await;
    let _ = denied(&mut call).await;
    grants.grant(stream, request(2, bot, false)).await;
    let _ = granted(&mut call).await;
    port.disable(&agent).await;
    grants.grant(stream, request(3, bot, false)).await;
    let _ = denied(&mut call).await;

    assert_eq!(port.audit().await.len(), before);
    port.stop().await;
}

/// The grant's own trail (the user's change at the plan review): one `info`
/// line with its stream, its bot, the request's number, its kind, whether
/// it was fresh and its expiry; never the token, the name or the UUID.
#[sqlx::test(migrations = false)]
async fn a_grant_is_logged_at_info_with_its_stream_and_bot_and_nothing_secret(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let mut call = holding(&port, &agent, bot).await;
    let stream = port.grant_stream(&agent, bot);
    port.minecraft.answer(Ok(minecraft_session(
        "secret-token-in-a-buffer",
        &profile(1),
        "HiddenName8",
    )));
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));

    {
        let _guard = tracing::subscriber::set_default(subscriber);
        port.grants(true).grant(stream, request(4, bot, true)).await;
    }
    let _ = granted(&mut call).await;

    let lines = buffer.lines_with("a session was granted").unwrap();
    assert_eq!(lines.len(), 1, "{}", buffer.text());
    let line = &lines[0];
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["agent_id"], agent.id.to_string());
    assert_eq!(line["bot_id"], bot.to_string());
    assert_eq!(line["session_request"], 4);
    assert_eq!(line["kind"], "online");
    assert_eq!(line["fresh"], true);
    assert!(line["expires_at"].is_string());
    for secret in [
        "secret-token-in-a-buffer",
        "HiddenName8",
        "AfkOnline1",
        &profile(1),
    ] {
        assert!(!buffer.text().contains(secret), "{secret}");
    }
    port.stop().await;
}
