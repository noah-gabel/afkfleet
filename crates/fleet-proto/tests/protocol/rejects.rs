//! Every rule that refuses a message, one case each: a valid message with one
//! field broken, and the error that names it.

use core::num::NonZeroU32;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotAccount, BotSnapshot, BotSpec, BotState, DesiredRunState};
use fleet_core::chat::{ChatMessageError, IncomingChatError};
use fleet_core::disconnect::{ConflictTexts, ConflictTextsError, DisconnectReason};
use fleet_core::mc::SessionCredentials;
use fleet_core::mode::{HotbarSlotError, LimitedStep, ModeDefinition, ModeError};
use fleet_core::value::{
    AgentCapacityError, AgentNameError, AgentVersionError, McUsernameError, ServerAddressError,
};
use fleet_proto::control::{
    AgentMessage, BotEvent, BotEventKind, ServerMessage, SessionGrant, SessionRequestId, Welcome,
};
use fleet_proto::generated as wire;
use fleet_proto::{ConvertError, Problem};
use rstest::rstest;
use secrecy::SecretString;
use uuid::Uuid;

const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";
const AGENT: &str = "018bcfe5-6800-7ccc-8ccc-cccccccccccc";
const OTHER_BOT: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd";
const ACCOUNT: &str = "018bcfe5-6800-7ddd-8ddd-dddddddddddd";
const REQUEST: &str = "018bcfe5-6800-7aaa-9aaa-aaaaaaaaaaaa";
const V4: &str = "550e8400-e29b-41d4-a716-446655440000";
const PROFILE: &str = "0123456789ab4def8123456789abcdef";
const AT: i64 = 1_790_000_000_000;

fn bytes(text: &str) -> Vec<u8> {
    text.parse::<Uuid>().unwrap().as_bytes().to_vec()
}

fn at() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(AT).unwrap()
}

fn error(field: &'static str, problem: Problem) -> ConvertError {
    ConvertError::new(field, problem)
}

fn spec() -> BotSpec {
    BotSpec {
        id: BOT.parse().unwrap(),
        account: BotAccount::Online(ACCOUNT.parse().unwrap()),
        server: "mc.example.com:25566".parse().unwrap(),
        mode: ModeDefinition::farm(),
        desired: DesiredRunState::Running,
        conflict_texts: ConflictTexts::try_new(&["Logged in from another location"]).unwrap(),
        // Not the default, so a reader that ignored the field would show.
        chat_rate: fleet_core::chat::ChatRate::try_new(5, 2).unwrap(),
    }
}

fn wire_spec() -> wire::BotSpec {
    spec().into()
}

fn snapshot(bot: &str) -> BotSnapshot {
    BotSnapshot {
        bot_id: bot.parse().unwrap(),
        state: BotState::Online {
            since: at(),
            attempt: NonZeroU32::MIN,
        },
        since: at(),
        last_disconnect: Some(DisconnectReason::kicked(
            Some("multiplayer.disconnect.duplicate_login"),
            "You logged in from another location",
        )),
    }
}

fn wire_snapshot() -> wire::BotSnapshot {
    snapshot(BOT).try_into().unwrap()
}

fn hello() -> wire::Hello {
    wire::Hello {
        name: "agent-1".to_owned(),
        agent_version: "0.1.0".to_owned(),
        protocol_version: 1,
        capacity: 50,
        running: vec![wire_snapshot()],
    }
}

fn agent(msg: wire::agent_message::Msg) -> wire::AgentMessage {
    wire::AgentMessage { msg: Some(msg) }
}

fn server(msg: wire::server_message::Msg) -> wire::ServerMessage {
    wire::ServerMessage { msg: Some(msg) }
}

fn event(kind: wire::bot_event::Kind) -> wire::BotEvent {
    wire::BotEvent {
        bot_id: bytes(BOT),
        at_unix_ms: Some(AT),
        kind: Some(kind),
    }
}

fn online_session() -> wire::OnlineSession {
    wire::OnlineSession {
        username: "AfkBot1".to_owned(),
        uuid: bytes(PROFILE),
        access_token: "eyJ.token-value".to_owned(),
        expires_at_unix_ms: Some(AT),
    }
}

/// The valid online session with one change. The message wipes its token
/// when dropped, so struct-update syntax can't move fields out of it.
fn online_with(change: fn(&mut wire::OnlineSession)) -> wire::OnlineSession {
    let mut session = online_session();
    change(&mut session);
    session
}

fn grant(session: wire::session_grant::Session) -> wire::SessionGrant {
    wire::SessionGrant {
        id: 1,
        bot_id: bytes(BOT),
        session: Some(session),
    }
}

