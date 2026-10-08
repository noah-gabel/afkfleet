//! The bot's current session, and what its events do.

use fleet_core::bot::BotEvent;
use fleet_core::disconnect::DisconnectReason;
use fleet_core::mc::{MinecraftConnector, SessionCredentialProvider, SessionEvent, SessionEvents};
use tokio_util::sync::CancellationToken;

use super::BotActor;
use crate::event::FleetEventKind;

/// The session the bot is in, from a successful connect until the actor
/// executes `Disconnect` for it.
pub(super) struct Session<S> {
    /// The handle the actor, the mode runner and the chat delivery share.
    pub(super) handle: S,
    /// Which connect started it, counted from 1. A teardown reports
    /// `SessionClosed` only while no newer session has connected
    /// (ADR-0010).
    pub(super) generation: u64,
    /// The parent of the mode runner's and the chat delivery's tokens.
    pub(super) token: CancellationToken,
    /// The mode runner's token, while the mode runs.
    pub(super) runner: Option<CancellationToken>,
}

/// Waits for the next event of `events`, or forever without a session.
pub(super) async fn next_event<E: SessionEvents>(events: &mut Option<E>) -> Option<SessionEvent> {
    match events {
        Some(events) => events.next().await,
        None => core::future::pending().await,
    }
}

impl<C: MinecraftConnector, P: SessionCredentialProvider> BotActor<C, P> {
    /// Applies one event of the current session. `None` means its events
    /// ended without a terminal event, as when its host thread is gone:
    /// that's a crash.
    pub(super) async fn session_event(&mut self, event: Option<SessionEvent>) {
        match event {
            None => {
                self.session_events = None;
                self.ended(DisconnectReason::SessionCrashed, BotEvent::SessionClosed)
                    .await;
            }
            Some(SessionEvent::Joined) => self.feed(BotEvent::Joined).await,
            Some(SessionEvent::Chat(chat)) => self.publish(FleetEventKind::ChatReceived(chat)),
            Some(SessionEvent::Died) => {
                self.publish(FleetEventKind::Died);
                self.feed(BotEvent::Died).await;
            }
            Some(SessionEvent::Disconnected(reason)) => {
                self.ended(reason.clone(), BotEvent::Disconnected(reason))
                    .await;
            }
            Some(SessionEvent::ConnectionFailed(failure)) => {
                self.ended(
                    DisconnectReason::ConnectFailed { failure },
                    BotEvent::ConnectFailed(failure),
                )
                .await;
            }
        }
    }

    /// The session ended on its own for `reason`: records it in the
    /// snapshot, then applies `event`.
    pub(super) async fn ended(&mut self, reason: DisconnectReason, event: BotEvent) {
        self.last_disconnect = Some(reason);
        self.feed(event).await;
    }

    /// Applies the events of the current session that are ready now,
    /// without waiting for more, and at most `session_drain` of them. The
    /// actor does this before every input that ends the session, so a
    /// duplicate-login kick that's already queued always comes first and
    /// pauses the bot (ADR-0013).
    pub(super) async fn drain(&mut self) {
        for _ in 0..self.config.session_drain.get() {
            let Some(events) = self.session_events.as_mut() else {
                return;
            };
            let ready = tokio::select! {
                biased;
                // Cancel-safe: `next` is (port contract), so an event that
                // isn't ready yet stays queued.
                event = events.next() => Some(event),
                () = core::future::ready(()) => None,
            };
            let Some(event) = ready else {
                return;
            };
            self.session_event(event).await;
        }
    }
}
