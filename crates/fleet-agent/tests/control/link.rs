//! The managed run's link against the fake server (Plan.md P10.7; ADR-0019,
//! S65–S67, R48–R53), in real time: the hello, the welcome, the ends and
//! their exit codes, reconnects, resyncs and outages.

use core::time::Duration;

use fleet_agent::run::Exit;
use fleet_agent::signals::Signal;
use fleet_core::bot::{BotAccount, BotSpec, BotState, DesiredRunState};
use fleet_core::disconnect::{ConflictTexts, DisconnectReason};
use fleet_core::id::BotId;
use fleet_core::mc::SessionEvent;
use fleet_core::mode::ModePreset;
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{AgentMessage, Assignment, RejectReason, ServerMessage};
use fleet_testkit::mc::EmitOutcome;
use tokio::time::Instant;
use tonic::Status;
use tonic::metadata::MetadataValue;

use crate::agent::{Agent, TIMINGS, agent};
use crate::pki::{TestCa, test_dir};
use crate::server::{Call, FakeServer};

const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

fn bot(n: u8) -> BotId {
    BotId::new_v7(chrono::DateTime::UNIX_EPOCH, [n; 10]).unwrap()
}

fn assignment(n: u8) -> Assignment {
    Assignment {
        spec: BotSpec {
            id: bot(n),
            account: BotAccount::Offline(format!("AfkBot{n}").parse().unwrap()),
            server: "mc.example.com".try_into().unwrap(),
            mode: ModePreset::Afk.definition(),
            desired: DesiredRunState::Running,
            conflict_texts: ConflictTexts::default(),
        },
        restore: None,
    }
}

/// Whether `message` is bot `n`'s status in `matches`'s state.
fn online(message: &AgentMessage, n: u8) -> Option<()> {
    match message {
        AgentMessage::Status(status)
            if status.bot_id == bot(n) && matches!(status.state, BotState::Online { .. }) =>
        {
            Some(())
        }
        _ => None,
    }
}

/// A welcomed stream with bot 1 online on it.
async fn one_bot_online(agent: &Agent, call: &mut Call) {
    call.welcome(crate::agent::agent(), secs(10)).await;
    call.reconcile(vec![assignment(1)]).await;
    // The call answers the bot's session request while it joins.
    let joining = async {
        let session = tokio::time::timeout(secs(10), agent.fake.session(0))
            .await
            .expect("the bot connects");
        session.emit(SessionEvent::Joined)
    };
    let (joined, ()) = tokio::join!(joining, call.until(|message| online(message, 1)));
    assert_eq!(joined, EmitOutcome::Queued);
}

#[tokio::test]
async fn the_agent_says_hello_over_mtls_with_its_name_its_capacity_and_no_bots() {
    let dir = test_dir("control", "hello");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);

    let pending = server.pending().await;
    let hello = pending.hello.clone();
    let certificates = pending.certificates.clone();
    let call = pending.accept();
    call.welcome(agent(), secs(10)).await;
    let outcome = running.stop().await;

    assert_eq!(hello.name.as_str(), "agent-1");
    assert_eq!(hello.capacity.get(), 4);
    assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
    assert_eq!(hello.running, []);
    assert!(!certificates.contains(&ca.der), "its own certificate alone");
    if let Some(presented) = certificates.first() {
        let pem = std::fs::read_to_string(dir.join("agent.crt")).unwrap();
        let expected: rustls::pki_types::CertificateDer<'static> =
            rustls::pki_types::pem::PemObject::from_pem_slice(pem.as_bytes()).unwrap();
        assert_eq!(presented.as_slice(), expected.as_ref());
    }
    assert_eq!(outcome.exit, Exit::Stopped);
    drop(call);
}

#[tokio::test]
async fn a_reconciled_bot_asks_for_its_session_runs_and_reports_online() {
    let dir = test_dir("control", "reconcile");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();

    one_bot_online(&running, &mut call).await;
    let connected = running.logs.lines_with("the agent is connected").unwrap();
    let outcome = running.stop().await;

    assert_eq!(connected.len(), 1);
    assert_eq!(connected[0]["agent_id"], agent().to_string());
    assert_eq!(outcome.exit, Exit::Stopped);
    assert_eq!(outcome.report.map(|report| report.stopped), Some(1));
}

#[tokio::test]
async fn no_welcome_means_a_redial_after_the_welcome_timeout() {
    let dir = test_dir("control", "no_welcome");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);

    let silent = server.pending().await.accept();
    let started = Instant::now();
    let again = server.pending().await;
    let took = started.elapsed();
    let outcome = running.stop().await;

    assert!(took >= TIMINGS.welcome, "{took:?}");
    assert!(took < TIMINGS.welcome + secs(5), "{took:?}");
    assert_eq!(outcome.exit, Exit::Stopped);
    drop((silent, again));
}