#[test]
fn the_fixtures_are_valid() {
    assert_eq!(BotSpec::try_from(wire_spec()), Ok(spec()));
    assert_eq!(BotSnapshot::try_from(wire_snapshot()), Ok(snapshot(BOT)));
    assert!(AgentMessage::try_from(agent(wire::agent_message::Msg::Hello(hello()))).is_ok());
    assert!(
        ServerMessage::try_from(server(wire::server_message::Msg::SessionGrant(grant(
            wire::session_grant::Session::Online(online_session())
        ))))
        .is_ok()
    );
}

#[rstest]
#[case::id_too_short(|s: &mut wire::BotSpec| s.id = vec![1; 15], error("BotSpec.id", Problem::WrongLength { len: 15 }))]
#[case::id_too_long(|s: &mut wire::BotSpec| s.id = vec![1; 17], error("BotSpec.id", Problem::WrongLength { len: 17 }))]
#[case::id_not_v7(|s: &mut wire::BotSpec| s.id = bytes(V4), error("BotSpec.id", Problem::NotV7))]
#[case::no_account(|s: &mut wire::BotSpec| s.account = None, error("BotSpec.account", Problem::Missing))]
#[case::empty_account(|s: &mut wire::BotSpec| s.account = Some(wire::BotAccount { account: None }), error("BotAccount.account", Problem::Missing))]
#[case::bad_offline_name(
    |s: &mut wire::BotSpec| s.account = Some(wire::BotAccount { account: Some(wire::bot_account::Account::OfflineUsername("x".to_owned())) }),
    error("BotAccount.offline_username", Problem::McUsername(McUsernameError::InvalidLength { len: 1 }))
)]
#[case::online_account_not_v7(
    |s: &mut wire::BotSpec| s.account = Some(wire::BotAccount { account: Some(wire::bot_account::Account::OnlineAccountId(bytes(V4))) }),
    error("BotAccount.online_account_id", Problem::NotV7)
)]
#[case::server_with_a_scheme(|s: &mut wire::BotSpec| s.server = "tcp://mc.example.com".to_owned(), error("BotSpec.server", Problem::ServerAddress(ServerAddressError::Scheme)))]
#[case::server_not_exact(|s: &mut wire::BotSpec| s.server = "MC.example.com:25566".to_owned(), error("BotSpec.server", Problem::NotExact))]
#[case::no_mode(|s: &mut wire::BotSpec| s.mode = None, error("BotSpec.mode", Problem::Missing))]
#[case::desired_unspecified(|s: &mut wire::BotSpec| s.desired = 0, error("BotSpec.desired", Problem::Missing))]
#[case::desired_unknown(|s: &mut wire::BotSpec| s.desired = 3, error("BotSpec.desired", Problem::Unknown))]
#[case::conflict_text_with_a_section_sign(
    |s: &mut wire::BotSpec| s.conflict_texts = vec!["a§b".to_owned()],
    error("BotSpec.conflict_texts", Problem::ConflictTexts(ConflictTextsError::ForbiddenChar { index: 0, position: 1 }))
)]
#[case::too_many_conflict_texts(
    |s: &mut wire::BotSpec| s.conflict_texts = (0..17).map(|n| n.to_string()).collect(),
    error("BotSpec.conflict_texts", Problem::ConflictTexts(ConflictTextsError::TooMany { count: 17 }))
)]
#[case::no_chat_rate(|s: &mut wire::BotSpec| s.chat_rate = None, error("BotSpec.chat_rate", Problem::Missing))]
#[case::chat_interval_0(
    |s: &mut wire::BotSpec| s.chat_rate = Some(wire::ChatRate { interval_secs: 0, burst: 3 }),
    error("ChatRate.interval_secs", Problem::Missing)
)]
#[case::chat_interval_past_a_minute(
    |s: &mut wire::BotSpec| s.chat_rate = Some(wire::ChatRate { interval_secs: 61, burst: 3 }),
    error("ChatRate.interval_secs", Problem::OutOfRange)
)]
#[case::chat_burst_0(
    |s: &mut wire::BotSpec| s.chat_rate = Some(wire::ChatRate { interval_secs: 3, burst: 0 }),
    error("ChatRate.burst", Problem::Missing)
)]
#[case::chat_burst_past_eight(
    |s: &mut wire::BotSpec| s.chat_rate = Some(wire::ChatRate { interval_secs: 3, burst: 9 }),
    error("ChatRate.burst", Problem::OutOfRange)
)]
fn a_bot_spec_is_refused(#[case] mutate: fn(&mut wire::BotSpec), #[case] expected: ConvertError) {
    let mut spec = wire_spec();
    mutate(&mut spec);
    assert_eq!(BotSpec::try_from(spec), Err(expected));
}

fn step(spec: &mut wire::BotSpec, index: usize) -> &mut wire::Step {
    &mut spec.mode.as_mut().unwrap().steps[index]
}

