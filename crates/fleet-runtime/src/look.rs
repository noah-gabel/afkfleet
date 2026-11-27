//! [`Look`]: what a bot sees, on demand (Plan.md P16.20; ADR-0036, M14).
//!
//! [`Fleet::look`](crate::Fleet::look) hands the bot's actor a look at its
//! session's snapshot and returns at once; the look answers later, within
//! [`LOOK_TIMEOUT`]. The actor runs one look at a time a bot, in a task of
//! its own `JoinSet`, so neither the fleet nor the actor ever waits on a
//! session for it.

use core::time::Duration;

use fleet_core::mc::MenuView;
use tokio::sync::oneshot;

/// The longest a look waits for the bot's session: a snapshot takes a
/// game tick's job on the host thread, far less than this.
pub const LOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a look at a bot's screen didn't see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum LookError {
    /// The bot isn't in a world: it's stopped, connecting, between worlds,
    /// or its session is ending.
    #[error("the bot isn't in a world")]
    NotOnline,
    /// A look at this bot runs already.
    #[error("the bot's screen is being looked at already")]
    Busy,
    /// The bot's session didn't answer within [`LOOK_TIMEOUT`].
    #[error("the bot's session didn't answer in time")]
    TimedOut,
}

/// A look at a bot's screen, answered later.
#[derive(Debug)]
pub struct Look(oneshot::Receiver<Result<MenuView, LookError>>);

impl Look {
    /// A look the actor answers on `answer`.
    pub(crate) const fn new(answer: oneshot::Receiver<Result<MenuView, LookError>>) -> Self {
        Self(answer)
    }

    /// A look answered already, with `answer`.
    pub(crate) fn answered(answer: Result<MenuView, LookError>) -> Self {
        let (reply, look) = oneshot::channel();
        // The receiver is right here, so the send can't fail.
        let _ = reply.send(answer);
        Self(look)
    }

    /// What the bot sees, or why the look didn't see it. A look whose actor
    /// ended before it answered saw nothing: the bot isn't in a world any
    /// more.
    ///
    /// # Errors
    /// [`LookError`].
    pub async fn seen(self) -> Result<MenuView, LookError> {
        // Bounded by the actor's own LOOK_TIMEOUT: the sender is answered or
        // dropped by then.
        self.0.await.unwrap_or(Err(LookError::NotOnline))
    }
}