#[tokio::test]
async fn a_heartbeat_goes_after_one_welcome_interval_of_nothing() {
    let dir = test_dir("control", "heartbeat");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(1)).await;
    call.reconcile(Vec::new()).await;

    let beat = call.raw_next(secs(5)).await;
    let outcome = running.stop().await;

    assert_eq!(beat, Some(AgentMessage::Heartbeat));
    assert_eq!(outcome.exit, Exit::Stopped);
}

#[rstest::rstest]
#[case::disabled(RejectReason::AgentDisabled)]
#[case::revoked(RejectReason::CertificateRevoked)]
#[case::name(RejectReason::NameMismatch)]
#[tokio::test]
async fn a_refusal_stops_every_bot_and_exits_5(#[case] reason: RejectReason) {
    let dir = test_dir("control", &format!("refused_{reason:?}"));
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    one_bot_online(&running, &mut call).await;
    let fake = running.fake.clone();

    call.send(ServerMessage::Reject(reason)).await;
    let outcome = running.outcome().await;

    assert_eq!(outcome.exit, Exit::Refused);
    assert_eq!(outcome.exit.code(), 5);
    assert_eq!(outcome.report.map(|report| report.stopped), Some(1));
    assert!(fake.try_session(0).unwrap().is_torn_down());
}

#[tokio::test]
async fn unauthenticated_at_the_gate_exits_5_without_a_second_dial() {
    let dir = test_dir("control", "unauthenticated");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);

    server
        .pending()
        .await
        .refuse(Status::unauthenticated("the server's own words"));
    let outcome = running.outcome().await;
    let again = server.pending_within(secs(3)).await;

    assert_eq!(outcome.exit, Exit::Refused);
    assert!(again.is_none());
}

#[tokio::test]
async fn already_connected_backs_off_and_dials_again() {
    let dir = test_dir("control", "already_connected");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let call = server.pending().await.accept();

    call.send(ServerMessage::Reject(RejectReason::AlreadyConnected))
        .await;
    let again = server.pending_within(secs(10)).await;
    let outcome = running.stop().await;

    assert!(again.is_some());
    assert_eq!(outcome.exit, Exit::Stopped);
}

#[tokio::test]
async fn a_rate_limit_waits_its_retry_after() {
    let dir = test_dir("control", "retry_after");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut status = Status::resource_exhausted("slow down");
    status
        .metadata_mut()
        .insert("retry-after", MetadataValue::from_static("4"));

    server.pending().await.refuse(status);
    let refused_at = Instant::now();
    let again = server.pending().await;
    let took = refused_at.elapsed();
    let outcome = running.stop().await;

    assert!(took >= secs(4), "{took:?}");
    assert_eq!(outcome.exit, Exit::Stopped);
    drop(again);
}

#[tokio::test]
async fn a_cut_stream_reconnects_with_a_hello_that_lists_the_running_bot() {
    let dir = test_dir("control", "cut");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    one_bot_online(&running, &mut call).await;

    call.fail(Status::unavailable("the server is shutting down"))
        .await;
    let again = server.pending().await;
    let listed: Vec<BotId> = again.hello.running.iter().map(|bot| bot.bot_id).collect();
    let session = running.fake.try_session(0).unwrap();
    let outcome = running.stop().await;

    assert_eq!(listed, [bot(1)]);
    assert!(
        matches!(again.hello.running[0].state, BotState::Online { .. }),
        "{:?}",
        again.hello.running
    );
    assert!(session.is_torn_down(), "stopped at the signal");
    assert_eq!(outcome.exit, Exit::Stopped);
}

#[tokio::test]
async fn a_silent_connection_is_noticed_within_the_keepalive_and_the_agent_resyncs() {
    let dir = test_dir("control", "silent");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let proxy = crate::proxy::Proxy::start(server.address).await;
    let running = Agent::start(&dir, &proxy.url(), &ca);
    let mut call = server.pending().await.accept();
    one_bot_online(&running, &mut call).await;

    proxy.go_silent();
    let silenced = Instant::now();
    let again = server.pending().await;
    let took = silenced.elapsed();
    let still_online = !running.fake.try_session(0).unwrap().is_torn_down();
    let outcome = running.stop().await;

    let bound = TIMINGS.keepalive_interval + TIMINGS.keepalive_timeout + secs(3);
    assert!(took <= bound, "{took:?} > {bound:?}");
    assert!(still_online, "the bot kept running through the outage");
    assert_eq!(
        again
            .hello
            .running
            .iter()
            .map(|bot| bot.bot_id)
            .collect::<Vec<_>>(),
        [bot(1)]
    );
    assert_eq!(outcome.exit, Exit::Stopped);
    drop(call);
}

#[tokio::test]
async fn a_kicked_bot_rejoins_with_its_kept_grant_while_the_server_is_down() {
    let dir = test_dir("control", "kept_grant");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    one_bot_online(&running, &mut call).await;

    // The server goes away; then the Minecraft server kicks the bot.
    drop(call);
    server.stop().await;
    let session = running.fake.try_session(0).unwrap();
    let kick = DisconnectReason::kicked(Some("multiplayer.disconnect.kicked"), "kicked");
    assert_eq!(
        session.emit(SessionEvent::Disconnected(kick)),
        EmitOutcome::Queued
    );
    let rejoined = tokio::time::timeout(secs(20), running.fake.session(1)).await;
    let outcome = running.stop().await;

    assert!(
        rejoined.is_ok(),
        "the bot connected again without the server"
    );
    assert_eq!(outcome.exit, Exit::Stopped);
}

