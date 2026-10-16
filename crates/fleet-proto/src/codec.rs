//! The codec the generated clients and servers use (ADR-0019, S25): prost's,
//! with one check in front of decoding.
//!
//! tonic checks a message's declared length against the decoding limit, then
//! prost builds it whole. A hello's `running` entries are the one place that
//! is cheap on the wire and costly in memory: an empty entry takes 2 bytes
//! and becomes a `BotSnapshot` of about 112, so a hello within the 8 MiB
//! limit could build millions of them before the conversion refuses more
//! than [`MAX_BOTS`]. [`BoundedCodec`] counts them first, through a view that
//! decodes each entry into a zero-sized [`wire::Unit`], and refuses more
//! than [`MAX_BOTS`] before prost builds anything. Every other message is
//! decoded as prost does.
//!
//! A message that can't be decoded is `INVALID_ARGUMENT` [`UNREADABLE`],
//! never prost's `INTERNAL`: a receiver can tell a sender's malformed
//! message from a transport that failed, which tonic reports with other
//! codes.
//!
//! The build script names this codec with `codec_path`; the generated code
//! is the only user.

use core::fmt;
use core::marker::PhantomData;

use prost::Message;
use prost::bytes::Buf as _;
use tonic::Status;
use tonic::codec::{BufferSettings, Codec, DecodeBuf, Decoder};
use tonic_prost::{ProstCodec, ProstEncoder};

use crate::control::MAX_BOTS;
use crate::generated as wire;

/// The refusal of a hello that lists more than [`MAX_BOTS`] bots.
pub const TOO_MANY_BOTS: &str = "A hello lists at most 1000 bots.";

/// The refusal of a message that can't be decoded: `INVALID_ARGUMENT`.
pub const UNREADABLE: &str = "The message couldn't be decoded.";

/// A message's check before prost decodes it; see the [module docs](self).
pub trait Bounded {
    /// Checks `message`, the whole encoded message.
    ///
    /// # Errors
    /// A [`Status`] for a message that must not be decoded.
    fn check(message: &[u8]) -> Result<(), Status> {
        let _ = message;
        Ok(())
    }
}

/// What [`Bounded`] reads of an [`wire::AgentMessage`]: its hello (field 1),
/// and of that only the running bots (field 5), each decoded into nothing.
/// Every occurrence of a field adds up, as prost's own decoding merges them,
/// so the count is never lower than what prost would build.
#[derive(Clone, PartialEq, Message)]
struct AgentMessageView {
    #[prost(message, optional, tag = "1")]
    hello: Option<HelloView>,
}

/// The running bots of [`AgentMessageView`]'s hello.
#[derive(Clone, PartialEq, Message)]
struct HelloView {
    #[prost(message, repeated, tag = "5")]
    running: Vec<wire::Unit>,
}

impl Bounded for wire::AgentMessage {
    /// Refuses a hello that lists more than [`MAX_BOTS`] bots. A message the
    /// view can't read isn't a message prost could read either, so it's
    /// refused here too.
    fn check(message: &[u8]) -> Result<(), Status> {
        let view = AgentMessageView::decode(message).map_err(|_| unreadable())?;
        let count = view.hello.map_or(0, |hello| hello.running.len());
        if count > MAX_BOTS {
            Err(Status::out_of_range(TOO_MANY_BOTS))
        } else {
            Ok(())
        }
    }
}

impl Bounded for wire::ServerMessage {}
impl Bounded for wire::EnrollRequest {}
impl Bounded for wire::EnrollResponse {}

/// prost's codec with [`Bounded`]'s check before decoding.
pub struct BoundedCodec<T, U> {
    types: PhantomData<(T, U)>,
}

impl<T, U> Default for BoundedCodec<T, U> {
    fn default() -> Self {
        Self { types: PhantomData }
    }
}

impl<T, U> fmt::Debug for BoundedCodec<T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedCodec").finish()
    }
}

impl<T, U> Codec for BoundedCodec<T, U>
where
    T: Message + Send + 'static,
    U: Message + Default + Send + Bounded + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = ProstEncoder<T>;
    type Decoder = BoundedDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        ProstCodec::<T, U>::default().encoder()
    }

    fn decoder(&mut self) -> Self::Decoder {
        BoundedDecoder { types: PhantomData }
    }
}

/// prost's decoding with [`Bounded`]'s check first.
pub struct BoundedDecoder<U> {
    types: PhantomData<fn() -> U>,
}

impl<U> fmt::Debug for BoundedDecoder<U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedDecoder").finish()
    }
}