#[rstest]
#[case::no_action(|s: &mut wire::BotSpec| step(s, 1).action = None, error("Step.action", Problem::Missing).at(1))]
#[case::empty_action(|s: &mut wire::BotSpec| step(s, 1).action = Some(wire::Action { action: None }), error("Action.action", Problem::Missing).at(1))]
#[case::no_schedule(|s: &mut wire::BotSpec| step(s, 0).schedule = None, error("Step.schedule", Problem::Missing).at(0))]
#[case::empty_schedule(|s: &mut wire::BotSpec| step(s, 0).schedule = Some(wire::Schedule { schedule: None }), error("Schedule.schedule", Problem::Missing).at(0))]
#[case::probability_too_big_for_a_byte(|s: &mut wire::BotSpec| step(s, 1).probability = 256, error("Step.probability", Problem::OutOfRange).at(1))]
#[case::slot_9(
    |s: &mut wire::BotSpec| step(s, 0).action = Some(wire::Action { action: Some(wire::action::Action::SelectHotbarSlot(wire::action::SelectHotbarSlot { slot: 9 })) }),
    error("Action.SelectHotbarSlot.slot", Problem::HotbarSlot(HotbarSlotError::OutOfRange)).at(0)
)]
#[case::slot_too_big_for_a_byte(
    |s: &mut wire::BotSpec| step(s, 0).action = Some(wire::Action { action: Some(wire::action::Action::SelectHotbarSlot(wire::action::SelectHotbarSlot { slot: 300 })) }),
    error("Action.SelectHotbarSlot.slot", Problem::OutOfRange).at(0)
)]
#[case::chat_not_exact(
    |s: &mut wire::BotSpec| step(s, 0).action = Some(wire::Action { action: Some(wire::action::Action::SendChat(wire::action::SendChat { message: " /spawn".to_owned() })) }),
    error("Action.SendChat.message", Problem::NotExact).at(0)
)]
#[case::chat_empty(
    |s: &mut wire::BotSpec| step(s, 0).action = Some(wire::Action { action: Some(wire::action::Action::SendChat(wire::action::SendChat { message: String::new() })) }),
    error("Action.SendChat.message", Problem::ChatMessage(ChatMessageError::Empty)).at(0)
)]
#[case::attack_too_often(
    |s: &mut wire::BotSpec| step(s, 1).schedule = Some(wire::Schedule { schedule: Some(wire::schedule::Schedule::Every(wire::schedule::Every { interval_ms: 100, jitter_ms: 0 })) }),
    error("ModeDefinition.steps", Problem::Mode(ModeError::IntervalTooShort { step: 1, min: Duration::from_millis(500) }))
)]
#[case::a_second_attack(
    |s: &mut wire::BotSpec| { let attack = step(s, 1).clone(); s.mode.as_mut().unwrap().steps.push(attack); },
    error("ModeDefinition.steps", Problem::Mode(ModeError::DuplicateStep { step: 2, kind: LimitedStep::RepeatingAttack }))
)]
#[case::too_many_steps(
    |s: &mut wire::BotSpec| { let jump = step(s, 0).clone(); s.mode.as_mut().unwrap().steps = vec![jump; 33]; },
    error("ModeDefinition.steps", Problem::Mode(ModeError::TooManySteps { count: 33 }))
)]
fn a_mode_is_refused(#[case] mutate: fn(&mut wire::BotSpec), #[case] expected: ConvertError) {
    let mut spec = wire_spec();
    mutate(&mut spec);
    assert_eq!(BotSpec::try_from(spec), Err(expected));
}

fn state(state: wire::bot_state::State) -> wire::BotState {
    wire::BotState { state: Some(state) }
}

#[rstest]
#[case::no_state(wire::BotState { state: None }, error("BotState.state", Problem::Missing))]
#[case::attempt_0_awaiting(
    state(wire::bot_state::State::AwaitingSession(wire::bot_state::AwaitingSession { attempt: 0, fresh: false })),
    error("BotState.AwaitingSession.attempt", Problem::Missing)
)]
#[case::attempt_0_connecting(
    state(wire::bot_state::State::Connecting(wire::bot_state::Connecting { attempt: 0, auth_retried: false })),
    error("BotState.Connecting.attempt", Problem::Missing)
)]
#[case::attempt_0_online(
    state(wire::bot_state::State::Online(wire::bot_state::Online { since_unix_ms: Some(AT), attempt: 0 })),
    error("BotState.Online.attempt", Problem::Missing)
)]
#[case::attempt_0_backoff(
    state(wire::bot_state::State::Backoff(wire::bot_state::Backoff { attempt: 0, fresh: true })),
    error("BotState.Backoff.attempt", Problem::Missing)
)]
#[case::online_without_a_time(
    state(wire::bot_state::State::Online(wire::bot_state::Online { since_unix_ms: None, attempt: 1 })),
    error("BotState.Online.since_unix_ms", Problem::Missing)
)]
#[case::online_out_of_range(
    state(wire::bot_state::State::Online(wire::bot_state::Online { since_unix_ms: Some(i64::MAX), attempt: 1 })),
    error("BotState.Online.since_unix_ms", Problem::OutOfRange)
)]
#[case::paused_unspecified(
    state(wire::bot_state::State::Paused(0)),
    error("BotState.paused", Problem::Missing)
)]
#[case::paused_unknown(
    state(wire::bot_state::State::Paused(2)),
    error("BotState.paused", Problem::Unknown)
)]
#[case::failed_unknown(
    state(wire::bot_state::State::Failed(9)),
    error("BotState.failed", Problem::Unknown)
)]
fn a_bot_state_is_refused(#[case] wire: wire::BotState, #[case] expected: ConvertError) {
    assert_eq!(BotState::try_from(wire), Err(expected));
}