#[tokio::test]
async fn a_signal_while_the_agent_reconnects_ends_it_at_once() {
    let dir = test_dir("control", "signal_reconnecting");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let url = server.url();
    let running = Agent::start(&dir, &url, &ca);
    server
        .pending()
        .await
        .refuse(Status::unavailable("the server is shutting down"));
    server.stop().await;

    let started = Instant::now();
    running.signal(Signal::Interrupt).await;
    let outcome = running.outcome().await;

    assert_eq!(outcome.exit, Exit::Stopped);
    assert!(started.elapsed() < secs(5), "{:?}", started.elapsed());
}

#[tokio::test]
async fn no_agent_line_holds_the_servers_address_or_its_message() {
    let dir = test_dir("control", "logs");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let port = server.address.port().to_string();

    server
        .pending()
        .await
        .refuse(Status::unavailable("the-servers-own-words"));
    let _ = server.pending().await;
    let logs = running.logs.clone();
    let _ = running.stop().await;

    let agent_lines: Vec<String> = logs
        .text()
        .lines()
        .filter(|line| line.contains("\"target\":\"fleet_agent"))
        .map(str::to_owned)
        .collect();
    assert_ne!(agent_lines, Vec::<String>::new());
    for line in agent_lines {
        assert!(!line.contains("the-servers-own-words"), "{line}");
        assert!(!line.contains(&format!(":{port}")), "{line}");
    }
}

/// Bot `n`'s spec with every field at its cap but the account, given up
/// for good, so the agent never connects it.
fn largest(n: u16) -> Assignment {
    use fleet_core::bot::{FailReason, StickyState};
    use fleet_core::chat::ChatMessage;
    use fleet_core::disconnect::PermanentKind;
    use fleet_core::mode::{Action, ModeDefinition, ModeDraft, Schedule, Step};

    let wide = |len: usize| -> String { core::iter::repeat_n('\u{20AC}', len).collect() };
    let every = Schedule::Every {
        interval: ModeDefinition::MAX_INTERVAL,
        jitter: ModeDefinition::MAX_JITTER,
    };
    let chat = Step {
        action: Action::SendChat {
            message: ChatMessage::try_from(wide(ChatMessage::MAX_LEN).as_str()).unwrap(),
        },
        schedule: every,
        probability: 100,
    };
    let at_start = Step {
        schedule: Schedule::AtStart,
        ..chat.clone()
    };
    let mut steps = vec![at_start, chat];
    while steps.len() < ModeDefinition::MAX_STEPS {
        steps.push(Step {
            action: Action::Look {
                yaw: -ModeDefinition::MAX_YAW,
                pitch: -ModeDefinition::MAX_PITCH,
            },
            schedule: every,
            probability: 100,
        });
    }
    let texts: Vec<String> = (0..ConflictTexts::MAX_ENTRIES)
        .map(|entry| format!("{entry:02}{}", wide(ConflictTexts::MAX_LEN - 2)))
        .collect();
    let mut random = [0xAB; 10];
    random[..2].copy_from_slice(&n.to_be_bytes());
    Assignment {
        spec: BotSpec {
            id: BotId::new_v7(chrono::DateTime::UNIX_EPOCH, random).unwrap(),
            account: BotAccount::Offline(format!("AfkBot{n}").parse().unwrap()),
            server: format!("{}.example.com:65535", "a".repeat(63))
                .parse()
                .unwrap(),
            mode: ModeDraft { steps }.validate().unwrap(),
            desired: DesiredRunState::Running,
            conflict_texts: ConflictTexts::try_new(&texts).unwrap(),
        },
        restore: Some(StickyState::Failed(FailReason::Permanent {
            kind: PermanentKind::Banned,
        })),
    }
}

#[tokio::test]
async fn a_reconcile_past_tonics_default_4_mib_is_read() {
    let dir = test_dir("control", "large_reconcile");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    let assignments: Vec<Assignment> = (0..400).map(largest).collect();
    let first = assignments[0].spec.id;
    let wire = fleet_proto::generated::ServerMessage::try_from(ServerMessage::Reconcile(
        assignments.clone(),
    ))
    .unwrap();
    assert!(
        fleet_proto::control::server_message_len(&wire) > 4 * 1024 * 1024,
        "the reconcile is past the default"
    );

    call.welcome(agent(), secs(10)).await;
    call.reconcile(assignments).await;
    call.until(|message| match message {
        AgentMessage::Status(status) if status.bot_id == first => Some(()),
        _ => None,
    })
    .await;
    let again = server.pending_within(secs(2)).await;
    let outcome = running.stop().await;

    assert!(again.is_none(), "the stream stayed");
    assert_eq!(outcome.exit, Exit::Stopped);
}
