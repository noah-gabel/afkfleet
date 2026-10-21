//! The managed run's link against the fake server (Plan.md P10.7; ADR-0019,
//! S65–S67, R48–R53), in real time: the hello, the welcome, the ends and
//! their exit codes, reconnects, resyncs and outages.

use core::time::Duration;

use fleet_agent::run::Exit;
use fleet_agent::signals::Signal;
use fleet_core::bot::{BotAccount, BotSpec, BotState, DesiredRunState};
use fleet_core::chat::ChatMessage;
use fleet_core::disconnect::{ConflictTexts, DisconnectReason};
use fleet_core::id::BotId;
use fleet_core::id::ChatRequestId;
use fleet_core::mc::SessionEvent;
use fleet_core::mode::ModePreset;
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{AgentMessage, Assignment, RejectReason, SendChat, ServerMessage};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::mc::EmitOutcome;
use tokio::time::Instant;
use tonic::Status;
use tonic::metadata::MetadataValue;

use crate::agent::{Agent, Options, SHORT, agent};
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
            chat_rate: fleet_core::chat::ChatRate::default(),
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
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            timings: SHORT,
            ..Options::default()
        },
    );

    let silent = server.pending().await.accept();
    let started = Instant::now();
    let again = server.pending().await;
    let took = started.elapsed();
    let outcome = running.stop().await;

    assert!(took >= SHORT.welcome, "{took:?}");
    assert!(took < SHORT.welcome + secs(5), "{took:?}");
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

/// The wipe's line: the kept grants forgotten at a refusal.
const WIPED: &str = "every kept session grant was forgotten";

/// The line of a stream's end, as the credentials note it.
const END_NOTED: &str = "the control stream's end was noted";

/// The kept grants were forgotten, `forgotten` of them, before the last
/// stream's end was noted (S63): nothing could hand one out after the
/// server's refusal.
fn assert_wiped_before_the_end(logs: &LogBuffer, forgotten: u64) {
    let lines = logs.json_lines().unwrap();
    let wiped = lines
        .iter()
        .position(|line| line["message"] == WIPED)
        .unwrap_or_else(|| panic!("no wipe: {}", logs.text()));
    let noted = lines
        .iter()
        .rposition(|line| line["message"] == END_NOTED)
        .unwrap_or_else(|| panic!("no stream end: {}", logs.text()));
    assert_eq!(lines[wiped]["forgotten"], forgotten, "{}", logs.text());
    assert!(
        wiped < noted,
        "the stream's end came before the wipe: {}",
        logs.text()
    );
}

#[rstest::rstest]
#[case::disabled(RejectReason::AgentDisabled)]
#[case::revoked(RejectReason::CertificateRevoked)]
#[case::expired(RejectReason::CertificateExpired)]
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
    let logs = running.logs.clone();

    call.send(ServerMessage::Reject(reason)).await;
    let outcome = running.outcome().await;

    assert_eq!(outcome.exit, Exit::Refused);
    assert_eq!(outcome.exit.code(), 5);
    assert_eq!(outcome.report.map(|report| report.stopped), Some(1));
    assert!(fake.try_session(0).unwrap().is_torn_down());
    assert_wiped_before_the_end(&logs, 1);
}

#[tokio::test]
async fn a_refusal_before_the_welcome_exits_5_without_a_second_dial() {
    let dir = test_dir("control", "refused_first");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let logs = running.logs.clone();

    let call = server.pending().await.accept();
    call.send(ServerMessage::Reject(RejectReason::AgentDisabled))
        .await;
    let outcome = running.outcome().await;
    let again = server.pending_within(secs(3)).await;

    assert_eq!(outcome.exit, Exit::Refused);
    assert!(again.is_none());
    assert_wiped_before_the_end(&logs, 0);
}

#[rstest::rstest]
#[case::bare(None)]
#[case::with_the_servers_request_id(Some("0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"))]
#[tokio::test]
async fn unauthenticated_at_the_gate_exits_5_without_a_second_dial(
    #[case] request_id: Option<&'static str>,
) {
    let dir = test_dir(
        "control",
        &format!("unauthenticated_{}", request_id.is_some()),
    );
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let logs = running.logs.clone();
    let mut status = Status::unauthenticated("the server's own words");
    if let Some(id) = request_id {
        status.metadata_mut().insert(
            fleet_proto::enrollment::REQUEST_ID,
            MetadataValue::from_static(id),
        );
    }

    server.pending().await.refuse(status);
    let outcome = running.outcome().await;
    let again = server.pending_within(secs(3)).await;

    assert_eq!(outcome.exit, Exit::Refused);
    assert!(again.is_none());
    assert_wiped_before_the_end(&logs, 0);
}

