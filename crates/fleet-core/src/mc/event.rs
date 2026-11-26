//! [`SessionEvent`]: what a session reports, and [`SessionEvents`], the stream
//! it comes from.

use core::future::Future;

use super::LetGo;
use crate::chat::IncomingChat;
use crate::disconnect::{ConnectFailure, DisconnectReason};
use crate::script::menu::{ClickResult, ScreenId, SignEditor};

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
    /// The server put the bot into another world on the same connection,
    /// and the bot has loaded there: a second login, as a proxy sends when
    /// it moves a player to another server, or a respawn packet without a
    /// death, as a portal causes (ADR-0036, D6). Whatever the bot held is
    /// let go.
    WorldChanged,
    /// The session let go of inputs by itself, below whoever pressed them
    /// (ADR-0036, D10, D11).
    LetGo(LetGo),
    /// A screen opened: one the server opened, or the bot's own inventory
    /// (ADR-0036, M2). A new screen is a new screen, whatever its title.
    /// Every key and mouse button is let go, and none goes down while it's
    /// open (M8).
    MenuOpened(ScreenId),
    /// The open screen's content changed: its slots, its cursor or a
    /// merchant's trades.
    MenuUpdated(ScreenId),
    /// The screen closed: the server or the bot closed it, a sign editor
    /// took its place, or the bot died or changed worlds.
    MenuClosed(ScreenId),
    /// What became of an action sent onto a screen, decided at its
    /// barrier's reply or after its wait (ADR-0036, R1).
    Clicked(ClickResult),
    /// The server opened a sign editor (ADR-0036, M16). It's answered only
    /// through [`SessionHandle::menu`](super::SessionHandle::menu), and
    /// never by the session itself.
    SignOpened(SignEditor),
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
/// - `WorldChanged` and each reason of `LetGo` are pending at most once:
///   a second one before the first was read adds nothing, so a server
///   that sends a bot back and forth can't fill the queue.
/// - The menu events are bounded the same way, whatever a server sends
///   (ADR-0051): `MenuUpdated` is pending at most once a screen; a screen
///   that opens drops what's unread of the screens before it, all but the
///   close of one whose opening was read; a screen that closes before its
///   `MenuOpened` was read takes that event with it; and a `SignOpened`
///   replaces one unread. `Clicked` is always delivered, and pending at
///   most once: no action goes out while a fate is unread.
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
