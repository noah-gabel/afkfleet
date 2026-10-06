//! [`SessionHandle`]: how the runtime acts in a running session.

use core::future::Future;

use super::Liveness;
use crate::chat::ChatMessage;
use crate::mode::GameAction;

/// Why a [`SessionHandle`] call didn't run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// The session has ended, or is being torn down.
    #[error("the session has ended")]
    Closed,
    /// The session's job queue is full, so the call was dropped instead of
    /// waiting.
    #[error("the session's job queue is full")]
    QueueFull,
    /// The session didn't answer in time; it may hang. The watchdog decides
    /// whether it does.
    #[error("the session didn't answer in time")]
    TimedOut,
    /// The bot isn't in a world: it hasn't joined yet, or it has left.
    #[error("the bot isn't in a world")]
    NotInWorld,
}

/// A handle to one running Minecraft session (Plan.md P2.10; ADR-0008,
/// ADR-0010).
///
/// Clones control the same session. The calls queue work for the session's
/// host thread and wait for the answer, with a timeout; none of them blocks
/// the caller's runtime.
pub trait SessionHandle: Clone + Send + Sync + 'static {
    /// Performs a game action.
    ///
    /// The adapter skips an action with a non-finite angle and logs it, as a
    /// safety net behind mode validation (P3.6).
    ///
    /// # Errors
    /// [`SessionError::Closed`], [`SessionError::QueueFull`],
    /// [`SessionError::TimedOut`] or [`SessionError::NotInWorld`].
    fn perform(&self, action: GameAction) -> impl Future<Output = Result<(), SessionError>> + Send;

    /// Sends a chat message or a `/command`.
    ///
    /// # Errors
    /// As for [`perform`](Self::perform).
    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send;

    /// Respawns the bot after it died.
    ///
    /// # Errors
    /// As for [`perform`](Self::perform).
    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send;

    /// Tears the session down completely (ADR-0008 §10): exit the client,
    /// wait for its runner, drop every handle and close the host thread.
    ///
    /// It always finishes: a host thread that hangs is abandoned. Calling it
    /// again, from any clone, or after the session ended on its own is
    /// harmless.
    fn disconnect(&self) -> impl Future<Output = ()> + Send;

    /// Reads when the session last ticked and last heard from the server.
    fn liveness(&self) -> Liveness;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::closed(SessionError::Closed, "the session has ended")]
    #[case::queue_full(SessionError::QueueFull, "the session's job queue is full")]
    #[case::timed_out(SessionError::TimedOut, "the session didn't answer in time")]
    #[case::not_in_world(SessionError::NotInWorld, "the bot isn't in a world")]
    fn errors_have_fixed_messages(#[case] error: SessionError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }
}