#[rstest::rstest]
#[case::incompatible(RejectReason::IncompatibleProtocol)]
#[case::invalid_hello(RejectReason::InvalidHello)]
#[tokio::test]
async fn a_hello_the_server_cant_read_waits_the_plateau(#[case] reason: RejectReason) {
    let dir = test_dir("control", &format!("slow_{reason:?}"));
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let logs = running.logs.clone();

    let call = server.pending().await.accept();
    call.send(ServerMessage::Reject(reason)).await;
    // A backoff's first wait is at most 2 s; the plateau's at least 15 s.
    let again = server.pending_within(secs(5)).await;
    let outcome = running.stop().await;

    assert!(again.is_none(), "dialled again before the plateau");
    assert_eq!(outcome.exit, Exit::Stopped);
    let slow = logs.lines_with(SLOW).unwrap();
    assert_eq!(slow.len(), 1, "{}", logs.text());
    assert_eq!(slow[0]["level"], "ERROR");
    let wait = slow[0]["wait_ms"].as_u64().unwrap();
    assert!((15_000..=30_000).contains(&wait), "{wait}");
}

/// The slow retry's line.
const SLOW: &str = "the server can't read this agent's hello; it tries again slowly: is the agent older or newer than the server?";

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
    let running = Agent::launch(
        &dir,
        &proxy.url(),
        &ca,
        Options {
            timings: SHORT,
            ..Options::default()
        },
    );
    let mut call = server.pending().await.accept();
    one_bot_online(&running, &mut call).await;

    proxy.go_silent();
    let silenced = Instant::now();
    let again = server.pending().await;
    let took = silenced.elapsed();
    let still_online = !running.fake.try_session(0).unwrap().is_torn_down();
    let outcome = running.stop().await;

    let bound = SHORT.keepalive_interval + SHORT.keepalive_timeout + secs(3);
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

/// `message` with its first spec's first step on a schedule case the reader
/// doesn't know: prost skips the case's field, so the schedule reads as
/// unset, as fleet-proto's `rejects` tests show (ADR-0020, S25).
fn with_an_unknown_schedule(message: ServerMessage) -> fleet_proto::generated::ServerMessage {
    use fleet_proto::generated as wire;
    use wire::server_message::Msg as M;
    let mut wire = wire::ServerMessage::try_from(message).unwrap();
    let spec = match wire.msg.as_mut().unwrap() {
        M::Reconcile(full) => full.bots[0].spec.as_mut().unwrap(),
        M::Assign(assign) => assign.spec.as_mut().unwrap(),
        M::Update(update) => update.spec.as_mut().unwrap(),
        other => panic!("no spec in {other:?}"),
    };
    spec.mode.as_mut().unwrap().steps[0].schedule = Some(wire::Schedule { schedule: None });
    wire
}

/// A spec with a schedule case the agent doesn't know is refused whole:
/// the stream ends as unreadable, nothing in the message is applied, no bot
/// starts, and the line says so at `error`, naming the field, never a value
/// (ADR-0020, S25).
#[rstest::rstest]
#[case::reconcile("reconcile", ServerMessage::Reconcile(vec![assignment(1)]))]
#[case::assign("assign", ServerMessage::Assign(assignment(1)))]
#[case::update("update", ServerMessage::Update(assignment(1).spec))]
#[tokio::test]
async fn a_spec_with_a_schedule_this_agent_doesnt_know_is_refused_whole(
    #[case] name: &str,
    #[case] message: ServerMessage,
) {
    let dir = test_dir("control", &format!("unknown_schedule_{name}"));
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(10)).await;

    call.send_raw(with_an_unknown_schedule(message)).await;
    let ended = call.until_ended(crate::server::PATIENCE).await;
    let again = server.pending().await;
    let lines = running
        .logs
        .lines_with("a message from the server couldn't be read")
        .unwrap();
    let ends = running
        .logs
        .lines_with("the control stream ended; the agent reconnects")
        .unwrap();
    let no_bot = running.fake.try_session(0).is_none();
    let outcome = running.stop().await;

    assert!(ended.is_some(), "the agent ended its request stream");
    assert_eq!(again.hello.running, [], "nothing was applied");
    assert!(no_bot, "no bot started");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "ERROR");
    let error = lines[0]["error"].as_str().unwrap();
    assert!(error.contains("Schedule.schedule"), "{error}");
    assert!(!error.contains("AfkBot1"), "{error}");
    assert!(
        ends.iter().any(|end| end["cause"] == "unreadable"),
        "{ends:?}"
    );
    assert_eq!(outcome.exit, Exit::Stopped);
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
            chat_rate: fleet_core::chat::ChatRate::default(),
        },
        restore: Some(StickyState::Failed(FailReason::Permanent {
            kind: PermanentKind::Banned,
        })),
    }
}

