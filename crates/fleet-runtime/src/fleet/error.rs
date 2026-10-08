//! Why a [`Fleet`](super::Fleet) call failed, why a fleet couldn't be built,
//! and what a shutdown reports.

use core::fmt;

use crate::chat::{ChatBucketError, ChatError};

/// Why a [`Fleet`](super::Fleet) call failed (ADR-0013). Nothing waits for
/// room: overload is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum FleetError {
    /// The fleet runs no bot with this ID, or the bot is being removed.
    #[error("the fleet has no such bot")]
    UnknownBot,
    /// A queue on the way is full, the bot's actor has just ended, or the bot
    /// (or the only one holding its account) is being removed. Trying again
    /// later can work.
    #[error("the fleet is busy; try again")]
    Busy,
    /// The fleet already holds `max_bots` bots.
    #[error("the fleet holds as many bots as it may")]
    AtCapacity,
    /// The spec names a different account for a bot the fleet runs. A bot's
    /// account never changes, not even in case: remove it and add a new one.
    #[error("a bot's account can't change")]
    AccountChanged,
    /// Another bot in the fleet already plays as this account.
    #[error("another bot already uses this account")]
    AccountInUse,
    /// The fleet is shutting down, or has shut down.
    #[error("the fleet is shutting down")]
    ShuttingDown,
    /// The supervisor didn't answer within the reply timeout.
    #[error("the fleet didn't answer in time")]
    TimedOut,
}

/// Why [`Fleet::send_chat`](super::Fleet::send_chat) didn't queue a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum SendChatError {
    /// The fleet refused the call.
    #[error(transparent)]
    Fleet(#[from] FleetError),
    /// The bot's chat queue refused the message.
    #[error(transparent)]
    Chat(#[from] ChatError),
}

/// A channel capacity in the [`RuntimeConfig`](crate::RuntimeConfig) that
/// tokio would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapacitySetting {
    /// `event_buffer`: the fleet's event `broadcast`.
    EventBuffer,
    /// `supervisor_queue`: the supervisor's command queue.
    SupervisorQueue,
    /// `actor_inbox`: each bot actor's inbox.
    ActorInbox,
    /// `chat_queue`: each bot's chat queue.
    ChatQueue,
}

impl fmt::Display for CapacitySetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EventBuffer => "event_buffer",
            Self::SupervisorQueue => "supervisor_queue",
            Self::ActorInbox => "actor_inbox",
            Self::ChatQueue => "chat_queue",
        })
    }
}

/// Why [`Fleet::new`](super::Fleet::new) couldn't build a fleet from its
/// settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FleetSetupError {
    /// The chat rate limit is invalid.
    #[error("the chat rate limit is invalid: {0}")]
    ChatQuota(#[from] ChatBucketError),
    /// A channel capacity is larger than tokio allows, so building the
    /// channel would panic.
    #[error("`{setting}` is larger than tokio allows")]
    CapacityTooLarge {
        /// The setting.
        setting: CapacitySetting,
    },
}

/// How a shutdown went: how each actor that was running ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShutdownReport {
    /// Actors that stopped their bot and finished every teardown in time.
    pub stopped: usize,
    /// Actors still running at the timeout, which were aborted. Aborting
    /// drops their sessions, so fleet-mc ends the host threads.
    pub aborted: usize,
    /// Actors that panicked, or one of whose tasks did, while they stopped.
    pub crashed: usize,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::unknown_bot(FleetError::UnknownBot, "the fleet has no such bot")]
    #[case::busy(FleetError::Busy, "the fleet is busy; try again")]
    #[case::at_capacity(FleetError::AtCapacity, "the fleet holds as many bots as it may")]
    #[case::account_changed(FleetError::AccountChanged, "a bot's account can't change")]
    #[case::account_in_use(FleetError::AccountInUse, "another bot already uses this account")]
    #[case::shutting_down(FleetError::ShuttingDown, "the fleet is shutting down")]
    #[case::timed_out(FleetError::TimedOut, "the fleet didn't answer in time")]
    fn fleet_errors_have_fixed_messages(#[case] error: FleetError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }

    #[test]
    fn send_chat_errors_show_their_cause() {
        assert_eq!(
            SendChatError::from(FleetError::Busy).to_string(),
            "the fleet is busy; try again"
        );
        assert_eq!(
            SendChatError::from(ChatError::RateLimited).to_string(),
            "the bot's chat rate limit is reached"
        );
    }

    #[rstest]
    #[case::event_buffer(
        CapacitySetting::EventBuffer,
        "`event_buffer` is larger than tokio allows"
    )]
    #[case::supervisor_queue(
        CapacitySetting::SupervisorQueue,
        "`supervisor_queue` is larger than tokio allows"
    )]
    #[case::actor_inbox(
        CapacitySetting::ActorInbox,
        "`actor_inbox` is larger than tokio allows"
    )]
    #[case::chat_queue(CapacitySetting::ChatQueue, "`chat_queue` is larger than tokio allows")]
    fn a_capacity_error_names_the_setting(#[case] setting: CapacitySetting, #[case] message: &str) {
        assert_eq!(
            FleetSetupError::CapacityTooLarge { setting }.to_string(),
            message
        );
    }

    #[test]
    fn a_quota_error_says_why() {
        assert_eq!(
            FleetSetupError::from(ChatBucketError::ZeroInterval).to_string(),
            "the chat rate limit is invalid: the chat interval is zero"
        );
    }
}