#[rstest]
#[case::no_bot_id(|s: &mut wire::BotSnapshot| s.bot_id = Vec::new(), error("BotSnapshot.bot_id", Problem::WrongLength { len: 0 }))]
#[case::no_state(|s: &mut wire::BotSnapshot| s.state = None, error("BotSnapshot.state", Problem::Missing))]
#[case::no_time(|s: &mut wire::BotSnapshot| s.since_unix_ms = None, error("BotSnapshot.since_unix_ms", Problem::Missing))]
#[case::time_out_of_range(|s: &mut wire::BotSnapshot| s.since_unix_ms = Some(i64::MIN), error("BotSnapshot.since_unix_ms", Problem::OutOfRange))]
#[case::empty_disconnect(|s: &mut wire::BotSnapshot| s.last_disconnect = Some(wire::DisconnectReason { reason: None }), error("DisconnectReason.reason", Problem::Missing))]
#[case::connect_failure_unknown(
    |s: &mut wire::BotSnapshot| s.last_disconnect = Some(wire::DisconnectReason { reason: Some(wire::disconnect_reason::Reason::ConnectFailed(7)) }),
    error("DisconnectReason.connect_failed", Problem::Unknown)
)]
#[case::restriction_unspecified(
    |s: &mut wire::BotSnapshot| s.last_disconnect = Some(wire::DisconnectReason { reason: Some(wire::disconnect_reason::Reason::AccountRestricted(0)) }),
    error("DisconnectReason.account_restricted", Problem::Missing)
)]
#[case::session_server_failure_unknown(
    |s: &mut wire::BotSnapshot| s.last_disconnect = Some(wire::DisconnectReason { reason: Some(wire::disconnect_reason::Reason::SessionServerFailed(-1)) }),
    error("DisconnectReason.session_server_failed", Problem::Unknown)
)]
fn a_snapshot_is_refused(
    #[case] mutate: fn(&mut wire::BotSnapshot),
    #[case] expected: ConvertError,
) {
    let mut snapshot = wire_snapshot();
    mutate(&mut snapshot);
    assert_eq!(BotSnapshot::try_from(snapshot), Err(expected));
}

#[rstest]
#[case::no_state(wire::StickyState { state: None }, error("StickyState.state", Problem::Missing))]
#[case::paused_unspecified(
    wire::StickyState { state: Some(wire::sticky_state::State::Paused(0)) },
    error("StickyState.paused", Problem::Missing)
)]
#[case::failed_unknown(
    wire::StickyState { state: Some(wire::sticky_state::State::Failed(100)) },
    error("StickyState.failed", Problem::Unknown)
)]
fn a_sticky_state_is_refused(#[case] wire: wire::StickyState, #[case] expected: ConvertError) {
    assert_eq!(fleet_core::bot::StickyState::try_from(wire), Err(expected));
}

#[rstest]
#[case::no_notification(wire::BotNotification { notification: None }, error("BotNotification.notification", Problem::Missing))]
#[case::paused_unknown(
    wire::BotNotification { notification: Some(wire::bot_notification::Notification::Paused(5)) },
    error("BotNotification.paused", Problem::Unknown)
)]
#[case::failed_unspecified(
    wire::BotNotification { notification: Some(wire::bot_notification::Notification::Failed(0)) },
    error("BotNotification.failed", Problem::Missing)
)]
fn an_alert_is_refused(#[case] wire: wire::BotNotification, #[case] expected: ConvertError) {
    assert_eq!(
        fleet_core::bot::BotNotification::try_from(wire),
        Err(expected)
    );
}

fn chat(kind: wire::ChatKind, sender: Option<wire::ChatSender>) -> wire::IncomingChat {
    wire::IncomingChat {
        kind: kind.into(),
        sender,
        text: "hello".to_owned(),
        truncated: false,
    }
}

fn sender(uuid: Option<Vec<u8>>) -> wire::ChatSender {
    wire::ChatSender {
        name: "AfkBot2".to_owned(),
        uuid,
    }
}

