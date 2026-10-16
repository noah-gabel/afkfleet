//! A stream's first message (ADR-0019, S24): [`read_hello`] reads it as a
//! hello, checking first that it is one, then that it speaks this build's
//! protocol version, and only then converting the rest, so an agent of
//! another version learns that and not some field's problem.

use super::agent::{Hello, hello_from_wire};
use super::server::RejectReason;
use crate::generated as wire;
use crate::{ConvertError, PROTOCOL_VERSION};

/// Why a stream's first message isn't a hello the server accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloRefusal {
    /// The first message isn't a hello.
    NotHello,
    /// The hello speaks another protocol version than
    /// [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION); 0 is one too.
    Incompatible {
        /// The version the hello named.
        version: u32,
    },
    /// The hello doesn't convert.
    Invalid(ConvertError),
}

impl HelloRefusal {
    /// The reason the server's `Reject` gives for it.
    #[must_use]
    pub const fn reason(&self) -> RejectReason {
        match self {
            Self::Incompatible { .. } => RejectReason::IncompatibleProtocol,
            Self::NotHello | Self::Invalid(_) => RejectReason::InvalidHello,
        }
    }
}

/// Reads a stream's first message as a hello: that it is one, then its
/// protocol version, and only then the rest, so an agent of another version
/// learns that and not some field's problem.
///
/// # Errors
/// [`HelloRefusal::NotHello`] for any other message,
/// [`HelloRefusal::Incompatible`] for another protocol version, and
/// [`HelloRefusal::Invalid`] for a hello that doesn't convert.
pub fn read_hello(message: wire::AgentMessage) -> Result<Hello, HelloRefusal> {
    let Some(wire::agent_message::Msg::Hello(hello)) = message.msg else {
        return Err(HelloRefusal::NotHello);
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(HelloRefusal::Incompatible {
            version: hello.protocol_version,
        });
    }
    hello_from_wire(hello).map_err(HelloRefusal::Invalid)
}