#[tokio::test]
async fn a_reconcile_past_tonics_default_4_mib_is_read() {
    // Built and measured before the agent dials: encoding it takes seconds
    // in a slow debug build, and the welcome timeout counts from the dial.
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
    let dir = test_dir("control", "large_reconcile");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();

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

#[tokio::test]
async fn an_overflowing_outbox_never_turns_a_refusal_into_a_reconnect() {
    let dir = test_dir("control", "overflow_refused");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    one_bot_online(&running, &mut call).await;
    call.fail(Status::unavailable("the server restarts")).await;
    // The next stream is welcomed and never reconciled, so the answers to
    // chat for a bot the agent doesn't run wait in the outbox, past its cap.
    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(10)).await;
    for n in 0..=150_u8 {
        call.send(ServerMessage::SendChat(SendChat {
            request: ChatRequestId::new_v7(chrono::DateTime::UNIX_EPOCH, [n; 10]).unwrap(),
            bot_id: bot(9),
            message: ChatMessage::try_from("hello").unwrap(),
        }))
        .await;
    }
    let overflowed = call.until_ended(secs(10)).await;
    let fake = running.fake.clone();
    let logs = running.logs.clone();

    call.send(ServerMessage::Reject(RejectReason::AgentDisabled))
        .await;
    let outcome = running.outcome().await;

    assert!(
        overflowed.is_some(),
        "the overflow ended the request stream"
    );
    assert_eq!(outcome.exit, Exit::Refused);
    assert_eq!(outcome.report.map(|report| report.stopped), Some(1));
    assert!(fake.try_session(0).unwrap().is_torn_down());
    assert_wiped_before_the_end(&logs, 1);
}

#[tokio::test]
async fn a_signal_ends_the_request_stream_and_no_heartbeat_follows() {
    let dir = test_dir("control", "signal_ends_stream");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let running = Agent::start(&dir, &server.url(), &ca);
    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(2)).await;
    call.reconcile(Vec::new()).await;
    assert_eq!(call.raw_next(secs(5)).await, Some(AgentMessage::Heartbeat));

    running.signal(Signal::Terminate).await;
    let read = call.until_ended(secs(5)).await;
    let outcome = running.outcome().await;

    let read = read.expect("the agent ended its request stream");
    assert!(!read.contains(&AgentMessage::Heartbeat), "{read:?}");
    assert_eq!(outcome.exit, Exit::Stopped);
}

/// The first of January of `year`, as the run's start.
fn new_year(year: i32) -> chrono::DateTime<chrono::Utc> {
    chrono::NaiveDate::from_ymd_opt(year, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
}

#[tokio::test]
async fn a_certificate_expired_by_the_runs_clock_stops_the_agent_with_exit_5() {
    let dir = test_dir("control", "own_expired");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    // Valid until 2090 by the host's clock at the start; the run's clock
    // reads 2091.
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            anchor: new_year(2091),
            ..Options::default()
        },
    );
    let logs = running.logs.clone();

    let outcome = running.outcome().await;
    let dialled = server.pending_within(secs(3)).await;

    assert_eq!(outcome.exit, Exit::Refused);
    assert!(dialled.is_none(), "it never dials");
    let expired = logs
        .lines_with(
            "this agent's certificate expired by its own clock; every bot stops, then the agent exits: enroll it again, or fix the clock (NTP)",
        )
        .unwrap();
    assert_eq!(expired.len(), 1, "{}", logs.text());
    assert_eq!(expired[0]["level"], "ERROR");
    assert_eq!(expired[0]["refusal"], "own_certificate_expired");
    assert_eq!(
        logs.lines_with("the server refused this agent; every bot stops, then the agent exits")
            .unwrap()
            .len(),
        0,
        "{}",
        logs.text()
    );
    let wiped = logs.lines_with(WIPED).unwrap();
    assert_eq!(wiped.len(), 1, "{}", logs.text());
}

#[tokio::test]
async fn a_certificate_not_yet_valid_by_the_runs_clock_backs_off_with_an_error_line() {
    let dir = test_dir("control", "own_not_yet_valid");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    // Valid from 2020 by the host's clock at the start; the run's clock
    // reads 2019.
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            anchor: new_year(2019),
            ..Options::default()
        },
    );
    let logs = running.logs.clone();

    let dialled = server.pending_within(secs(4)).await;
    let outcome = running.stop().await;

    assert!(dialled.is_none(), "it never dials");
    assert_eq!(outcome.exit, Exit::Stopped);
    let early = logs
        .lines_with(
            "the agent's certificate isn't valid yet by this host's clock; it tries again: fix the clock (NTP)",
        )
        .unwrap();
    assert_eq!(early.len(), 1, "{}", logs.text());
    assert_eq!(early[0]["level"], "ERROR");
    assert_eq!(early[0]["cause"], "not_yet_valid");
    assert_eq!(logs.lines_with(WIPED).unwrap().len(), 0, "{}", logs.text());
}