#[rstest]
#[case::kind_unspecified(
    chat(wire::ChatKind::Unspecified, None),
    error("IncomingChat.kind", Problem::Missing)
)]
#[case::kind_unknown(
    wire::IncomingChat { kind: 6, ..chat(wire::ChatKind::System, None) },
    error("IncomingChat.kind", Problem::Unknown)
)]
#[case::system_with_a_sender(
    chat(wire::ChatKind::System, Some(sender(None))),
    error(
        "IncomingChat.sender",
        Problem::IncomingChat(IncomingChatError::SenderOnSystemMessage)
    )
)]
#[case::chat_without_a_sender(
    chat(wire::ChatKind::Chat, None),
    error(
        "IncomingChat.sender",
        Problem::IncomingChat(IncomingChatError::MissingSender)
    )
)]
#[case::sender_uuid_too_short(
    chat(wire::ChatKind::Chat, Some(sender(Some(vec![7; 3])))),
    error("ChatSender.uuid", Problem::WrongLength { len: 3 })
)]
fn received_chat_is_refused(#[case] wire: wire::IncomingChat, #[case] expected: ConvertError) {
    assert_eq!(
        fleet_core::chat::IncomingChat::try_from(wire),
        Err(expected)
    );
}

#[rstest]
#[case::no_message(wire::AgentMessage { msg: None }, error("AgentMessage.msg", Problem::Missing))]
#[case::bad_name(
    agent(wire::agent_message::Msg::Hello(wire::Hello { name: "_x".to_owned(), ..hello() })),
    error("Hello.name", Problem::AgentName(AgentNameError::InvalidStart))
)]
#[case::bad_version(
    agent(wire::agent_message::Msg::Hello(wire::Hello { agent_version: String::new(), ..hello() })),
    error("Hello.agent_version", Problem::AgentVersion(AgentVersionError::InvalidLength { len: 0 }))
)]
#[case::capacity_0(
    agent(wire::agent_message::Msg::Hello(wire::Hello { capacity: 0, ..hello() })),
    error("Hello.capacity", Problem::AgentCapacity(AgentCapacityError::OutOfRange { value: 0 }))
)]
#[case::capacity_1001(
    agent(wire::agent_message::Msg::Hello(wire::Hello { capacity: 1001, ..hello() })),
    error("Hello.capacity", Problem::AgentCapacity(AgentCapacityError::OutOfRange { value: 1001 }))
)]
#[case::too_many_bots(
    agent(wire::agent_message::Msg::Hello(wire::Hello { running: vec![wire_snapshot(); 1001], ..hello() })),
    error("Hello.running", Problem::TooMany { count: 1001, max: 1000 })
)]
#[case::a_bot_twice(
    agent(wire::agent_message::Msg::Hello(wire::Hello {
        running: vec![wire_snapshot(), snapshot(OTHER_BOT).try_into().unwrap(), wire_snapshot()],
        ..hello()
    })),
    error("Hello.running", Problem::Duplicate).at(2)
)]
#[case::a_bad_running_bot(
    agent(wire::agent_message::Msg::Hello(wire::Hello {
        running: vec![wire_snapshot(), wire::BotSnapshot { since_unix_ms: None, ..wire_snapshot() }],
        ..hello()
    })),
    error("BotSnapshot.since_unix_ms", Problem::Missing).at(1)
)]
#[case::status_without_a_state(
    agent(wire::agent_message::Msg::Status(wire::BotSnapshot { state: None, ..wire_snapshot() })),
    error("BotSnapshot.state", Problem::Missing)
)]
#[case::event_without_a_bot(
    agent(wire::agent_message::Msg::Event(wire::BotEvent { bot_id: vec![1; 4], ..event(wire::bot_event::Kind::Died(wire::Unit {})) })),
    error("BotEvent.bot_id", Problem::WrongLength { len: 4 })
)]
#[case::event_without_a_time(
    agent(wire::agent_message::Msg::Event(wire::BotEvent { at_unix_ms: None, ..event(wire::bot_event::Kind::Died(wire::Unit {})) })),
    error("BotEvent.at_unix_ms", Problem::Missing)
)]
#[case::event_without_a_kind(
    agent(wire::agent_message::Msg::Event(wire::BotEvent { kind: None, ..event(wire::bot_event::Kind::Died(wire::Unit {})) })),
    error("BotEvent.kind", Problem::Missing)
)]
#[case::chat_sent_request_not_v7(
    agent(wire::agent_message::Msg::Event(event(wire::bot_event::Kind::ChatSent(wire::ChatSent { request_id: bytes(V4) })))),
    error("ChatSent.request_id", Problem::NotV7)
)]
#[case::chat_failed_request_too_short(
    agent(wire::agent_message::Msg::Event(event(wire::bot_event::Kind::ChatFailed(wire::ChatFailed { request_id: vec![2; 8], reason: 1 })))),
    error("ChatFailed.request_id", Problem::WrongLength { len: 8 })
)]
#[case::chat_failed_reason_unspecified(
    agent(wire::agent_message::Msg::Event(event(wire::bot_event::Kind::ChatFailed(wire::ChatFailed { request_id: bytes(REQUEST), reason: 0 })))),
    error("ChatFailed.reason", Problem::Missing)
)]
#[case::chat_failed_reason_unknown(
    agent(wire::agent_message::Msg::Event(event(wire::bot_event::Kind::ChatFailed(wire::ChatFailed { request_id: bytes(REQUEST), reason: 12 })))),
    error("ChatFailed.reason", Problem::Unknown)
)]
#[case::mode_chat_not_exact(
    agent(wire::agent_message::Msg::Event(event(wire::bot_event::Kind::ModeChatSent(wire::ModeChatSent { message: "hi ".to_owned() })))),
    error("ModeChatSent.message", Problem::NotExact)
)]
#[case::mode_chat_with_a_control_character(
    agent(wire::agent_message::Msg::Event(event(wire::bot_event::Kind::ModeChatSent(wire::ModeChatSent { message: "a\u{7}b".to_owned() })))),
    error("ModeChatSent.message", Problem::ChatMessage(ChatMessageError::ForbiddenChar { index: 1 }))
)]
#[case::alert_without_a_reason(
    agent(wire::agent_message::Msg::Event(event(wire::bot_event::Kind::Alert(wire::BotNotification { notification: None })))),
    error("BotNotification.notification", Problem::Missing)
)]
#[case::session_request_0(
    agent(wire::agent_message::Msg::SessionRequest(wire::SessionRequest { id: 0, bot_id: bytes(BOT), fresh: false })),
    error("SessionRequest.id", Problem::Missing)
)]
#[case::session_request_bot_not_v7(
    agent(wire::agent_message::Msg::SessionRequest(wire::SessionRequest { id: 1, bot_id: bytes(V4), fresh: true })),
    error("SessionRequest.bot_id", Problem::NotV7)
)]
fn an_agent_message_is_refused(#[case] wire: wire::AgentMessage, #[case] expected: ConvertError) {
    assert_eq!(AgentMessage::try_from(wire), Err(expected));
}

