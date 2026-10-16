//! fleet-proto's tests (Plan.md P10.1; ADR-0019):
//!
//! - `round_trip`: every valid core value survives the wire unchanged, and
//!   every value of each enum the wire maps.
//! - `decode`: random and mutated bytes, received chat and modes in any form
//!   are refused or read as a valid value that reads back the same.
//! - `rejects`: one case per rule that refuses a message, with its error.
//! - `enrollment`: the enrollment's messages: the wire, their refusals, the
//!   token's redaction and the shared limits.
//! - `redaction`: a secret never prints, and every field named like a secret
//!   belongs to a redacted message.
//! - `schema`: the pinned wire schema.
//! - `strategies`: the proptest strategies the files share.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

mod decode;
mod enrollment;
mod redaction;
mod rejects;
mod round_trip;
mod schema;
mod strategies;

use fleet_core::mc::SessionCredentials;
use fleet_proto::control::{ServerMessage, SessionGrant};
use prost::Message;
use secrecy::ExposeSecret;

/// The wire schema the build script rendered from the .proto files.
const WIRE_SCHEMA: &str = include_str!(concat!(env!("OUT_DIR"), "/wire-schema.txt"));

/// Sends a validated message over the wire and reads it back: into its
/// generated envelope, into protobuf bytes, and back.
fn over_the_wire<T, W>(value: T) -> Result<T, fleet_proto::ConvertError>
where
    W: Message + Default + TryFrom<T, Error = fleet_proto::ConvertError>,
    T: TryFrom<W, Error = fleet_proto::ConvertError>,
{
    let bytes = W::try_from(value)?.encode_to_vec();
    T::try_from(W::decode(bytes.as_slice()).unwrap())
}

/// Whether two sets of credentials are the same, token included. Only tests
/// compare tokens this way; the code never compares a secret outside constant
/// time.
fn same_credentials(a: &SessionCredentials, b: &SessionCredentials) -> bool {
    match (a, b) {
        (
            SessionCredentials::Offline { username: a },
            SessionCredentials::Offline { username: b },
        ) => a == b,
        (
            SessionCredentials::Online {
                username,
                uuid,
                access_token,
                expires_at,
            },
            SessionCredentials::Online {
                username: other_username,
                uuid: other_uuid,
                access_token: other_token,
                expires_at: other_expiry,
            },
        ) => {
            username == other_username
                && uuid == other_uuid
                && access_token.expose_secret() == other_token.expose_secret()
                && expires_at == other_expiry
        }
        _ => false,
    }
}

fn same_grant(a: &SessionGrant, b: &SessionGrant) -> bool {
    a.id == b.id && a.bot_id == b.bot_id && same_credentials(&a.credentials, &b.credentials)
}

/// Whether two server messages are the same. `ServerMessage` has no
/// `PartialEq`, since a grant holds a secret.
fn same_server_message(a: &ServerMessage, b: &ServerMessage) -> bool {
    match (a, b) {
        (ServerMessage::Welcome(a), ServerMessage::Welcome(b)) => a == b,
        (ServerMessage::Reconcile(a), ServerMessage::Reconcile(b)) => a == b,
        (ServerMessage::Assign(a), ServerMessage::Assign(b)) => a == b,
        (ServerMessage::Unassign(a), ServerMessage::Unassign(b)) => a == b,
        (ServerMessage::Update(a), ServerMessage::Update(b)) => a == b,
        (ServerMessage::SendChat(a), ServerMessage::SendChat(b)) => a == b,
        (ServerMessage::SessionGrant(a), ServerMessage::SessionGrant(b)) => same_grant(a, b),
        (ServerMessage::SessionDenied(a), ServerMessage::SessionDenied(b)) => a == b,
        (ServerMessage::Reject(a), ServerMessage::Reject(b)) => a == b,
        _ => false,
    }
}
