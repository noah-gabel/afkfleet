//! [`MinecraftConnector`]: how the runtime starts a session.

use core::future::Future;
use core::time::Duration;

use super::{SessionCredentials, SessionEvents, SessionHandle};
use crate::id::BotId;
use crate::value::ServerAddress;

/// What a session needs to connect.
#[derive(Debug, Clone)]
pub struct ConnectParams {
    /// The bot the session belongs to, so the adapter can name its host
    /// thread and tag its logs.
    pub bot_id: BotId,
    /// The server to join.
    pub server: ServerAddress,
    /// The credentials to log in with.
    pub credentials: SessionCredentials,
    /// How long connecting may take before it fails with
    /// [`ConnectFailure::TimedOut`](crate::disconnect::ConnectFailure::TimedOut).
    /// azalea has no connect timeout of its own (ADR-0008 §5).
    pub connect_timeout: Duration,
}

/// Why no session could be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    /// No host thread is available, e.g. because too many hung threads were
    /// abandoned (ADR-0008 §5). The actor reports it as
    /// [`ConnectFailure::HostUnavailable`](crate::disconnect::ConnectFailure::HostUnavailable).
    #[error("no host thread is available for a new session")]
    HostUnavailable,
}

/// Starts Minecraft sessions (Plan.md P2.10; ADR-0008, ADR-0010).
///
/// The runtime is generic over this trait, so it never sees azalea: fleet-mc
/// implements it with azalea, and fleet-testkit with a scriptable fake.
pub trait MinecraftConnector: Send + Sync + 'static {
    /// The handle to a running session.
    type Session: SessionHandle;
    /// The events of a running session.
    type Events: SessionEvents;

    /// Starts a session with `params`.
    ///
    /// The future resolves once the session has started, before the bot
    /// reaches the server. How connecting goes arrives as an event:
    /// [`Joined`](super::SessionEvent::Joined),
    /// [`ConnectionFailed`](super::SessionEvent::ConnectionFailed) (also when
    /// `params.connect_timeout` runs out) or
    /// [`Disconnected`](super::SessionEvent::Disconnected).
    ///
    /// # Errors
    /// [`ConnectError::HostUnavailable`] if no session can be started at all.
    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_has_a_fixed_message() {
        assert_eq!(
            ConnectError::HostUnavailable.to_string(),
            "no host thread is available for a new session"
        );
    }
}