fn assign(spec: wire::BotSpec) -> wire::AssignBot {
    wire::AssignBot {
        spec: Some(spec),
        restore: None,
    }
}

fn other_spec() -> wire::BotSpec {
    BotSpec {
        id: OTHER_BOT.parse().unwrap(),
        ..spec()
    }
    .into()
}

#[rstest]
#[case::no_message(wire::ServerMessage { msg: None }, error("ServerMessage.msg", Problem::Missing))]
#[case::heartbeat_too_often(
    server(wire::server_message::Msg::Welcome(wire::Welcome { heartbeat_interval_ms: 999, agent_id: bytes(AGENT) })),
    error("Welcome.heartbeat_interval_ms", Problem::OutOfRange)
)]
#[case::heartbeat_too_rare(
    server(wire::server_message::Msg::Welcome(wire::Welcome { heartbeat_interval_ms: 300_001, agent_id: bytes(AGENT) })),
    error("Welcome.heartbeat_interval_ms", Problem::OutOfRange)
)]
#[case::welcome_without_an_agent(
    server(wire::server_message::Msg::Welcome(wire::Welcome { heartbeat_interval_ms: 10_000, agent_id: Vec::new() })),
    error("Welcome.agent_id", Problem::WrongLength { len: 0 })
)]
#[case::welcome_of_an_agent_id_that_isnt_v7(
    server(wire::server_message::Msg::Welcome(wire::Welcome { heartbeat_interval_ms: 10_000, agent_id: bytes(V4) })),
    error("Welcome.agent_id", Problem::NotV7)
)]
#[case::too_many_bots(
    server(wire::server_message::Msg::Reconcile(wire::ReconcileFull { bots: vec![assign(wire_spec()); 1001] })),
    error("ReconcileFull.bots", Problem::TooMany { count: 1001, max: 1000 })
)]
#[case::a_bot_twice(
    server(wire::server_message::Msg::Reconcile(wire::ReconcileFull { bots: vec![assign(wire_spec()), assign(other_spec()), assign(wire_spec())] })),
    error("ReconcileFull.bots", Problem::Duplicate).at(2)
)]
#[case::a_bad_bot_in_a_reconcile(
    server(wire::server_message::Msg::Reconcile(wire::ReconcileFull { bots: vec![assign(other_spec()), assign(wire::BotSpec { desired: 0, ..wire_spec() })] })),
    error("BotSpec.desired", Problem::Missing).at(1)
)]
#[case::assign_without_a_spec(
    server(wire::server_message::Msg::Assign(wire::AssignBot { spec: None, restore: None })),
    error("AssignBot.spec", Problem::Missing)
)]
#[case::assign_with_a_bad_restore(
    server(wire::server_message::Msg::Assign(wire::AssignBot { spec: Some(wire_spec()), restore: Some(wire::StickyState { state: None }) })),
    error("StickyState.state", Problem::Missing)
)]
#[case::unassign_not_v7(
    server(wire::server_message::Msg::Unassign(wire::UnassignBot { bot_id: bytes(V4) })),
    error("UnassignBot.bot_id", Problem::NotV7)
)]
#[case::update_without_a_spec(
    server(wire::server_message::Msg::Update(wire::UpdateBotSpec { spec: None })),
    error("UpdateBotSpec.spec", Problem::Missing)
)]
#[case::send_chat_request_not_v7(
    server(wire::server_message::Msg::SendChat(wire::SendChat { request_id: bytes(V4), bot_id: bytes(BOT), message: "hi".to_owned() })),
    error("SendChat.request_id", Problem::NotV7)
)]
#[case::send_chat_bot_too_long(
    server(wire::server_message::Msg::SendChat(wire::SendChat { request_id: bytes(REQUEST), bot_id: vec![3; 20], message: "hi".to_owned() })),
    error("SendChat.bot_id", Problem::WrongLength { len: 20 })
)]
#[case::send_chat_too_long(
    server(wire::server_message::Msg::SendChat(wire::SendChat { request_id: bytes(REQUEST), bot_id: bytes(BOT), message: "a".repeat(257) })),
    error("SendChat.message", Problem::ChatMessage(ChatMessageError::TooLong { units: 257 }))
)]
#[case::send_chat_not_exact(
    server(wire::server_message::Msg::SendChat(wire::SendChat { request_id: bytes(REQUEST), bot_id: bytes(BOT), message: "\thi".to_owned() })),
    error("SendChat.message", Problem::NotExact)
)]
#[case::grant_0(
    server(wire::server_message::Msg::SessionGrant(wire::SessionGrant { id: 0, ..grant(wire::session_grant::Session::Online(online_session())) })),
    error("SessionGrant.id", Problem::Missing)
)]
#[case::grant_bot_not_v7(
    server(wire::server_message::Msg::SessionGrant(wire::SessionGrant { bot_id: bytes(V4), ..grant(wire::session_grant::Session::Online(online_session())) })),
    error("SessionGrant.bot_id", Problem::NotV7)
)]
#[case::grant_without_a_session(
    server(wire::server_message::Msg::SessionGrant(wire::SessionGrant { id: 1, bot_id: bytes(BOT), session: None })),
    error("SessionGrant.session", Problem::Missing)
)]
#[case::grant_bad_username(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.username = "a b".to_owned()))))),
    error("OnlineSession.username", Problem::McUsername(McUsernameError::InvalidChar { index: 1 }))
)]
#[case::grant_nil_profile(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.uuid = vec![0; 16]))))),
    error("OnlineSession.uuid", Problem::Nil)
)]
#[case::grant_profile_too_short(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.uuid = vec![9; 15]))))),
    error("OnlineSession.uuid", Problem::WrongLength { len: 15 })
)]
#[case::grant_empty_token(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.access_token = String::new()))))),
    error("OnlineSession.access_token", Problem::InvalidToken)
)]
#[case::grant_token_over_16_kib(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.access_token = "t".repeat(16 * 1024 + 1)))))),
    error("OnlineSession.access_token", Problem::InvalidToken)
)]
#[case::grant_token_with_a_space(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.access_token = "a b".to_owned()))))),
    error("OnlineSession.access_token", Problem::InvalidToken)
)]
#[case::grant_token_not_ascii(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.access_token = "tökén".to_owned()))))),
    error("OnlineSession.access_token", Problem::InvalidToken)
)]
#[case::grant_without_an_expiry(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Online(online_with(|s: &mut wire::OnlineSession| s.expires_at_unix_ms = None))))),
    error("OnlineSession.expires_at_unix_ms", Problem::Missing)
)]
#[case::grant_bad_offline_name(
    server(wire::server_message::Msg::SessionGrant(grant(wire::session_grant::Session::Offline(wire::OfflineSession { username: "AfkBot1!".to_owned() })))),
    error("OfflineSession.username", Problem::McUsername(McUsernameError::InvalidChar { index: 7 }))
)]
#[case::denied_0(
    server(wire::server_message::Msg::SessionDenied(wire::SessionDenied { id: 0, bot_id: bytes(BOT), retryable: true })),
    error("SessionDenied.id", Problem::Missing)
)]
#[case::denied_bot_too_short(
    server(wire::server_message::Msg::SessionDenied(wire::SessionDenied { id: 4, bot_id: Vec::new(), retryable: false })),
    error("SessionDenied.bot_id", Problem::WrongLength { len: 0 })
)]
#[case::reject_unspecified(
    server(wire::server_message::Msg::Reject(wire::Reject { reason: 0 })),
    error("Reject.reason", Problem::Missing)
)]
#[case::reject_unknown(
    server(wire::server_message::Msg::Reject(wire::Reject { reason: 8 })),
    error("Reject.reason", Problem::Unknown)
)]
#[case::lifecycle_bot_not_v7(
    server(wire::server_message::Msg::Lifecycle(wire::LifecycleCommand { bot_id: bytes(V4), command: wire::Lifecycle::Resume.into() })),
    error("LifecycleCommand.bot_id", Problem::NotV7)
)]
#[case::lifecycle_bot_too_short(
    server(wire::server_message::Msg::Lifecycle(wire::LifecycleCommand { bot_id: vec![7; 15], command: wire::Lifecycle::Restart.into() })),
    error("LifecycleCommand.bot_id", Problem::WrongLength { len: 15 })
)]
#[case::lifecycle_unspecified(
    server(wire::server_message::Msg::Lifecycle(wire::LifecycleCommand { bot_id: bytes(BOT), command: 0 })),
    error("LifecycleCommand.command", Problem::Missing)
)]
#[case::lifecycle_unknown(
    server(wire::server_message::Msg::Lifecycle(wire::LifecycleCommand { bot_id: bytes(BOT), command: 4 })),
    error("LifecycleCommand.command", Problem::Unknown)
)]
fn a_server_message_is_refused(#[case] wire: wire::ServerMessage, #[case] expected: ConvertError) {
    assert_eq!(ServerMessage::try_from(wire).err(), Some(expected));
}

