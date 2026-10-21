//! [`SessionEvent`]: what a session reports, and [`SessionEvents`], the stream
//! it comes from.

use core::future::Future;

use crate::chat::IncomingChat;
use crate::disconnect::{ConnectFailure, DisconnectReason};

/// Something that happened in a Minecraft session.
///
/// Ticks and packets aren't events: they only update the session's
/// [`Liveness`](super::Liveness) stamps (ADR-0008 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The bot joined the server and is in the world.
    Joined,
    /// The bot received a chat message, already sanitized.
    Chat(IncomingChat),
    /// The bot died. It stays dead until the server confirms a respawn the
    /// session asked for, with [`SessionHandle::respawn`](super::SessionHandle::respawn).
    Died,
    /// The bot lives again: after a `Died` this stream delivered and a
    /// [`SessionHandle::respawn`](super::SessionHandle::respawn) since, the
    /// server respawned it and the client has loaded (ADR-0020, S20, S22).
    /// Never for a dimension change.
    Respawned,
    /// The session ended: a kick, a closed connection, a rejected session or
    /// a crash.
    Disconnected(DisconnectReason),
    /// Connecting failed before the bot reached the server.
    ConnectionFailed(ConnectFailure),
}

/// The events of one session, in order.
///
/// What an implementation must guarantee (ADR-0010; ADR-0020, S22):
/// - [`next`](Self::next) is cancel-safe: the actor calls it in
///   `tokio::select!`, and an event is never lost because another branch won.
/// - Only [`SessionEvent::Chat`] may be dropped, when the consumer lags
///   behind; the adapter counts what it drops. `Joined`, `Died`, `Respawned`,
///   `Disconnected` and `ConnectionFailed` are always delivered.
/// - `Died` comes once per death, though azalea can report it twice
///   (ADR-0008 §4), and the next `Died` only after `Respawned`.
/// - `Respawned` comes at most once per `Died`, and only after
///   [`next`](Self::next) has delivered that `Died` and
///   [`respawn`](super::SessionHandle::respawn) was called since. So at most
///   three lifecycle events are pending at once: `Joined` or `Respawned`,
///   one `Died`, one terminal event.
/// - A session ends with at most one terminal event, `Disconnected` or
///   `ConnectionFailed`. After it, or once the session is torn down,
///   [`next`](Self::next) returns `None`.
pub trait SessionEvents: Send + 'static {
    /// Waits for the next event. Returns `None` when the session has no more
    /// events.
    fn next(&mut self) -> impl Future<Output = Option<SessionEvent>> + Send;
}
