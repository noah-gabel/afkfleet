//! What never reaches a log line (S24): the peer's address, the name a
//! hello gave and the certificate's bytes, of any target; and a session's
//! token, name and UUID, granted or denied (P10.6; S7, S60); a chat's text
//! and sender, stored or dropped (P10.8; S70).

use core::fmt::Write as _;

use chrono::DateTime;
use fleet_core::chat::{ChatMessage, IncomingChat, PlayerChatKind};
use fleet_core::id::BotId;
use fleet_core::system::Clock as _;
use fleet_proto::control::{AgentMessage, BotEvent, BotEventKind, ServerMessage};
use fleet_proto::generated as wire;
use fleet_server::app::agents::ingest::{IngestBounds, Tick};
use fleet_server::app::session_tokens::SessionTokenError;
use fleet_server::ports::msauth::Failure;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{Settings, hello, minecraft_session, profile, session_request, start};

#[sqlx::test(migrations = false)]
async fn no_log_line_holds_a_session_token_its_name_or_its_uuid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    const SECRET: &str = "mc-token-that-must-never-be-logged-3e9c1d";
    const NAME: &str = "HiddenName7";
    fleet_testkit::log_capture::install().unwrap();
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (_, bot) = port.online_bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await.len(), 1);
    port.minecraft
        .answer(Ok(minecraft_session(SECRET, &profile(1), NAME)));
    port.minecraft
        .answer(Err(SessionTokenError::Microsoft(Failure::RateLimited)));

    call.send(session_request(1, bot, false)).await;
    let grant = call.next().await;
    call.send(session_request(2, bot, true)).await;
    let refused = call.next().await;
    let unheld_bot = BotId::new_v7(port.clock.now(), [9; 10]).unwrap();
    call.send(session_request(3, unheld_bot, false)).await;
    let unheld = call.next().await;
    call.close();
    let _ = port.disconnect(&agent).await;
    port.stop().await;

    assert!(
        matches!(grant, Ok(Some(ServerMessage::SessionGrant(_)))),
        "{grant:?}"
    );
    assert!(
        matches!(refused, Ok(Some(ServerMessage::SessionDenied(_)))),
        "{refused:?}"
    );
    assert!(
        matches!(unheld, Ok(Some(ServerMessage::SessionDenied(_)))),
        "{unheld:?}"
    );
    let uuid = profile(1);
    for secret in [
        SECRET,
        NAME,
        "AfkOnline1",
        uuid.as_str(),
        &uuid.replace('-', ""),
    ] {
        fleet_testkit::log_capture::check_absent(secret).unwrap();
    }
    // The lines that are there name the grant and the denials.
    assert_eq!(
        fleet_testkit::log_capture::count(
            "fleet_server::app::agents::session_grants",
            "a session was granted"
        ),
        1
    );
    assert!(
        fleet_testkit::log_capture::count(
            "fleet_server::app::agents::session_grants",
            "a session request was denied"
        ) >= 1
    );
}

#[sqlx::test(migrations = false)]
async fn no_log_line_holds_the_peer_a_hellos_name_or_a_certificate(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    fleet_testkit::log_capture::install().unwrap();
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let revoked = port.enroll(2, "agent-2").await;
    port.revoke(&revoked).await;
    let stranger = port.stranger(3);

    let (mut call, _) = port.welcomed(&agent).await;
    call.close();
    let _ = call.next().await;
    let mut named = port.open(&agent).await.unwrap();
    named.send(hello("intruder-name-7f3a")).await;
    let _ = named.rejected().await;
    let _ = port.open(&revoked).await.unwrap_err();
    let _ = port.open(&stranger).await.unwrap_err();
    let _ = port.disconnect(&agent).await;
    port.stop().await;

    fleet_testkit::log_capture::check_absent("intruder-name-7f3a").unwrap();
    for certificate in [&agent, &revoked, &stranger] {
        let hex = certificate.der[20..44]
            .iter()
            .fold(String::new(), |mut hex, byte| {
                write!(hex, "{byte:02x}").unwrap();
                hex
            });
        let bytes = format!("{:?}", &certificate.der[20..44]);
        let base64 = certificate.pem.lines().nth(2).unwrap();
        for form in [hex.as_str(), &bytes[1..bytes.len() - 1], base64] {
            fleet_testkit::log_capture::check_absent(form).unwrap();
        }
    }
    // The test's own client logs the address it dials; no target of the
    // server's side does.
    let peer = fleet_testkit::log_capture::check_absent("127.0.0.1")
        .err()
        .map(|leak| leak.hits.into_keys().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        peer.iter()
            .all(|target| target == "hyper_util::client::legacy::connect::http"),
        "{peer:?}"
    );
    // The lines that are there name the agent.
    assert!(
        fleet_testkit::log_capture::count("fleet_server::grpc::control::session", "agent-1") >= 2,
        "the stream's lines name the agent"
    );
}

#[sqlx::test(migrations = false)]
async fn no_log_line_holds_a_chats_text_or_sender(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    const TEXT: &str = "chat-text-never-logged-5b2e";
    const SENDER: &str = "HiddenSender9";
    const COMMAND: &str = "/mode-command-never-logged-c41a";
    fleet_testkit::log_capture::install().unwrap();
    let port = start(
        options,
        Settings {
            ingest: IngestBounds {
                queue: 10.try_into().unwrap(),
                stream: 2.try_into().unwrap(),
            },
            ..Settings::reconciling()
        },
    )
    .await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await.len(), 1);
    let unheld_bot = BotId::new_v7(port.clock.now(), [9; 10]).unwrap();
    let whisper = IncomingChat::player(PlayerChatKind::Whisper, SENDER, None, TEXT);

    // Stored, stored, past the stream's share, and of a bot it doesn't hold.
    call.send(chat(bot, BotEventKind::ChatReceived(whisper.clone())))
        .await;
    call.send(chat(
        bot,
        BotEventKind::ModeChatSent(ChatMessage::try_from(COMMAND).unwrap()),
    ))
    .await;
    call.send(chat(bot, BotEventKind::ChatReceived(whisper.clone())))
        .await;
    call.send(chat(unheld_bot, BotEventKind::ChatReceived(whisper)))
        .await;
    call.send(session_request(1, unheld_bot, false)).await;
    let barrier = call.next().await;
    let tick = port.ingest_tick().await;
    call.close();
    let _ = port.disconnect(&agent).await;
    port.stop().await;

    assert!(
        matches!(barrier, Ok(Some(ServerMessage::SessionDenied(_)))),
        "{barrier:?}"
    );
    assert_eq!(
        tick,
        Tick::Written {
            stored: 2,
            skipped: 0,
            sticky: 0
        }
    );
    for secret in [TEXT, SENDER, COMMAND] {
        fleet_testkit::log_capture::check_absent(secret).unwrap();
    }
    // The drop is logged, by IDs alone.
    assert_eq!(
        fleet_testkit::log_capture::count(
            "fleet_server::grpc::control::session",
            "an agent's chat message was dropped"
        ),
        1
    );
}

fn chat(bot: BotId, kind: BotEventKind) -> wire::AgentMessage {
    wire::AgentMessage::try_from(AgentMessage::Event(BotEvent {
        bot_id: bot,
        at: DateTime::from_timestamp_millis(1_790_000_000_000).unwrap(),
        kind,
    }))
    .unwrap()
}