#[test]
fn an_event_too_late_for_the_wire_isnt_sent() {
    let event = AgentMessage::Event(BotEvent {
        bot_id: BOT.parse().unwrap(),
        at: DateTime::from_timestamp_millis(253_402_300_800_000).unwrap(),
        kind: BotEventKind::Died,
    });
    assert_eq!(
        wire::AgentMessage::try_from(event),
        Err(error("BotEvent.at_unix_ms", Problem::OutOfRange))
    );
}

#[test]
fn a_snapshot_too_early_for_the_wire_isnt_sent() {
    let snapshot = BotSnapshot {
        since: DateTime::from_timestamp_millis(-62_167_219_200_001).unwrap(),
        ..snapshot(BOT)
    };
    assert_eq!(
        wire::AgentMessage::try_from(AgentMessage::Status(snapshot)),
        Err(error("BotSnapshot.since_unix_ms", Problem::OutOfRange))
    );
}

#[rstest]
#[case::zero(Duration::ZERO)]
#[case::just_under_a_second(Duration::from_millis(999))]
#[case::just_over_five_minutes(Duration::from_millis(300_001))]
fn a_heartbeat_interval_out_of_range_isnt_sent(#[case] heartbeat_interval: Duration) {
    let welcome = ServerMessage::Welcome(Welcome {
        heartbeat_interval,
        agent_id: AGENT.parse().unwrap(),
    });
    assert_eq!(
        wire::ServerMessage::try_from(welcome),
        Err(error("Welcome.heartbeat_interval_ms", Problem::OutOfRange))
    );
}

