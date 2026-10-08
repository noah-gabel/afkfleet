//! [`Command`]: one `Fleet` call on its way to the supervisor.

use core::time::Duration;

use fleet_core::bot::{BotSnapshot, BotSpec, StickyState};
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use tokio::sync::oneshot;

use super::{FleetError, SendChatError, ShutdownReport};
use crate::chat::{ChatError, ChatTicket};

/// Where the supervisor answers a call.
pub(super) type Reply<T> = oneshot::Sender<Result<T, FleetError>>;

/// Where the bot's actor answers a chat message.
pub(super) type ChatAnswer = oneshot::Receiver<Result<ChatTicket, ChatError>>;

/// One `Fleet` call, with where to answer it. Its `Debug` output can hold
/// chat text, so it's never logged.
#[derive(Debug)]
pub(super) enum Command {
    Apply {
        spec: Box<BotSpec>,
        restore: Option<StickyState>,
        reply: Reply<()>,
    },
    Remove {
        id: BotId,
        reply: Reply<()>,
    },
    Lifecycle {
        id: BotId,
        action: Lifecycle,
        reply: Reply<()>,
    },
    /// The supervisor answers with the receiver the actor answers on, so it
    /// never waits for the actor itself.
    SendChat {
        id: BotId,
        message: ChatMessage,
        reply: oneshot::Sender<Result<ChatAnswer, SendChatError>>,
    },
    Snapshot {
        id: BotId,
        reply: Reply<BotSnapshot>,
    },
    SnapshotAll {
        reply: Reply<Vec<BotSnapshot>>,
    },
    Shutdown {
        timeout: Duration,
        reply: Reply<ShutdownReport>,
    },
}

/// A lifecycle call: one `BotCommand` each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lifecycle {
    Reset,
    Resume,
    Restart,
}

impl Command {
    /// Answers the call with `error` without doing it.
    pub(super) fn refuse(self, error: FleetError) {
        // An error only means the caller stopped waiting.
        match self {
            Self::Apply { reply, .. }
            | Self::Remove { reply, .. }
            | Self::Lifecycle { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::SendChat { reply, .. } => {
                let _ = reply.send(Err(error.into()));
            }
            Self::Snapshot { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::SnapshotAll { reply } => {
                let _ = reply.send(Err(error));
            }
            Self::Shutdown { reply, .. } => {
                let _ = reply.send(Err(error));
            }
        }
    }
}
