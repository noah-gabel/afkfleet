//! [`BotCommand`]: what a bot's actor is asked to do, and [`BotInbox`], where
//! it's asked.

use fleet_core::bot::BotSpec;
use fleet_core::chat::ChatMessage;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use crate::chat::{ChatError, ChatTicket};

/// A message to a bot's actor: one per `Fleet` call (ADR-0013).
///
/// There's no Start or Stop: the actor starts and stops the bot only as the
/// spec's desired state says, so the two can never disagree. Its `Debug`
/// output can hold chat text, so it's logged only at `debug`.
#[derive(Debug)]
pub enum BotCommand {
    /// Take on a new spec. A changed server reconnects, a changed mode only
    /// restarts the mode, and the desired state starts or stops the bot. A
    /// spec for another bot or account is ignored.
    UpdateSpec(Box<BotSpec>),
    /// Stop the bot, then start it again: a new run that reconnects. Nothing
    /// happens while the desired state is Stopped.
    Restart,
    /// Try again after `Failed`.
    Reset,
    /// Connect again after `Paused`.
    Resume,
    /// Queue a user's chat message. The reply is the message's ticket, or
    /// why it wasn't queued.
    SendChat {
        /// The message.
        message: ChatMessage,
        /// Where the actor answers.
        reply: oneshot::Sender<Result<ChatTicket, ChatError>>,
    },
}

/// Why a [`BotCommand`] didn't reach the actor. Nothing waits: a full inbox
/// is an error, not a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InboxError {
    /// The actor's inbox is full.
    #[error("the bot's inbox is full")]
    Full,
    /// The actor has ended.
    #[error("the bot's actor has ended")]
    Closed,
}

/// The sending side of a bot actor's bounded inbox. Clones send to the same
/// actor; the actor ends once every clone is dropped.
#[derive(Debug, Clone)]
pub struct BotInbox {
    sender: mpsc::Sender<BotCommand>,
}

impl BotInbox {
    pub(crate) const fn new(sender: mpsc::Sender<BotCommand>) -> Self {
        Self { sender }
    }

    /// Hands `command` to the actor without waiting.
    ///
    /// # Errors
    /// [`InboxError::Full`] if the inbox is full, and [`InboxError::Closed`]
    /// once the actor has ended.
    pub fn try_send(&self, command: BotCommand) -> Result<(), InboxError> {
        self.sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => InboxError::Full,
            TrySendError::Closed(_) => InboxError::Closed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::full(InboxError::Full, "the bot's inbox is full")]
    #[case::closed(InboxError::Closed, "the bot's actor has ended")]
    fn inbox_errors_have_fixed_messages(#[case] error: InboxError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }
}
