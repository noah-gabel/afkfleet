//! The hello's own reading (Plan.md P10.4; ADR-0019, S24): a stream's first
//! message must be a hello, of this build's protocol version, that converts.
//! The version is read before anything else in it, so an agent of another
//! version learns that and not some field's problem.

use fleet_proto::control::{HelloRefusal, RejectReason, read_hello};
use fleet_proto::generated as wire;
use fleet_proto::{PROTOCOL_VERSION, Problem};
use proptest::prelude::*;
use rstest::rstest;

const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";
const REQUEST: &str = "018bcfe5-6800-7aaa-9aaa-aaaaaaaaaaaa";

fn bytes(text: &str) -> Vec<u8> {
    text.parse::<uuid::Uuid>().unwrap().as_bytes().to_vec()
}

fn hello() -> wire::Hello {
    wire::Hello {
        name: "agent-1".to_owned(),
        agent_version: "0.1.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        capacity: 50,
        running: Vec::new(),
    }
}

fn first(msg: Option<wire::agent_message::Msg>) -> wire::AgentMessage {
    wire::AgentMessage { msg }
}

fn with_hello(change: impl FnOnce(&mut wire::Hello)) -> wire::AgentMessage {
    let mut hello = hello();
    change(&mut hello);
    first(Some(wire::agent_message::Msg::Hello(hello)))
}

#[test]
fn a_hello_of_this_version_is_read() {
    let read = read_hello(with_hello(|_| {})).unwrap();

    assert_eq!(read.name.as_str(), "agent-1");
    assert_eq!(read.protocol_version, PROTOCOL_VERSION);
    assert_eq!(u32::from(read.capacity), 50);
}

#[test]
fn the_protocol_version_is_read_before_the_rest_of_the_hello() {
    let refusal = read_hello(with_hello(|hello| {
        hello.protocol_version = PROTOCOL_VERSION + 1;
        hello.capacity = 0;
    }))
    .unwrap_err();

    assert!(
        matches!(refusal, HelloRefusal::Incompatible { version } if version == PROTOCOL_VERSION + 1),
        "{refusal:?}"
    );
}

#[test]
fn an_unset_protocol_version_is_incompatible() {
    let refusal = read_hello(with_hello(|hello| hello.protocol_version = 0)).unwrap_err();

    assert!(
        matches!(refusal, HelloRefusal::Incompatible { version: 0 }),
        "{refusal:?}"
    );
}

#[rstest]
#[case::nothing(None)]
#[case::a_heartbeat(Some(wire::agent_message::Msg::Heartbeat(wire::Heartbeat {})))]
#[case::a_status(Some(wire::agent_message::Msg::Status(wire::BotSnapshot::default())))]
#[case::an_event(Some(wire::agent_message::Msg::Event(wire::BotEvent {
    bot_id: bytes(BOT),
    at_unix_ms: Some(1_790_000_000_000),
    kind: Some(wire::bot_event::Kind::Died(wire::Unit {})),
})))]
#[case::a_session_request(Some(wire::agent_message::Msg::SessionRequest(wire::SessionRequest {
    id: 1,
    bot_id: bytes(REQUEST),
    fresh: false,
})))]
fn a_first_message_of_any_other_kind_is_not_a_hello(#[case] msg: Option<wire::agent_message::Msg>) {
    let refusal = read_hello(first(msg)).unwrap_err();

    assert!(matches!(refusal, HelloRefusal::NotHello), "{refusal:?}");
}

#[rstest]
#[case::no_capacity(0, "Hello.capacity")]
#[case::too_much_capacity(1001, "Hello.capacity")]
fn a_hello_that_doesnt_convert_is_invalid_with_its_field(
    #[case] capacity: u32,
    #[case] field: &str,
) {
    let refusal = read_hello(with_hello(|hello| hello.capacity = capacity)).unwrap_err();

    let HelloRefusal::Invalid(error) = refusal else {
        panic!("{refusal:?}");
    };
    assert_eq!(error.field, field);
}

#[test]
fn a_hello_with_another_name_shape_is_invalid_with_its_field() {
    let refusal = read_hello(with_hello(|hello| hello.name = String::new())).unwrap_err();

    let HelloRefusal::Invalid(error) = refusal else {
        panic!("{refusal:?}");
    };
    assert_eq!(error.field, "Hello.name");
}

#[test]
fn each_refusal_has_its_reject_reason() {
    let invalid = fleet_proto::ConvertError::new("Hello.capacity", Problem::Missing);

    assert_eq!(HelloRefusal::NotHello.reason(), RejectReason::InvalidHello);
    assert_eq!(
        HelloRefusal::Incompatible { version: 2 }.reason(),
        RejectReason::IncompatibleProtocol
    );
    assert_eq!(
        HelloRefusal::Invalid(invalid).reason(),
        RejectReason::InvalidHello
    );
}

proptest! {
    #[test]
    fn every_version_but_this_builds_is_incompatible(
        version in prop_oneof![
            Just(0_u32),
            Just(PROTOCOL_VERSION + 1),
            Just(u32::MAX),
            any::<u32>(),
        ].prop_filter("another version", |version| *version != PROTOCOL_VERSION)
    ) {
        let mut hello = hello();
        hello.protocol_version = version;

        let refusal = read_hello(first(Some(wire::agent_message::Msg::Hello(hello)))).unwrap_err();

        prop_assert_eq!(refusal, HelloRefusal::Incompatible { version });
    }
}