impl<U: Message + Default + Bounded> Decoder for BoundedDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<U>, Status> {
        // tonic hands over exactly one whole message in one buffer; a
        // shorter chunk would let the check see less than prost reads.
        let whole = src.remaining();
        let read = if src.chunk().len() == whole {
            read(src.chunk())
        } else {
            Err(unreadable())
        };
        // The message is used up either way, as prost's decoding would.
        src.advance(whole);
        read.map(Some)
    }

    fn buffer_settings(&self) -> BufferSettings {
        BufferSettings::default()
    }
}

/// Checks `message` with [`Bounded`], then decodes it.
///
/// # Errors
/// [`Bounded::check`]'s refusal, or [`unreadable`] when prost can't
/// decode it.
fn read<U: Message + Default + Bounded>(message: &[u8]) -> Result<U, Status> {
    U::check(message)?;
    U::decode(message).map_err(|_| unreadable())
}

/// `INVALID_ARGUMENT` [`UNREADABLE`].
fn unreadable() -> Status {
    Status::invalid_argument(UNREADABLE)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use tonic::Code;

    use super::*;

    /// A hello whose `running` holds `count` empty entries.
    fn hello(count: usize) -> wire::AgentMessage {
        wire::AgentMessage {
            msg: Some(wire::agent_message::Msg::Hello(wire::Hello {
                running: vec![wire::BotSnapshot::default(); count],
                ..wire::Hello::default()
            })),
        }
    }

    fn checked(bytes: &[u8]) -> Result<(), Code> {
        <wire::AgentMessage as Bounded>::check(bytes).map_err(|status| status.code())
    }

    #[test]
    fn a_hello_with_a_thousand_and_one_bots_is_refused_before_it_is_decoded() {
        let bytes = hello(MAX_BOTS + 1).encode_to_vec();

        let refusal = <wire::AgentMessage as Bounded>::check(&bytes).unwrap_err();

        assert_eq!(refusal.code(), Code::OutOfRange);
        assert_eq!(refusal.message(), TOO_MANY_BOTS);
    }

    #[test]
    fn a_hello_with_exactly_max_bots_passes() {
        assert_eq!(checked(&hello(MAX_BOTS).encode_to_vec()), Ok(()));
    }

    #[test]
    fn a_hello_split_over_two_fields_is_counted_whole() {
        let mut bytes = hello(600).encode_to_vec();
        bytes.extend(hello(401).encode_to_vec());

        let merged = wire::AgentMessage::decode(bytes.as_slice()).unwrap();
        let Some(wire::agent_message::Msg::Hello(hello)) = merged.msg else {
            panic!("a hello");
        };
        assert_eq!(hello.running.len(), MAX_BOTS + 1);
        assert_eq!(checked(&bytes), Err(Code::OutOfRange));
    }

    #[test]
    fn a_readable_message_is_decoded() {
        let heartbeat = wire::AgentMessage {
            msg: Some(wire::agent_message::Msg::Heartbeat(wire::Heartbeat {})),
        };

        let read = read::<wire::AgentMessage>(&heartbeat.encode_to_vec()).unwrap();

        assert_eq!(read, heartbeat);
    }

    #[rstest::rstest]
    #[case::the_views_field_cut_short(&[0x0A])]
    #[case::a_heartbeat_with_the_wrong_wire_type(&[0x10, 0x01])]
    fn a_message_that_cant_be_decoded_is_invalid_argument(#[case] bytes: &[u8]) {
        let refusal = read::<wire::AgentMessage>(bytes).unwrap_err();

        assert_eq!(refusal.code(), Code::InvalidArgument);
        assert_eq!(refusal.message(), UNREADABLE);
    }

    #[test]
    fn every_other_message_passes_unchecked() {
        let heartbeat = wire::AgentMessage {
            msg: Some(wire::agent_message::Msg::Heartbeat(wire::Heartbeat {})),
        };

        assert_eq!(checked(&heartbeat.encode_to_vec()), Ok(()));
        assert!(<wire::ServerMessage as Bounded>::check(&[0xFF; 3]).is_ok());
        assert!(<wire::EnrollRequest as Bounded>::check(&[0xFF; 3]).is_ok());
        assert!(<wire::EnrollResponse as Bounded>::check(&[0xFF; 3]).is_ok());
    }

    proptest! {
        #[test]
        fn the_view_never_counts_fewer_bots_than_prost_builds(
            first in prop_oneof![0..=20_usize, 990..=1010_usize],
            second in prop_oneof![Just(0_usize), 0..=20_usize, 990..=1010_usize],
        ) {
            let mut bytes = hello(first).encode_to_vec();
            bytes.extend(hello(second).encode_to_vec());
            let built = match wire::AgentMessage::decode(bytes.as_slice()).unwrap().msg {
                Some(wire::agent_message::Msg::Hello(hello)) => hello.running.len(),
                _ => 0,
            };

            prop_assert_eq!(checked(&bytes).is_ok(), built <= MAX_BOTS);
        }
    }
}
