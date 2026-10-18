//! What the conductor makes of the fleet's answers (ADR-0019, S65), as
//! exhaustive tables: a chat message's failure and refusal as the wire's
//! [`ChatFailedReason`], and an apply's error as what happens to the spec.

use fleet_proto::control::ChatFailedReason;
use fleet_runtime::{ChatError, ChatFailure, FleetError, SendChatError};

/// A queued chat message's failure, one to one.
pub(crate) const fn failed(failure: ChatFailure) -> ChatFailedReason {
    match failure {
        ChatFailure::Disconnected => ChatFailedReason::Disconnected,
        ChatFailure::ChatUnavailable => ChatFailedReason::ChatUnavailable,
        ChatFailure::TimedOut => ChatFailedReason::TimedOut,
        ChatFailure::SessionBusy => ChatFailedReason::SessionBusy,
        ChatFailure::NotInWorld => ChatFailedReason::NotInWorld,
    }
}

/// A refusal to queue a chat message. The three apply errors `send_chat`
/// never returns become `AgentBusy`.
pub(crate) const fn refused(error: SendChatError) -> ChatFailedReason {
    match error {
        SendChatError::Chat(ChatError::NotOnline) => ChatFailedReason::NotOnline,
        SendChatError::Chat(ChatError::QueueFull) => ChatFailedReason::QueueFull,
        SendChatError::Chat(ChatError::RateLimited) => ChatFailedReason::RateLimited,
        SendChatError::Fleet(FleetError::UnknownBot) => ChatFailedReason::UnknownBot,
        SendChatError::Fleet(FleetError::ShuttingDown) => ChatFailedReason::ShuttingDown,
        SendChatError::Fleet(
            FleetError::Busy
            | FleetError::TimedOut
            | FleetError::AtCapacity
            | FleetError::AccountChanged
            | FleetError::AccountInUse,
        ) => ChatFailedReason::AgentBusy,
    }
}

/// What happens to a spec the fleet didn't apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unapplied {
    /// It stays pending and is tried again after any `Removed` and on the
    /// retry tick; `ends_batch` stops the instructions after it for now.
    Retry {
        /// The fleet didn't answer in time: give it room.
        ends_batch: bool,
    },
    /// It stays pending until a bot is removed, with one `warn`.
    AwaitRemoved,
    /// It's dropped with an `error` naming the bot.
    Rejected,
    /// It's dropped quietly: the fleet is going away.
    Dropped,
}

/// What `error` means for an apply.
pub(crate) const fn unapplied(error: FleetError) -> Unapplied {
    match error {
        FleetError::Busy => Unapplied::Retry { ends_batch: false },
        FleetError::TimedOut => Unapplied::Retry { ends_batch: true },
        FleetError::AtCapacity | FleetError::AccountInUse => Unapplied::AwaitRemoved,
        FleetError::AccountChanged => Unapplied::Rejected,
        // `apply` never answers `UnknownBot`.
        FleetError::ShuttingDown | FleetError::UnknownBot => Unapplied::Dropped,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(ChatFailure::Disconnected, ChatFailedReason::Disconnected)]
    #[case(ChatFailure::ChatUnavailable, ChatFailedReason::ChatUnavailable)]
    #[case(ChatFailure::TimedOut, ChatFailedReason::TimedOut)]
    #[case(ChatFailure::SessionBusy, ChatFailedReason::SessionBusy)]
    #[case(ChatFailure::NotInWorld, ChatFailedReason::NotInWorld)]
    fn every_chat_failure_has_its_reason(
        #[case] failure: ChatFailure,
        #[case] reason: ChatFailedReason,
    ) {
        assert_eq!(failed(failure), reason);
    }

    #[rstest]
    #[case(SendChatError::Chat(ChatError::NotOnline), ChatFailedReason::NotOnline)]
    #[case(SendChatError::Chat(ChatError::QueueFull), ChatFailedReason::QueueFull)]
    #[case(
        SendChatError::Chat(ChatError::RateLimited),
        ChatFailedReason::RateLimited
    )]
    #[case(
        SendChatError::Fleet(FleetError::UnknownBot),
        ChatFailedReason::UnknownBot
    )]
    #[case(SendChatError::Fleet(FleetError::Busy), ChatFailedReason::AgentBusy)]
    #[case(
        SendChatError::Fleet(FleetError::TimedOut),
        ChatFailedReason::AgentBusy
    )]
    #[case(
        SendChatError::Fleet(FleetError::ShuttingDown),
        ChatFailedReason::ShuttingDown
    )]
    #[case(
        SendChatError::Fleet(FleetError::AtCapacity),
        ChatFailedReason::AgentBusy
    )]
    #[case(
        SendChatError::Fleet(FleetError::AccountChanged),
        ChatFailedReason::AgentBusy
    )]
    #[case(
        SendChatError::Fleet(FleetError::AccountInUse),
        ChatFailedReason::AgentBusy
    )]
    fn every_chat_refusal_has_its_reason(
        #[case] error: SendChatError,
        #[case] reason: ChatFailedReason,
    ) {
        assert_eq!(refused(error), reason);
    }

    #[rstest]
    #[case(FleetError::Busy, Unapplied::Retry { ends_batch: false })]
    #[case(FleetError::TimedOut, Unapplied::Retry { ends_batch: true })]
    #[case(FleetError::AtCapacity, Unapplied::AwaitRemoved)]
    #[case(FleetError::AccountInUse, Unapplied::AwaitRemoved)]
    #[case(FleetError::AccountChanged, Unapplied::Rejected)]
    #[case(FleetError::ShuttingDown, Unapplied::Dropped)]
    #[case(FleetError::UnknownBot, Unapplied::Dropped)]
    fn every_apply_error_has_its_outcome(#[case] error: FleetError, #[case] outcome: Unapplied) {
        assert_eq!(unapplied(error), outcome);
    }
}
