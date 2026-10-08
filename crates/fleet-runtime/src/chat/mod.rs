//! A bot's outbound chat queue (Plan.md P4.5; ADR-0013).
//!
//! - A [`ChatQueue`] belongs to one bot. It's open while the bot is Online:
//!   [`ChatQueue::open`] attaches a session and returns the session's
//!   [`ChatDelivery`] task and a [`ModeChat`] handle for the mode runner.
//! - User chat and mode chat share the queue and the bot's [`ChatBucket`].
//!   A message is admitted or refused at once, never waiting: a full queue
//!   gives [`ChatError::QueueFull`], an empty bucket
//!   [`ChatError::RateLimited`], and a closed queue
//!   [`ChatError::NotOnline`]. The queue slot is reserved before the token
//!   is taken, so a refused message uses up neither.
//! - User chat gets a [`ChatTicket`] when it's queued. Its outcome comes later
//!   as a `ChatSent` or `ChatFailed` [`FleetEvent`](crate::FleetEvent); mode
//!   chat that's sent comes as `ModeChatSent`.
//! - A failed send isn't retried, and its token isn't refunded. Closing the
//!   queue fails what's still queued with [`ChatFailure::Disconnected`].

mod bucket;
mod queue;
mod ticket;

use fleet_core::mc::SessionError;

pub use bucket::{ChatBucket, ChatBucketError, ChatQuota};
pub use queue::{ChatDelivery, ChatQueue, ModeChat};
pub use ticket::{ChatTicket, ChatTickets};

/// Why a chat message wasn't queued. The caller gets it at once; nothing
/// waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ChatError {
    /// The bot isn't Online, so it has no session to send with. Nothing is
    /// kept for a later session.
    #[error("the bot isn't online")]
    NotOnline,
    /// The bot's chat queue is full.
    #[error("the bot's chat queue is full")]
    QueueFull,
    /// The bot's chat bucket is empty: it has sent as much as its rate limit
    /// allows for now.
    #[error("the bot's chat rate limit is reached")]
    RateLimited,
}

/// Why a queued chat message wasn't sent. It isn't retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ChatFailure {
    /// The session ended before the message was sent.
    #[error("the session ended before the message was sent")]
    Disconnected,
    /// The server requires signed chat, and the session can't sign it
    /// (ADR-0011).
    #[error("the server requires signed chat, and the session can't sign it")]
    ChatUnavailable,
    /// The session didn't answer in time.
    #[error("the session didn't answer in time")]
    TimedOut,
    /// The session's job queue was full.
    #[error("the session was too busy")]
    SessionBusy,
    /// The bot wasn't in a world.
    #[error("the bot wasn't in a world")]
    NotInWorld,
}

impl From<SessionError> for ChatFailure {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Closed => Self::Disconnected,
            SessionError::ChatUnavailable => Self::ChatUnavailable,
            SessionError::TimedOut => Self::TimedOut,
            SessionError::QueueFull => Self::SessionBusy,
            SessionError::NotInWorld => Self::NotInWorld,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::closed(SessionError::Closed, ChatFailure::Disconnected)]
    #[case::chat_unavailable(SessionError::ChatUnavailable, ChatFailure::ChatUnavailable)]
    #[case::timed_out(SessionError::TimedOut, ChatFailure::TimedOut)]
    #[case::queue_full(SessionError::QueueFull, ChatFailure::SessionBusy)]
    #[case::not_in_world(SessionError::NotInWorld, ChatFailure::NotInWorld)]
    fn each_session_error_maps_to_its_failure(
        #[case] error: SessionError,
        #[case] failure: ChatFailure,
    ) {
        assert_eq!(ChatFailure::from(error), failure);
    }

    #[rstest]
    #[case::not_online(ChatError::NotOnline, "the bot isn't online")]
    #[case::queue_full(ChatError::QueueFull, "the bot's chat queue is full")]
    #[case::rate_limited(ChatError::RateLimited, "the bot's chat rate limit is reached")]
    fn chat_errors_have_fixed_messages(#[case] error: ChatError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }

    #[rstest]
    #[case::disconnected(
        ChatFailure::Disconnected,
        "the session ended before the message was sent"
    )]
    #[case::chat_unavailable(
        ChatFailure::ChatUnavailable,
        "the server requires signed chat, and the session can't sign it"
    )]
    #[case::timed_out(ChatFailure::TimedOut, "the session didn't answer in time")]
    #[case::session_busy(ChatFailure::SessionBusy, "the session was too busy")]
    #[case::not_in_world(ChatFailure::NotInWorld, "the bot wasn't in a world")]
    fn chat_failures_have_fixed_messages(#[case] failure: ChatFailure, #[case] message: &str) {
        assert_eq!(failure.to_string(), message);
    }
}
