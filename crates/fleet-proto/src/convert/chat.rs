//! Received chat, which came from a Minecraft server and is cleaned again when
//! it's read, and the validated chat a bot sends, which must arrive exactly as
//! its type keeps it (ADR-0019).

use fleet_core::chat::{ChatKind, ChatMessage, IncomingChat};

use super::{enum_from_wire, uuid_from_wire};
use crate::generated as wire;
use crate::{ConvertError, Problem};

impl From<IncomingChat> for wire::IncomingChat {
    fn from(chat: IncomingChat) -> Self {
        let kind = match chat.kind() {
            ChatKind::Chat => wire::ChatKind::Chat,
            ChatKind::Emote => wire::ChatKind::Emote,
            ChatKind::Whisper => wire::ChatKind::Whisper,
            ChatKind::Announcement => wire::ChatKind::Announcement,
            ChatKind::System => wire::ChatKind::System,
        };
        Self {
            kind: kind.into(),
            sender: chat.sender().map(|sender| wire::ChatSender {
                name: sender.name().to_owned(),
                uuid: sender.uuid().map(|uuid| uuid.as_bytes().to_vec()),
            }),
            text: chat.text().to_owned(),
            truncated: chat.is_truncated(),
        }
    }
}

impl TryFrom<wire::IncomingChat> for IncomingChat {
    type Error = ConvertError;

    fn try_from(chat: wire::IncomingChat) -> Result<Self, ConvertError> {
        const KIND: &str = "IncomingChat.kind";
        let kind = match enum_from_wire(KIND, chat.kind)? {
            wire::ChatKind::Chat => ChatKind::Chat,
            wire::ChatKind::Emote => ChatKind::Emote,
            wire::ChatKind::Whisper => ChatKind::Whisper,
            wire::ChatKind::Announcement => ChatKind::Announcement,
            wire::ChatKind::System => ChatKind::System,
            wire::ChatKind::Unspecified => return Err(ConvertError::new(KIND, Problem::Missing)),
        };
        let sender = chat
            .sender
            .as_ref()
            .map(|sender| {
                let uuid = sender
                    .uuid
                    .as_deref()
                    .map(|uuid| uuid_from_wire("ChatSender.uuid", uuid))
                    .transpose()?;
                Ok((sender.name.as_str(), uuid))
            })
            .transpose()?;
        // Text from a Minecraft server: `from_parts` cleans and caps the text and
        // the sender's name again, and keeps a cut the agent made.
        Self::from_parts(kind, sender, &chat.text, chat.truncated)
            .map_err(|e| ConvertError::new("IncomingChat.sender", e.into()))
    }
}

/// Reads a validated chat message. Nothing is repaired: text the constructor
/// refuses, or would change by trimming it, is refused.
pub(crate) fn chat_message_from_wire(
    field: &'static str,
    text: &str,
) -> Result<ChatMessage, ConvertError> {
    let message = ChatMessage::try_from(text).map_err(|e| ConvertError::new(field, e.into()))?;
    if message.as_str() == text {
        Ok(message)
    } else {
        Err(ConvertError::new(field, Problem::NotExact))
    }
}
