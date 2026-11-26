//! The bot's current session, and what its events do.

use core::sync::atomic::AtomicBool;
use fleet_core::bot::BotEvent;
use fleet_core::disconnect::DisconnectReason;
use std::sync::Arc;

use fleet_core::mc::{MinecraftConnector, SessionCredentialProvider, SessionEvent, SessionEvents};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::BotActor;
use crate::event::FleetEventKind;
use crate::script::{Life, ScriptFeed};

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
    /// The mode runner, while the mode runs.
    pub(super) runner: Option<Runner>,
    /// Whether the session may still hold what a script pressed: the
    /// runner sets it, and clears it once its own last `ReleaseAll` went
    /// through (Plan.md P15.17).
    pub(super) held: Arc<AtomicBool>,
}

/// The actor's hold on a session's mode runner.
pub(super) struct Runner {
    /// Ends the runner at once.
    pub(super) cancel: CancellationToken,
    /// Asks the runner to end its mode in an orderly way.
    pub(super) stop: CancellationToken,
    /// Cancelled when the runner's task has ended, however it ended.
    pub(super) done: CancellationToken,
    /// Where the actor puts what a script must hear of the session.
    pub(super) feed: ScriptFeed,
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
            Some(SessionEvent::Chat(chat)) => {
                // Kept only while a script that reads chat runs; a line
                // beyond its queue is dropped and counted there. Either
                // way the actor has nothing more to do with it.
                if let Some(feed) = self.script_feed() {
                    let _kept = feed.chat(&chat);
                }
                self.publish(FleetEventKind::ChatReceived(chat));
            }
            Some(SessionEvent::Died) => {
                self.publish(FleetEventKind::Died);
                self.tell_script(Life::Died);
                self.feed(BotEvent::Died).await;
            }
            Some(SessionEvent::Respawned) => self.respawned(),
            // A scripted mode's runner is what reads these two (Plan.md
            // P15.16). For the bot's lifecycle and a step mode nothing
            // changed: the bot is online as it was.
            Some(SessionEvent::WorldChanged) => {
                debug!("the server put the bot into another world");
                self.tell_script(Life::WorldChanged);
            }
            Some(SessionEvent::LetGo(reason)) => {
                debug!(?reason, "the session let go of inputs by itself");
                self.tell_script(Life::LetGo(reason));
            }
            // Read here, so a click's fate counts as read; a script hears of
            // screens, clicks and signs from Phase 16's group D on
            // (P16.13). Nothing of the server's text is in them.
            Some(
                event @ (SessionEvent::MenuOpened(_)
                | SessionEvent::MenuUpdated(_)
                | SessionEvent::MenuClosed(_)
                | SessionEvent::Clicked(_)
                | SessionEvent::SignOpened(_)),
            ) => debug!(?event, "a screen, a click or a sign editor"),
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

    /// The server confirmed a respawn: the retry, which never ends in
    /// success, is dropped, and the mode runner runs its on-respawn steps
    /// (ADR-0020, S23). One without an open death, which the actor stopped
    /// waiting for, changes nothing.
    fn respawned(&mut self) {
        if !self.death_open {
            debug!("a respawn the bot no longer waits for was confirmed");
            return;
        }
        self.death_open = false;
        self.respawn = None;
        debug!("the server confirmed the respawn");
        // With no runner the bump is dropped: the next one subscribes later.
        self.respawns
            .send_modify(|count| *count = count.wrapping_add(1));
        self.tell_script(Life::Respawned);
    }

    /// The current session's feed for its mode runner, while one runs.
    fn script_feed(&self) -> Option<&ScriptFeed> {
        self.session
            .as_ref()
            .and_then(|session| session.runner.as_ref())
            .map(|runner| &runner.feed)
    }

    /// Hands the session's mode runner something a script has a hook or a
    /// rule for. A step mode's runner never reads it, and each kind waits
    /// there at most once.
    fn tell_script(&self, life: Life) {
        if let Some(feed) = self.script_feed() {
            feed.life(life);
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