#[test]
fn a_heartbeat_interval_finer_than_a_millisecond_is_cut_to_it() {
    let welcome = ServerMessage::Welcome(Welcome {
        heartbeat_interval: Duration::from_micros(10_000_500),
        agent_id: AGENT.parse().unwrap(),
    });
    assert_eq!(
        wire::ServerMessage::try_from(welcome),
        Ok(server(wire::server_message::Msg::Welcome(wire::Welcome {
            heartbeat_interval_ms: 10_000,
            agent_id: bytes(AGENT),
        })))
    );
}

#[test]
fn a_grant_expiring_too_late_for_the_wire_isnt_sent() {
    let grant = ServerMessage::SessionGrant(SessionGrant {
        id: SessionRequestId::new(core::num::NonZeroU64::MIN),
        bot_id: BOT.parse().unwrap(),
        credentials: SessionCredentials::Online {
            username: "AfkBot1".parse().unwrap(),
            uuid: PROFILE.parse().unwrap(),
            access_token: SecretString::from("eyJ.token-value"),
            expires_at: DateTime::from_timestamp_millis(253_402_300_800_000).unwrap(),
        },
    });
    assert_eq!(
        wire::ServerMessage::try_from(grant).err(),
        Some(error(
            "OnlineSession.expires_at_unix_ms",
            Problem::OutOfRange
        ))
    );
}

#[test]
fn every_error_names_its_field_and_position_and_never_a_value() {
    assert_eq!(
        error("Hello.running", Problem::Duplicate).at(2).to_string(),
        "Hello.running [2]: a bot that appears twice"
    );
    assert_eq!(
        error("BotSpec.id", Problem::WrongLength { len: 15 }).to_string(),
        "BotSpec.id: 15 bytes instead of 16"
    );
    assert_eq!(
        error("BotSpec.desired", Problem::Missing).at(1).at(4),
        error("BotSpec.desired", Problem::Missing).at(1),
    );
}
