//! The control protocol's messages in validated form: what agent and server
//! code handle (Plan.md P10.1; ADR-0019).
//!
//! Every message from the other side is read through `TryFrom` from its
//! generated envelope ([`crate::generated::AgentMessage`] or
//! [`crate::generated::ServerMessage`]). That runs each value through
//! fleet-core's constructors, refuses a missing or unknown field, and cleans
//! again the text that came from a Minecraft server. Every message to the other
//! side is written back through `TryFrom` into its generated envelope, which
//! only fails for a time or an interval outside what the wire accepts. Nobody
//! matches on the generated oneofs.
//!
//! What exists only on the wire stays here: the hello, the heartbeat interval,
//! the reasons for a refusal, and the numbers that pair a session request with
//! its answer. What is stored, shown or used by a policy is fleet-core's.

mod agent;
mod hello;
mod server;

use core::num::NonZeroU64;
use core::time::Duration;
use std::collections::BTreeSet;

use fleet_core::id::BotId;

use crate::{ConvertError, Problem};

pub use agent::{
    AgentMessage, BotEvent, BotEventKind, ChatFailedReason, Hello, SessionRequest, SessionRequestId,
};
pub use hello::{HelloRefusal, read_hello};
pub use server::{
    Assignment, LifecycleCommand, RejectReason, SendChat, ServerMessage, SessionDenied,
    SessionGrant, Welcome,
};

/// The most bots one message may carry: a hello's running bots, or a full
/// reconcile's assignments. It's the largest
/// [`AgentCapacity`](fleet_core::value::AgentCapacity).
pub const MAX_BOTS: usize = 1000;

/// The largest message an agent sends and the server reads, in bytes: a
/// power of two above the largest legitimate hello, 1000 bots with kicks at
/// their caps (ADR-0019, S25). Both ends set it; tonic's default of 4 MiB
/// would refuse that hello.
pub const MAX_AGENT_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// The largest message the server sends and an agent reads, in bytes: a
/// power of two above the largest legitimate full reconcile, 1000 bots
/// whose specs are at their caps (ADR-0019, S25).
pub const MAX_SERVER_MESSAGE_BYTES: usize = 128 * 1024 * 1024;

/// The length of `message` on the wire, which a sender compares with
/// [`MAX_SERVER_MESSAGE_BYTES`] before it queues it (ADR-0019, S25).
#[must_use]
pub fn server_message_len(message: &crate::generated::ServerMessage) -> usize {
    prost::Message::encoded_len(message)
}

/// How long the server waits for a stream's first message, from the moment
/// it opens the stream (ADR-0019, S24).
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an agent's attempt may take until `Welcome` or `Reject`, dialing
/// and the TLS handshake included: longer than [`HELLO_TIMEOUT`] and the
/// hello's write behind it. Bounding the whole attempt keeps S45's grace
/// exact (ADR-0019, S67).
pub const WELCOME_TIMEOUT: Duration = Duration::from_secs(30);

/// How often an agent's HTTP/2 keepalive pings its connection to the
/// server, while idle too (ADR-0019, S67): a dead server is noticed within
/// [`KEEPALIVE_INTERVAL`] plus [`KEEPALIVE_TIMEOUT`].
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// How long an agent's keepalive waits for a ping's answer before it closes
/// the connection (ADR-0019, S67).
pub const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// The longest an agent waits between two attempts to connect, whatever its
/// backoff (P10.7): with its keepalive and [`WELCOME_TIMEOUT`], it bounds how
/// soon after the server starts every running agent has said hello again,
/// which the server's placement waits for (ADR-0019, S45).
pub const RECONNECT_DELAY_MAX: Duration = Duration::from_secs(30);

/// Reads a list of bots: at most [`MAX_BOTS`], each bot once. An error in an
/// entry, or a bot that appears again, carries the entry's position.
fn bots_from_wire<W, T>(
    field: &'static str,
    items: Vec<W>,
    bot_id: impl Fn(&T) -> BotId,
) -> Result<Vec<T>, ConvertError>
where
    T: TryFrom<W, Error = ConvertError>,
{
    if items.len() > MAX_BOTS {
        return Err(ConvertError::new(
            field,
            Problem::TooMany {
                count: items.len(),
                max: MAX_BOTS,
            },
        ));
    }
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let value = T::try_from(item).map_err(|e| e.at(index))?;
            if seen.insert(bot_id(&value)) {
                Ok(value)
            } else {
                Err(ConvertError::new(field, Problem::Duplicate).at(index))
            }
        })
        .collect()
}

/// Writes a list of bots; an error carries the entry's position.
fn bots_to_wire<T, W>(items: Vec<T>) -> Result<Vec<W>, ConvertError>
where
    W: TryFrom<T, Error = ConvertError>,
{
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| W::try_from(item).map_err(|e| e.at(index)))
        .collect()
}

/// Reads a session request's number, which counts from 1.
fn request_number(field: &'static str, number: u64) -> Result<SessionRequestId, ConvertError> {
    NonZeroU64::new(number)
        .map(SessionRequestId::new)
        .ok_or(ConvertError::new(field, Problem::Missing))
}

#[cfg(test)]
mod tests {
    use fleet_core::value::AgentCapacity;

    use super::MAX_BOTS;

    #[test]
    fn a_message_carries_at_most_the_largest_capacity() {
        assert_eq!(MAX_BOTS, usize::from(AgentCapacity::MAX));
    }
}
