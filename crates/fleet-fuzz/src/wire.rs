//! The targets over the control plane's messages: what the server reads
//! from an agent, and what an agent reads from the server.

use fleet_proto::codec::Bounded;
use fleet_proto::control::{AgentMessage, MAX_BOTS, RenewResponse, ServerMessage, read_hello};
use fleet_proto::enrollment::{EnrollRequest, EnrollResponse};
use fleet_proto::generated as wire;
use prost::Message as _;

/// How many bots the hello in `message` lists, if it is one.
fn bots_in_hello(message: &wire::AgentMessage) -> Option<usize> {
    match &message.msg {
        Some(wire::agent_message::Msg::Hello(hello)) => Some(hello.running.len()),
        _ => None,
    }
}

/// What must hold of a decoded agent message, given whether the codec's
/// check let its bytes through: a hello it lets through lists at most
/// [`MAX_BOTS`] bots (ADR-0019, S25).
fn the_bound_holds(let_through: bool, message: &wire::AgentMessage) {
    let over = bots_in_hello(message).is_some_and(|bots| bots > MAX_BOTS);
    assert!(
        !(let_through && over),
        "the codec let a hello with too many bots through"
    );
}

/// What the server reads from an agent, over any bytes: the codec's check
/// of a hello's bots, the message's decoding, [`read_hello`] and the
/// conversion to the core's types, and an enrollment request's. No panic;
/// the bound holds; and a message the core accepts reads back as itself
/// from what the core would send.
///
/// # Panics
/// When a property breaks.
pub fn agent_message(data: &[u8]) {
    let let_through = <wire::AgentMessage as Bounded>::check(data).is_ok();
    if let Ok(message) = wire::AgentMessage::decode(data) {
        the_bound_holds(let_through, &message);
        let _ = read_hello(message.clone());
        if let Ok(read) = AgentMessage::try_from(message)
            && let Ok(sent) = wire::AgentMessage::try_from(read.clone())
        {
            assert_eq!(
                AgentMessage::try_from(sent).ok().as_ref(),
                Some(&read),
                "an agent's message doesn't read back from what the core sends"
            );
        }
    }
    if let Ok(request) = wire::EnrollRequest::decode(data) {
        let _ = EnrollRequest::try_from(request);
    }
}

/// What an agent reads from the server, over any bytes: a server message's
/// decoding and conversion, an enrollment's answer and a renewal's. No
/// panic.
///
/// # Panics
/// Never, unless a conversion does.
pub fn server_message(data: &[u8]) {
    if let Ok(message) = wire::ServerMessage::decode(data) {
        let _ = ServerMessage::try_from(message);
    }
    if let Ok(response) = wire::EnrollResponse::decode(data) {
        let _ = EnrollResponse::try_from(response);
    }
    if let Ok(response) = wire::RenewResponse::decode(data) {
        let _ = RenewResponse::try_from(response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hello that lists `bots` bots, each an empty snapshot.
    fn hello_with(bots: usize) -> wire::AgentMessage {
        wire::AgentMessage {
            msg: Some(wire::agent_message::Msg::Hello(wire::Hello {
                running: vec![wire::BotSnapshot::default(); bots],
                ..wire::Hello::default()
            })),
        }
    }

    #[test]
    fn a_hello_at_the_bound_may_be_let_through() {
        the_bound_holds(true, &hello_with(MAX_BOTS));
    }

    #[test]
    fn a_refused_hello_over_the_bound_is_fine() {
        the_bound_holds(false, &hello_with(MAX_BOTS + 1));
    }

    #[test]
    #[should_panic(expected = "the codec let a hello with too many bots through")]
    fn a_hello_over_the_bound_that_was_let_through_fails() {
        the_bound_holds(true, &hello_with(MAX_BOTS + 1));
    }

    #[test]
    fn the_codec_refuses_the_hello_over_the_bound_and_takes_the_one_at_it() {
        let over = hello_with(MAX_BOTS + 1).encode_to_vec();
        let at = hello_with(MAX_BOTS).encode_to_vec();

        assert!(<wire::AgentMessage as Bounded>::check(&over).is_err());
        assert!(<wire::AgentMessage as Bounded>::check(&at).is_ok());
    }
}
