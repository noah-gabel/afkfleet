//! The bounded event bridge between a session's host thread and its actor:
//! [`EventSink`] on the host thread, [`McEvents`] in the actor.

use core::future::Future;
use core::num::NonZeroUsize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use azalea::Event;
use fleet_core::chat::{ChatSender, IncomingChat};
use fleet_core::disconnect::DisconnectReason;
use fleet_core::id::BotId;
use fleet_core::mc::{LetGo, SessionEvent, SessionEvents};
use fleet_core::script::menu::{ClickResult, ScreenId, SignEditor};
use tokio::sync::Notify;
use tracing::{debug, warn};

use super::liveness::LivenessStamps;
use super::map::{Mapped, Terminal, map_event};

/// Counts what the bridges of one connector set aside. Shared by every
/// session, so the counts survive the sessions (P3.4 exposes them, P4.9
/// exports them).
#[derive(Debug, Default)]
pub(crate) struct EventCounters {
    dropped_chat: AtomicU64,
    ignored_action_bar: AtomicU64,
}

impl EventCounters {
    /// Chat messages dropped because a bridge was full.
    pub(crate) fn dropped_chat(&self) -> u64 {
        self.dropped_chat.load(Ordering::Relaxed)
    }

    /// Action-bar messages, which are status displays and never delivered.
    pub(crate) fn ignored_action_bar(&self) -> u64 {
        self.ignored_action_bar.load(Ordering::Relaxed)
    }
}

/// What the bridge did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Queued for [`McEvents::next`]. A spawn may queue `Died` after `Joined`.
    Queued,
    /// Only stamped liveness: a tick or a packet.
    Stamped,
    /// The queue was full, so the chat message was dropped and counted. The
    /// first drop of a burst logs a warning.
    ChatDropped {
        /// Whether this drop started the burst.
        first_in_burst: bool,
    },
    /// An action-bar message: counted, never delivered.
    ActionBarIgnored,
    /// A death before `Joined`: held until the bot has joined.
    DeathHeld,
    /// A death already reported and not yet answered with a confirmed
    /// respawn.
    DuplicateDeath,
    /// Nothing the session reports, such as a second spawn.
    Ignored,
    /// A world change, or a let-go for this reason, is queued already and
    /// not yet read: one is enough.
    AlreadyPending,
    /// The session already ended or was closed, so nothing is delivered.
    SessionEnded,
}

/// Creates the bridge of `bot_id`'s session: its producer and its consumer.
///
/// The queue holds `capacity` events before chat is dropped; lifecycle
/// events are always delivered, and the contract bounds them.
pub(crate) fn bridge(
    bot_id: BotId,
    capacity: NonZeroUsize,
    counters: Arc<EventCounters>,
    stamps: Arc<LivenessStamps>,
) -> (EventSink, McEvents) {
    let shared = Arc::new(Shared {
        bot_id,
        capacity: capacity.get(),
        counters,
        stamps,
        state: Mutex::new(State::default()),
        ready: Notify::new(),
    });
    (
        EventSink {
            source: Arc::new(Source {
                shared: Arc::clone(&shared),
            }),
        },
        McEvents { shared },
    )
}

/// What a session's sink and events share.
#[derive(Debug)]
struct Shared {
    bot_id: BotId,
    capacity: usize,
    counters: Arc<EventCounters>,
    stamps: Arc<LivenessStamps>,
    state: Mutex<State>,
    /// Wakes the one [`McEvents`] when an event is queued or the bridge
    /// closes. `notify_one` keeps a permit when nobody waits yet, so no
    /// wake-up is lost between looking at the queue and waiting.
    ready: Notify,
}

#[derive(Debug, Default)]
struct State {
    queue: VecDeque<SessionEvent>,
    phase: Phase,
    death: Death,
    /// A Respawn packet removed the bot's `Dead` since the last confirmed
    /// respawn (ADR-0020, S27).
    revived: bool,
    /// Chat is being dropped: the next drop continues the burst.
    dropping_chat: bool,
    /// A packet put the bot into another world, and the bot isn't placed
    /// there yet (ADR-0036, D6).
    world_awaited: bool,
    /// The last screen whose `MenuOpened` the consumer read.
    opening_read: Option<ScreenId>,
    /// How many `Clicked` are queued and unread.
    fates_unread: usize,
}

/// A packet that puts the bot into a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorldPacket {
    /// A login: the join's own, or a proxy's move to another server.
    Login,
    /// A respawn: a death's, or a change of dimension.
    Respawn,
}

/// What the session's screens, clicks and sign editors report (ADR-0036,
/// M2, M16, R1; ADR-0051): each becomes its [`SessionEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuReport {
    /// A screen opened.
    Opened(ScreenId),
    /// The open screen's content changed.
    Updated(ScreenId),
    /// The screen closed.
    Closed(ScreenId),
    /// An action's fate.
    Clicked(ClickResult),
    /// The server opened a sign editor.
    SignOpened(SignEditor),
}

/// Where the session is, as far as its events go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Phase {
    /// Connecting: `Joined` hasn't been queued yet.
    #[default]
    Starting,
    /// `Joined` was queued, from the first spawn.
    Joined,
    /// A terminal event was queued; nothing follows it.
    Ended,
    /// The bridge was torn down.
    Closed,
}

/// The death the session has reported, if any (ADR-0020, S22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Death {
    /// None since the last confirmed respawn.
    #[default]
    None,
    /// The bot died before `Joined`; `Died` is queued right after it.
    Held,
    /// `Died` was queued and not yet read.
    Reported,
    /// The actor read `Died`; `requested` once the session asked for a
    /// respawn since.
    Taken {
        /// Whether a respawn was asked for since `Died` was read.
        requested: bool,
    },
}

impl State {
    /// Applies a mapped event that may be queued. Ticks, packets and ignored
    /// events never get here.
    fn apply(&mut self, mapped: Mapped, capacity: usize, counters: &EventCounters) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        match mapped {
            Mapped::Chat(_) if self.queue.len() >= capacity => {
                counters.dropped_chat.fetch_add(1, Ordering::Relaxed);
                let first_in_burst = !self.dropping_chat;
                self.dropping_chat = true;
                Delivery::ChatDropped { first_in_burst }
            }
            Mapped::Chat(chat) => {
                self.dropping_chat = false;
                self.queue.push_back(SessionEvent::Chat(chat));
                Delivery::Queued
            }
            Mapped::ActionBar => {
                counters.ignored_action_bar.fetch_add(1, Ordering::Relaxed);
                Delivery::ActionBarIgnored
            }
            Mapped::Spawn if self.phase == Phase::Joined => Delivery::Ignored,
            Mapped::Spawn => {
                self.phase = Phase::Joined;
                self.queue.push_back(SessionEvent::Joined);
                // The state machine ignores `Died` outside Online, so a death
                // before the join comes right after it (ADR-0011).
                if self.death == Death::Held {
                    self.death = Death::Reported;
                    self.queue.push_back(SessionEvent::Died);
                }
                Delivery::Queued
            }
            Mapped::Death if self.death != Death::None => Delivery::DuplicateDeath,
            Mapped::Death if self.phase == Phase::Joined => {
                self.death = Death::Reported;
                self.queue.push_back(SessionEvent::Died);
                Delivery::Queued
            }
            Mapped::Death => {
                self.death = Death::Held;
                Delivery::DeathHeld
            }
            Mapped::Terminal(terminal) => {
                self.end(terminal);
                Delivery::Queued
            }
            Mapped::Tick | Mapped::Packet | Mapped::Ignored => Delivery::Ignored,
        }
    }

    /// Queues `terminal` as the session's last event, unless it already
    /// ended. Returns whether it did. A held death is dropped: the bot never
    /// joined.
    fn end(&mut self, terminal: Terminal) -> bool {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return false;
        }
        self.phase = Phase::Ended;
        if self.death == Death::Held {
            self.death = Death::None;
        }
        self.queue.push_back(terminal.into());
        true
    }

    /// The session asked for a respawn; `loaded` says whether the bot's
    /// entity has `HasClientLoaded` now (ADR-0020, S22, S27).
    /// A request counts only once the actor read the `Died`; a stale call
    /// (before the read, or with no death) records nothing. A server that
    /// respawned the bot by itself left a removal and a load behind, which
    /// confirm it at once: the Respawn handler removes `HasClientLoaded`
    /// with `Dead`, so a load now came after the latest removal.
    fn respawn_asked(&mut self, loaded: bool) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        let Death::Taken { .. } = self.death else {
            return Delivery::Ignored;
        };
        self.death = Death::Taken { requested: true };
        if loaded && self.revived {
            self.confirm()
        } else {
            Delivery::Ignored
        }
    }

    /// A Respawn packet removed the bot's `Dead`, which only a Respawn does
    /// (ADR-0020, S27). Remembered until the next confirmation, so a
    /// Respawn that came in the death's own frame, before its `Died`
    /// reached the bridge, still counts for it.
    fn dead_removed(&mut self) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        self.revived = true;
        Delivery::Ignored
    }

    /// The bot's entity was loaded again (ADR-0020, S27): confirms a
    /// respawn asked for after a removal of `Dead`. With no death pending,
    /// the removal is forgotten: it was a `Dead` azalea never reported a
    /// death for, as at a dimension change of a bot whose `Dead` azalea put
    /// back, and must not confirm a later death before its own Respawn.
    /// Deaths are read inside the frame, so one pending is in the bridge by
    /// then.
    fn loaded(&mut self) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        match self.death {
            Death::Taken { requested: true } if self.revived => self.confirm(),
            Death::None => {
                self.revived = false;
                Delivery::Ignored
            }
            Death::Held | Death::Reported | Death::Taken { .. } => Delivery::Ignored,
        }
    }

    /// Queues `Respawned`, once per `Died`: the next death is reported
    /// again, and waits for a removal of its own.
    fn confirm(&mut self) -> Delivery {
        self.death = Death::None;
        self.revived = false;
        self.queue.push_back(SessionEvent::Respawned);
        Delivery::Queued
    }

    /// A packet that puts the bot into a world arrived (ADR-0036, D6). The
    /// join's own login and a death's respawn are no world change; any
    /// other is one, reported once the bot is placed there
    /// ([`placed`](Self::placed)).
    ///
    /// Deaths are read inside the frame, before this (`events::respawn`),
    /// so a death the Respawn answers is in the bridge by now.
    fn world_packet(&mut self, packet: WorldPacket) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        let changes = match packet {
            WorldPacket::Login => true,
            WorldPacket::Respawn => self.death == Death::None,
        };
        // Before `Joined` it's the join's own world.
        if changes && self.phase == Phase::Joined {
            self.world_awaited = true;
        }
        Delivery::Ignored
    }

    /// The server has placed the bot in its world and the client has
    /// loaded there: a world change that was waiting for it is queued.
    fn placed(&mut self) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        if !core::mem::take(&mut self.world_awaited) {
            return Delivery::Ignored;
        }
        self.queue_once(SessionEvent::WorldChanged)
    }

    /// The session let go of inputs by itself, for `reason`.
    fn let_go(&mut self, reason: LetGo) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        if self.phase != Phase::Joined {
            return Delivery::Ignored;
        }
        self.queue_once(SessionEvent::LetGo(reason))
    }

    /// A screen, a click or a sign editor reported `report` (ADR-0051).
    ///
    /// Whatever a server sends, at most a close, an opening and an update
    /// of the screens, a fate and a sign editor are unread at once:
    /// - a screen that opens drops what's unread of the screens before it,
    ///   all but the close of the one whose opening was read;
    /// - an update is pending once a screen;
    /// - a screen that closes before its opening was read takes the
    ///   opening and its update along, and reports nothing;
    /// - a fate is always queued, and the host thread sends no action
    ///   while one is unread ([`BridgeControl::fate_unread`]);
    /// - a sign editor replaces one unread.
    fn menu(&mut self, report: MenuReport) -> Delivery {
        if matches!(self.phase, Phase::Ended | Phase::Closed) {
            return Delivery::SessionEnded;
        }
        if self.phase != Phase::Joined {
            return Delivery::Ignored;
        }
        match report {
            MenuReport::Opened(screen) => {
                let read = self.opening_read;
                self.queue.retain(|event| match event {
                    SessionEvent::MenuOpened(_) | SessionEvent::MenuUpdated(_) => false,
                    SessionEvent::MenuClosed(closed) => read == Some(*closed),
                    _ => true,
                });
                self.queue.push_back(SessionEvent::MenuOpened(screen));
                Delivery::Queued
            }
            MenuReport::Updated(screen) => self.queue_once(SessionEvent::MenuUpdated(screen)),
            MenuReport::Closed(screen) => {
                let opening = SessionEvent::MenuOpened(screen);
                if self.queue.contains(&opening) {
                    let update = SessionEvent::MenuUpdated(screen);
                    self.queue
                        .retain(|event| *event != opening && *event != update);
                    return Delivery::Ignored;
                }
                self.queue_once(SessionEvent::MenuClosed(screen))
            }
            MenuReport::Clicked(result) => {
                self.fates_unread = self.fates_unread.saturating_add(1);
                self.queue.push_back(SessionEvent::Clicked(result));
                Delivery::Queued
            }
            MenuReport::SignOpened(editor) => {
                self.queue
                    .retain(|event| !matches!(event, SessionEvent::SignOpened(_)));
                self.queue.push_back(SessionEvent::SignOpened(editor));
                Delivery::Queued
            }
        }
    }

    /// Queues `event` unless one equal to it is queued and unread: a
    /// second world change, or a second let-go for one reason, tells its
    /// reader nothing the first doesn't, and a server that sends a bot
    /// back and forth can't fill the queue this way.
    fn queue_once(&mut self, event: SessionEvent) -> Delivery {
        if self.queue.contains(&event) {
            return Delivery::AlreadyPending;
        }
        self.queue.push_back(event);
        Delivery::Queued
    }

    /// Tears the bridge down and drops what's queued.
    fn close(&mut self) {
        self.phase = Phase::Closed;
        self.death = Death::None;
        self.revived = false;
        self.world_awaited = false;
        self.opening_read = None;
        self.fates_unread = 0;
        self.queue.clear();
    }

    fn next(&mut self) -> Next {
        if self.phase == Phase::Closed {
            return Next::End;
        }
        match self.queue.pop_front() {
            Some(SessionEvent::Died) => {
                // Read now: a respawn asked for from here on counts.
                if self.death == Death::Reported {
                    self.death = Death::Taken { requested: false };
                }
                Next::Event(SessionEvent::Died)
            }
            Some(SessionEvent::MenuOpened(screen)) => {
                self.opening_read = Some(screen);
                Next::Event(SessionEvent::MenuOpened(screen))
            }
            Some(SessionEvent::Clicked(result)) => {
                self.fates_unread = self.fates_unread.saturating_sub(1);
                Next::Event(SessionEvent::Clicked(result))
            }
            Some(event) => Next::Event(event),
            None if self.phase == Phase::Ended => Next::End,
            None => Next::Wait,
        }
    }
}

/// What [`McEvents::next`] finds.
enum Next {
    Event(SessionEvent),
    End,
    Wait,
}

impl Shared {
    /// Applies one mapped event that arrived at `now`.
    fn apply(&self, mapped: Mapped, now: Instant) -> Delivery {
        match mapped {
            Mapped::Tick => {
                self.stamps.stamp_tick(now);
                return Delivery::Stamped;
            }
            Mapped::Packet => {
                self.stamps.stamp_packet(now);
                return Delivery::Stamped;
            }
            Mapped::Ignored => return Delivery::Ignored,
            Mapped::Chat(ref chat) => self.log_chat(chat),
            _ => {}
        }
        let delivery = self.lock().apply(mapped, self.capacity, &self.counters);
        match delivery {
            Delivery::Queued => self.ready.notify_one(),
            Delivery::ChatDropped {
                first_in_burst: true,
            } => warn!(
                bot_id = %self.bot_id,
                dropped_chat = self.counters.dropped_chat(),
                "the session's event queue is full, so chat is dropped until it drains"
            ),
            _ => {}
        }
        delivery
    }

    /// Chat is untrusted and may hold line breaks, so it's logged only at
    /// `debug`, and only as `?` fields, which a server can't use to forge log
    /// lines.
    fn log_chat(&self, chat: &IncomingChat) {
        debug!(
            bot_id = %self.bot_id,
            kind = %chat.kind(),
            sender = ?chat.sender().map(ChatSender::name),
            text = ?chat.text(),
            "chat received"
        );
    }

    /// Ends the session with `terminal`, unless it already ended. Returns
    /// whether it did.
    fn terminate(&self, terminal: Terminal) -> bool {
        let ended = self.lock().end(terminal);
        if ended {
            self.ready.notify_one();
        }
        ended
    }

    /// Applies a signal from inside the frame with `apply`, waking the
    /// consumer when it queued an event.
    fn respawn_signal(&self, apply: impl FnOnce(&mut State) -> Delivery) -> Delivery {
        let delivery = apply(&mut self.lock());
        if delivery == Delivery::Queued {
            self.ready.notify_one();
        }
        delivery
    }

    fn close(&self) {
        self.lock().close();
        self.ready.notify_one();
    }

    fn next(&self) -> Next {
        self.lock().next()
    }

    fn phase(&self) -> Phase {
        self.lock().phase
    }

    /// Locks the state. It stays consistent if a holder panicked, so a
    /// poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The host thread's side of the bridge. Clones feed the same session.
///
/// When the last clone is dropped, a session that hasn't ended ends as
/// `Disconnected(SessionCrashed)`: its events can't come anymore, for
/// example because its host thread is gone, and the actor must not wait for
/// them.
#[derive(Debug, Clone)]
pub(crate) struct EventSink {
    source: Arc<Source>,
}

/// What every clone of one sink shares; dropping it is dropping the last
/// clone.
#[derive(Debug)]
struct Source {
    shared: Arc<Shared>,
}

impl Drop for Source {
    fn drop(&mut self) {
        // A no-op once the session ended or was closed.
        if self
            .shared
            .terminate(Terminal::Disconnected(DisconnectReason::SessionCrashed))
        {
            warn!(
                bot_id = %self.shared.bot_id,
                "the session's event source went away without a terminal event, so it ended as crashed"
            );
        }
    }
}

impl EventSink {
    /// Maps one azalea event and applies the delivery rules.
    pub(crate) fn forward(&self, event: &Event) -> Delivery {
        self.source.shared.apply(map_event(event), Instant::now())
    }

    /// Ends the session with `terminal`, from a source other than azalea's
    /// events: the account's auth result, the connect timeout or `AppExit`.
    /// Returns whether it was the first terminal event, which is the one
    /// delivered.
    pub(crate) fn terminate(&self, terminal: Terminal) -> bool {
        self.source.shared.terminate(terminal)
    }

    /// Where the session is, as far as its events go.
    pub(crate) fn phase(&self) -> Phase {
        self.source.shared.phase()
    }

    /// A handle for the session's owner, which doesn't count as a source: the
    /// session still ends as crashed once every sink is gone.
    pub(crate) fn control(&self) -> BridgeControl {
        BridgeControl {
            shared: Arc::clone(&self.source.shared),
        }
    }
}

/// The owner's side of a session's bridge: it reads the phase, learns of
/// respawns asked for and of loads (ADR-0020, S22) and tears the bridge
/// down. Unlike an [`EventSink`], dropping it doesn't end the session, so
/// the owner can hold one without hiding a lost host thread.
#[derive(Debug, Clone)]
pub(crate) struct BridgeControl {
    shared: Arc<Shared>,
}

impl BridgeControl {
    /// Where the session is, as far as its events go.
    pub(crate) fn phase(&self) -> Phase {
        self.shared.phase()
    }

    /// The session asked the server to respawn the bot; `loaded` says
    /// whether its entity has `HasClientLoaded` right now (a server that
    /// respawned it by itself). A request counts only after the actor read
    /// the `Died`; `Respawned` follows once the bot is back (ADR-0020, S22,
    /// S27).
    pub(crate) fn respawn_asked(&self, loaded: bool) -> Delivery {
        self.shared
            .respawn_signal(|state| state.respawn_asked(loaded))
    }

    /// azalea reported the bot's death inside the frame (its `DeathEvent`),
    /// so a report reaches the bridge before any later load (ADR-0020,
    /// S27).
    pub(crate) fn died(&self) -> Delivery {
        self.shared.apply(Mapped::Death, Instant::now())
    }

    /// A Respawn packet removed the bot's `Dead` (ADR-0020, S27).
    pub(crate) fn dead_removed(&self) -> Delivery {
        self.shared.respawn_signal(State::dead_removed)
    }

    /// azalea added `HasClientLoaded` to the bot's entity again (ADR-0020,
    /// S27).
    pub(crate) fn loaded(&self) -> Delivery {
        self.shared.respawn_signal(State::loaded)
    }

    /// A Login or a Respawn packet arrived, read inside the frame after
    /// the frame's deaths (ADR-0036, D6).
    pub(crate) fn world_packet(&self, packet: WorldPacket) -> Delivery {
        self.shared
            .respawn_signal(|state| state.world_packet(packet))
    }

    /// The server placed the bot in its world and the client has loaded:
    /// `WorldChanged` follows if a packet moved the bot to another world
    /// since the join.
    pub(crate) fn placed(&self) -> Delivery {
        self.shared.respawn_signal(State::placed)
    }

    /// The session let go of inputs by itself: `LetGo` follows, once for
    /// each reason until it's read (ADR-0036, D10, D11).
    pub(crate) fn let_go(&self, reason: LetGo) -> Delivery {
        self.shared.respawn_signal(|state| state.let_go(reason))
    }

    /// A screen, a click or a sign editor reported `report`: its event
    /// follows, under the rules that keep them bounded (ADR-0051).
    pub(crate) fn menu(&self, report: MenuReport) -> Delivery {
        self.shared.respawn_signal(|state| state.menu(report))
    }

    /// Whether an action's fate is queued and not yet read: no action
    /// goes out until it is (ADR-0051).
    pub(crate) fn fate_unread(&self) -> bool {
        self.shared.lock().fates_unread > 0
    }

    /// Tears the bridge down: queued events are dropped, and
    /// [`McEvents::next`] returns `None`.
    pub(crate) fn close(&self) {
        self.shared.close();
    }
}

/// A session's events, in order (the `SessionEvents` of fleet-mc's
/// connector).
///
/// It keeps ADR-0010's contract: only chat is dropped when the queue is full,
/// and it's counted; `Joined` comes from the first spawn, and a death before
/// it is held until after it; `Died` comes once until `Respawned`, which
/// confirms, once per death, a respawn asked for after the actor read the
/// `Died` (ADR-0020, S22); the first terminal event is the last event, after
/// which [`next`](Self::next) returns `None`, as it does once the session is
/// torn down.
pub struct McEvents {
    shared: Arc<Shared>,
}

impl core::fmt::Debug for McEvents {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("McEvents")
            .field("bot_id", &self.shared.bot_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
impl McEvents {
    /// Everything queued right now, without waiting: for the unit tests of
    /// what a session's App reports.
    pub(crate) fn queued(&self) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        while let Next::Event(event) = self.shared.next() {
            out.push(event);
        }
        out
    }
}

impl SessionEvents for McEvents {
    /// Cancel-safe: an event leaves the queue only in the poll that returns
    /// it.
    fn next(&mut self) -> impl Future<Output = Option<SessionEvent>> + Send {
        let shared = Arc::clone(&self.shared);
        async move {
            loop {
                match shared.next() {
                    Next::Event(event) => return Some(event),
                    Next::End => return None,
                    Next::Wait => shared.ready.notified().await,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;
    use fleet_core::chat::IncomingChat;
    use fleet_core::disconnect::{ConnectFailure, DisconnectReason};
    use rstest::rstest;

    fn start(capacity: usize) -> (EventSink, McEvents, Arc<EventCounters>, Arc<LivenessStamps>) {
        let counters = Arc::new(EventCounters::default());
        let stamps = Arc::new(LivenessStamps::new(past()));
        let (sink, events) = bridge(
            "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            NonZeroUsize::new(capacity).unwrap(),
            Arc::clone(&counters),
            Arc::clone(&stamps),
        );
        (sink, events, counters, stamps)
    }

    fn past() -> Instant {
        Instant::now().checked_sub(Duration::from_secs(60)).unwrap()
    }

    fn apply(sink: &EventSink, mapped: Mapped) -> Delivery {
        sink.source.shared.apply(mapped, Instant::now())
    }

    fn chat(text: &str) -> Mapped {
        Mapped::Chat(IncomingChat::system(text))
    }

    fn chat_event(text: &str) -> SessionEvent {
        SessionEvent::Chat(IncomingChat::system(text))
    }

    fn kicked() -> Terminal {
        Terminal::Disconnected(DisconnectReason::ConnectionClosed)
    }

    /// Everything queued right now, without waiting.
    fn queued(events: &McEvents) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        while let Next::Event(event) = events.shared.next() {
            out.push(event);
        }
        out
    }

    // --- Order and lifecycle ---

    #[test]
    fn events_arrive_in_order() {
        let (sink, events, ..) = start(8);

        apply(&sink, Mapped::Spawn);
        apply(&sink, chat("hello"));
        apply(&sink, Mapped::Death);
        apply(&sink, Mapped::Terminal(kicked()));

        assert_eq!(
            queued(&events),
            [
                SessionEvent::Joined,
                chat_event("hello"),
                SessionEvent::Died,
                SessionEvent::Disconnected(DisconnectReason::ConnectionClosed),
            ]
        );
    }

    #[test]
    fn joined_comes_only_from_the_first_spawn() {
        let (sink, events, ..) = start(8);

        assert_eq!(apply(&sink, Mapped::Spawn), Delivery::Queued);
        assert_eq!(apply(&sink, Mapped::Spawn), Delivery::Ignored);

        assert_eq!(queued(&events), [SessionEvent::Joined]);
    }

    #[test]
    fn death_before_joined_is_held_until_after_it() {
        let (sink, events, ..) = start(8);

        assert_eq!(apply(&sink, Mapped::Death), Delivery::DeathHeld);
        assert_eq!(queued(&events), []);
        apply(&sink, Mapped::Spawn);

        assert_eq!(queued(&events), [SessionEvent::Joined, SessionEvent::Died]);
    }

    #[test]
    fn second_death_report_is_dropped_until_the_confirmed_respawn() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Spawn);

        assert_eq!(apply(&sink, Mapped::Death), Delivery::Queued);
        assert_eq!(apply(&sink, Mapped::Death), Delivery::DuplicateDeath);
        assert_eq!(queued(&events), [SessionEvent::Joined, SessionEvent::Died]);
        sink.control().dead_removed();
        sink.control().respawn_asked(true);
        assert_eq!(apply(&sink, Mapped::Death), Delivery::Queued);

        assert_eq!(
            queued(&events),
            [SessionEvent::Respawned, SessionEvent::Died]
        );
    }

    #[test]
    fn second_death_report_before_joined_is_dropped_too() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Death);

        assert_eq!(apply(&sink, Mapped::Death), Delivery::DuplicateDeath);
        apply(&sink, Mapped::Spawn);

        assert_eq!(queued(&events), [SessionEvent::Joined, SessionEvent::Died]);
    }

    // --- Overload ---

    #[test]
    fn full_queue_drops_only_chat_and_counts_it() {
        let (sink, events, counters, _) = start(2);
        apply(&sink, chat("one"));
        apply(&sink, chat("two"));

        let delivery = apply(&sink, chat("three"));

        assert_eq!(
            delivery,
            Delivery::ChatDropped {
                first_in_burst: true
            }
        );
        assert_eq!(counters.dropped_chat(), 1);
        assert_eq!(queued(&events), [chat_event("one"), chat_event("two")]);
    }

    #[test]
    fn lifecycle_events_are_delivered_past_the_capacity() {
        let (sink, events, ..) = start(1);
        apply(&sink, chat("fills the queue"));

        assert_eq!(apply(&sink, Mapped::Spawn), Delivery::Queued);
        assert_eq!(apply(&sink, Mapped::Death), Delivery::Queued);
        assert_eq!(apply(&sink, Mapped::Terminal(kicked())), Delivery::Queued);

        assert_eq!(
            queued(&events),
            [
                chat_event("fills the queue"),
                SessionEvent::Joined,
                SessionEvent::Died,
                SessionEvent::Disconnected(DisconnectReason::ConnectionClosed),
            ]
        );
    }

    #[test]
    fn only_the_first_drop_of_a_burst_starts_it() {
        let (sink, events, counters, _) = start(1);
        apply(&sink, chat("one"));

        assert_eq!(
            apply(&sink, chat("two")),
            Delivery::ChatDropped {
                first_in_burst: true
            }
        );
        assert_eq!(
            apply(&sink, chat("three")),
            Delivery::ChatDropped {
                first_in_burst: false
            }
        );
        queued(&events);
        assert_eq!(apply(&sink, chat("four")), Delivery::Queued);
        assert_eq!(
            apply(&sink, chat("five")),
            Delivery::ChatDropped {
                first_in_burst: true
            }
        );

        assert_eq!(counters.dropped_chat(), 3);
    }

    #[test]
    fn action_bar_messages_are_counted_apart_from_dropped_chat() {
        let (sink, events, counters, _) = start(8);

        assert_eq!(apply(&sink, Mapped::ActionBar), Delivery::ActionBarIgnored);

        assert_eq!(counters.ignored_action_bar(), 1);
        assert_eq!(counters.dropped_chat(), 0);
        assert_eq!(queued(&events), []);
    }

    #[test]
    fn counters_add_up_across_sessions() {
        let counters = Arc::new(EventCounters::default());
        let bot_id: BotId = "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap();
        let open = |counters: &Arc<EventCounters>| {
            bridge(
                bot_id,
                NonZeroUsize::MIN,
                Arc::clone(counters),
                Arc::new(LivenessStamps::new(Instant::now())),
            )
        };
        let (first, _first_events) = open(&counters);
        let (second, _second_events) = open(&counters);

        apply(&first, Mapped::ActionBar);
        apply(&second, Mapped::ActionBar);

        assert_eq!(counters.ignored_action_bar(), 2);
    }

    // --- Terminal events and teardown ---

    fn crashed() -> SessionEvent {
        SessionEvent::Disconnected(DisconnectReason::SessionCrashed)
    }

    #[test]
    fn dropping_every_sink_ends_a_live_session_as_crashed() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Spawn);
        let clone = sink.clone();

        drop(sink);
        assert_eq!(queued(&events), [SessionEvent::Joined]);
        drop(clone);

        assert_eq!(queued(&events), [crashed()]);
        assert!(matches!(events.shared.next(), Next::End));
    }

    #[rstest]
    #[case::after_a_terminal_event(true)]
    #[case::after_close(false)]
    fn dropping_the_sink_of_an_ended_session_changes_nothing(#[case] terminal: bool) {
        let (sink, events, ..) = start(8);
        if terminal {
            apply(&sink, Mapped::Terminal(kicked()));
        } else {
            sink.control().close();
        }

        drop(sink);

        let expected: &[SessionEvent] = if terminal {
            &[SessionEvent::Disconnected(
                DisconnectReason::ConnectionClosed,
            )]
        } else {
            &[]
        };
        assert_eq!(queued(&events), expected);
        assert!(matches!(events.shared.next(), Next::End));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_sink_wakes_a_waiting_consumer() {
        let (sink, mut events, ..) = start(8);
        let waiting = tokio::spawn(async move { events.next().await });
        tokio::task::yield_now().await;

        drop(sink);

        let event = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("the consumer wasn't woken");
        assert_eq!(event.unwrap(), Some(crashed()));
    }

    #[test]
    fn first_terminal_event_wins() {
        let (sink, events, ..) = start(8);

        assert!(sink.terminate(Terminal::Disconnected(DisconnectReason::AuthRejected)));
        assert_eq!(
            apply(&sink, Mapped::Terminal(kicked())),
            Delivery::SessionEnded
        );
        assert!(!sink.terminate(Terminal::ConnectionFailed(ConnectFailure::TimedOut)));

        assert_eq!(
            queued(&events),
            [SessionEvent::Disconnected(DisconnectReason::AuthRejected)]
        );
    }

    #[test]
    fn events_after_the_terminal_event_are_not_delivered() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Terminal(kicked()));

        assert_eq!(apply(&sink, chat("late")), Delivery::SessionEnded);
        assert_eq!(apply(&sink, Mapped::Spawn), Delivery::SessionEnded);

        assert_eq!(
            queued(&events),
            [SessionEvent::Disconnected(
                DisconnectReason::ConnectionClosed
            )]
        );
    }

    #[test]
    fn held_death_is_dropped_when_the_session_ends_before_joining() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Death);

        sink.terminate(Terminal::ConnectionFailed(ConnectFailure::Refused));
        apply(&sink, Mapped::Spawn);

        assert_eq!(
            queued(&events),
            [SessionEvent::ConnectionFailed(ConnectFailure::Refused)]
        );
    }

    #[test]
    fn close_drops_queued_events_and_ends_the_stream() {
        let (sink, events, ..) = start(8);
        apply(&sink, chat("queued"));

        sink.control().close();

        assert!(matches!(events.shared.next(), Next::End));
        assert_eq!(apply(&sink, chat("late")), Delivery::SessionEnded);
        assert!(!sink.terminate(kicked()));
    }

    #[tokio::test(start_paused = true)]
    async fn next_returns_none_after_the_terminal_event() {
        let (sink, mut events, ..) = start(8);
        apply(&sink, Mapped::Terminal(kicked()));

        assert_eq!(
            events.next().await,
            Some(SessionEvent::Disconnected(
                DisconnectReason::ConnectionClosed
            ))
        );
        assert_eq!(events.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn next_waits_for_an_event_that_comes_later() {
        let (sink, mut events, ..) = start(8);
        let waiting = tokio::spawn(async move { events.next().await });
        tokio::task::yield_now().await;

        apply(&sink, Mapped::Spawn);

        assert_eq!(waiting.await.unwrap(), Some(SessionEvent::Joined));
    }

    #[tokio::test(start_paused = true)]
    async fn next_is_cancel_safe() {
        let (sink, mut events, ..) = start(8);
        tokio::select! {
            _ = events.next() => panic!("nothing was queued"),
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }

        apply(&sink, Mapped::Spawn);

        assert_eq!(events.next().await, Some(SessionEvent::Joined));
    }

    #[tokio::test(start_paused = true)]
    async fn close_wakes_a_waiting_consumer() {
        let (sink, mut events, ..) = start(8);
        let waiting = tokio::spawn(async move { events.next().await });
        tokio::task::yield_now().await;

        sink.control().close();

        assert_eq!(waiting.await.unwrap(), None);
    }

    #[test]
    fn events_and_their_next_future_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        let (_, mut events, ..) = start(8);

        assert_send(&events.next());
        assert_send(&events);
    }

    // --- The owner's control ---

    #[test]
    fn control_follows_the_phase() {
        let (sink, _events, ..) = start(8);
        let control = sink.control();
        assert_eq!(control.phase(), Phase::Starting);

        apply(&sink, Mapped::Spawn);
        assert_eq!(control.phase(), Phase::Joined);
        assert_eq!(sink.phase(), Phase::Joined);

        apply(&sink, Mapped::Terminal(kicked()));
        assert_eq!(control.phase(), Phase::Ended);
    }

    #[test]
    fn control_close_tears_the_bridge_down() {
        let (sink, events, ..) = start(8);
        apply(&sink, chat("queued"));

        sink.control().close();

        assert_eq!(sink.phase(), Phase::Closed);
        assert!(matches!(events.shared.next(), Next::End));
    }

    // --- Respawn (ADR-0020, S22, S27) ---

    /// Joins, dies, and has the actor read both.
    fn joined_and_dead(sink: &EventSink, events: &McEvents) {
        apply(sink, Mapped::Spawn);
        assert_eq!(sink.control().died(), Delivery::Queued);
        assert_eq!(queued(events), [SessionEvent::Joined, SessionEvent::Died]);
    }

    #[test]
    fn a_respawn_asked_after_the_death_was_read_is_confirmed_once_the_bot_loads() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        joined_and_dead(&sink, &events);

        // The old life's client is still loaded while it's dead.
        let asked_while_dead = control.respawn_asked(true);
        let removed = control.dead_removed();
        let loaded = control.loaded();
        let loaded_again = control.loaded();

        assert_eq!(asked_while_dead, Delivery::Ignored);
        assert_eq!(removed, Delivery::Ignored, "a removal alone queues nothing");
        assert_eq!(loaded, Delivery::Queued);
        assert_eq!(loaded_again, Delivery::Ignored, "once per death");
        assert_eq!(queued(&events), [SessionEvent::Respawned]);
        assert_eq!(control.died(), Delivery::Queued);
        assert_eq!(queued(&events), [SessionEvent::Died]);
    }

    /// A Respawn and a load that no request after a read death waits for
    /// confirm nothing: a dimension change, or a stale call.
    #[rstest]
    #[case::alive_without_a_death(false, false)]
    #[case::dead_before_the_death_was_read(true, false)]
    #[case::dead_and_read_but_never_asked(true, true)]
    fn a_load_without_a_request_after_a_read_death_confirms_nothing(
        #[case] dies: bool,
        #[case] read: bool,
    ) {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        apply(&sink, Mapped::Spawn);
        if dies {
            control.died();
        }
        if read {
            queued(&events);
        }
        if dies && !read {
            // Asked before the actor read the death: a stale call.
            assert_eq!(control.respawn_asked(true), Delivery::Ignored);
        }

        control.dead_removed();
        let loaded = control.loaded();

        assert_eq!(loaded, Delivery::Ignored);
        assert!(
            !queued(&events).contains(&SessionEvent::Respawned),
            "a dimension change or a stale call confirms nothing"
        );
    }

    #[test]
    fn a_bot_the_server_respawned_by_itself_is_confirmed_when_the_respawn_is_asked() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        joined_and_dead(&sink, &events);
        control.dead_removed();
        assert_eq!(control.loaded(), Delivery::Ignored);

        let asked = control.respawn_asked(true);

        assert_eq!(asked, Delivery::Queued);
        assert_eq!(queued(&events), [SessionEvent::Respawned]);
    }

    /// A server that respawns the bot at once removes `Dead` in the death's
    /// own frame, before the death reaches the bridge: that removal still
    /// counts for the death (S27).
    #[test]
    fn a_removal_before_the_death_was_queued_counts_for_it() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        apply(&sink, Mapped::Spawn);

        control.dead_removed();
        control.died();
        let loaded_before_the_read = control.loaded();
        let read = queued(&events);
        let asked = control.respawn_asked(true);

        assert_eq!(loaded_before_the_read, Delivery::Ignored);
        assert_eq!(read, [SessionEvent::Joined, SessionEvent::Died]);
        assert_eq!(asked, Delivery::Queued);
        assert_eq!(queued(&events), [SessionEvent::Respawned]);
    }

    /// A removal no death waited for, as at a dimension change of a bot
    /// whose `Dead` azalea left behind, is forgotten at the load that
    /// follows it, so it can't confirm a later death before its own Respawn
    /// (S27).
    #[test]
    fn a_load_with_no_death_pending_forgets_a_removal() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        apply(&sink, Mapped::Spawn);
        control.dead_removed();
        control.loaded();
        control.died();
        queued(&events);

        let asked = control.respawn_asked(true);
        control.dead_removed();
        let loaded = control.loaded();

        assert_eq!(asked, Delivery::Ignored, "the old removal is forgotten");
        assert_eq!(loaded, Delivery::Queued, "its own removal and load");
        assert_eq!(queued(&events), [SessionEvent::Respawned]);
    }

    /// Each confirmation uses its removal up: the next death waits for its
    /// own (S27).
    #[test]
    fn each_confirmation_uses_its_removal_up() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        joined_and_dead(&sink, &events);
        control.respawn_asked(true);
        control.dead_removed();
        control.loaded();
        control.died();
        queued(&events);

        let asked = control.respawn_asked(true);
        let loaded = control.loaded();

        assert_eq!([asked, loaded], [Delivery::Ignored; 2]);
        assert_eq!(queued(&events), []);
    }

    #[test]
    fn second_death_report_before_the_confirmation_is_dropped() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        joined_and_dead(&sink, &events);
        control.respawn_asked(true);

        let again = control.died();
        control.dead_removed();
        control.loaded();

        assert_eq!(again, Delivery::DuplicateDeath);
        assert_eq!(queued(&events), [SessionEvent::Respawned]);
    }

    /// The bound the actor's drain relies on: a stale retry and a reload
    /// while the next death is still queued never add a lifecycle event, so
    /// at most `Respawned`, one `Died` and a terminal event are pending.
    #[test]
    fn stale_respawn_calls_never_hold_more_than_three_lifecycle_events() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        joined_and_dead(&sink, &events);
        control.respawn_asked(true);
        control.dead_removed();
        control.loaded();
        control.died();

        let stale = [
            control.respawn_asked(true),
            control.dead_removed(),
            control.loaded(),
            control.respawn_asked(true),
        ];
        apply(&sink, Mapped::Terminal(kicked()));

        assert_eq!(stale, [Delivery::Ignored; 4]);
        assert_eq!(
            queued(&events),
            [
                SessionEvent::Respawned,
                SessionEvent::Died,
                SessionEvent::Disconnected(DisconnectReason::ConnectionClosed),
            ]
        );
    }

    #[test]
    fn nothing_is_confirmed_after_a_terminal_event_or_a_close() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        joined_and_dead(&sink, &events);
        control.respawn_asked(true);
        apply(&sink, Mapped::Terminal(kicked()));

        let after_the_end = [control.dead_removed(), control.loaded()];
        control.close();
        let after_the_close = [control.respawn_asked(true), control.died()];

        assert_eq!(after_the_end, [Delivery::SessionEnded; 2]);
        assert_eq!(after_the_close, [Delivery::SessionEnded; 2]);
        assert!(matches!(events.shared.next(), Next::End));
    }

    #[test]
    fn a_held_control_doesnt_keep_the_session_alive() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        apply(&sink, Mapped::Spawn);

        drop(sink);

        assert_eq!(queued(&events), [SessionEvent::Joined, crashed()]);
        assert_eq!(control.phase(), Phase::Ended);
    }

    #[test]
    fn dropping_the_control_ends_nothing() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Spawn);

        drop(sink.control());

        assert_eq!(queued(&events), [SessionEvent::Joined]);
        assert_eq!(sink.phase(), Phase::Joined);
    }

    #[test]
    fn closing_before_the_sink_drops_means_no_crash() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Spawn);

        sink.control().close();
        drop(sink);

        assert_eq!(queued(&events), []);
        assert!(matches!(events.shared.next(), Next::End));
    }

    // --- The azalea side ---

    #[test]
    fn forwarded_ticks_and_keep_alives_only_stamp_liveness() {
        let (sink, events, _, stamps) = start(8);
        let before = stamps.read();

        assert_eq!(sink.forward(&Event::Tick), Delivery::Stamped);
        assert_eq!(sink.forward(&Event::KeepAlive(7)), Delivery::Stamped);

        assert!(stamps.read().last_tick > before.last_tick);
        assert!(stamps.read().last_packet > before.last_packet);
        assert_eq!(queued(&events), []);
    }

    #[test]
    fn forwarded_events_are_mapped_and_queued() {
        let (sink, events, ..) = start(8);

        assert_eq!(sink.forward(&Event::Spawn), Delivery::Queued);
        assert_eq!(sink.forward(&Event::Disconnect(None)), Delivery::Queued);

        assert_eq!(
            queued(&events),
            [
                SessionEvent::Joined,
                SessionEvent::Disconnected(DisconnectReason::ConnectionClosed),
            ]
        );
    }

    // --- A world change (Plan.md P15.14; ADR-0036, D6) ---

    /// A session whose bot joined, with `Joined` read.
    fn joined() -> (EventSink, McEvents) {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Spawn);
        assert_eq!(queued(&events), [SessionEvent::Joined]);
        (sink, events)
    }

    #[test]
    fn the_joins_own_login_is_no_world_change() {
        let (sink, events, ..) = start(8);

        assert_eq!(
            sink.control().world_packet(WorldPacket::Login),
            Delivery::Ignored
        );
        apply(&sink, Mapped::Spawn);
        assert_eq!(sink.control().placed(), Delivery::Ignored);

        assert_eq!(queued(&events), [SessionEvent::Joined]);
    }

    #[rstest]
    #[case::a_second_login(WorldPacket::Login)]
    #[case::a_respawn_without_a_death(WorldPacket::Respawn)]
    fn a_packet_into_another_world_is_a_world_change_once_the_bot_is_placed(
        #[case] packet: WorldPacket,
    ) {
        let (sink, events) = joined();

        assert_eq!(sink.control().world_packet(packet), Delivery::Ignored);
        let before_it_is_placed = queued(&events);
        assert_eq!(sink.control().placed(), Delivery::Queued);

        assert_eq!(before_it_is_placed, []);
        assert_eq!(queued(&events), [SessionEvent::WorldChanged]);
    }

    #[test]
    fn a_placement_without_a_packet_before_it_reports_nothing() {
        let (sink, events) = joined();

        assert_eq!(sink.control().placed(), Delivery::Ignored);
        sink.control().world_packet(WorldPacket::Login);
        assert_eq!(sink.control().placed(), Delivery::Queued);
        // Used up: the next placement is a teleport's, not a world's.
        assert_eq!(sink.control().placed(), Delivery::Ignored);

        assert_eq!(queued(&events), [SessionEvent::WorldChanged]);
    }

    #[test]
    fn a_deaths_respawn_is_no_world_change() {
        let (sink, events) = joined();
        sink.control().died();
        assert_eq!(queued(&events), [SessionEvent::Died]);
        sink.control().respawn_asked(false);

        // What a death's Respawn does: the removal, the packet, the load.
        sink.control().dead_removed();
        assert_eq!(
            sink.control().world_packet(WorldPacket::Respawn),
            Delivery::Ignored
        );
        assert_eq!(sink.control().loaded(), Delivery::Queued);
        assert_eq!(sink.control().placed(), Delivery::Ignored);

        assert_eq!(queued(&events), [SessionEvent::Respawned]);
    }

    #[test]
    fn a_respawn_packet_for_a_death_not_yet_read_is_no_world_change_either() {
        let (sink, events) = joined();
        sink.control().died();

        sink.control().world_packet(WorldPacket::Respawn);
        assert_eq!(sink.control().placed(), Delivery::Ignored);

        assert_eq!(queued(&events), [SessionEvent::Died]);
    }

    #[test]
    fn a_login_while_dead_is_a_world_change_all_the_same() {
        // A proxy moves a dead bot to another server: it's somewhere new.
        let (sink, events) = joined();
        sink.control().died();
        assert_eq!(queued(&events), [SessionEvent::Died]);

        sink.control().world_packet(WorldPacket::Login);
        assert_eq!(sink.control().placed(), Delivery::Queued);

        assert_eq!(queued(&events), [SessionEvent::WorldChanged]);
    }

    #[test]
    fn a_dimension_change_after_a_confirmed_respawn_is_a_world_change() {
        let (sink, events) = joined();
        sink.control().died();
        assert_eq!(queued(&events), [SessionEvent::Died]);
        sink.control().respawn_asked(false);
        sink.control().dead_removed();
        sink.control().world_packet(WorldPacket::Respawn);
        sink.control().loaded();
        sink.control().placed();
        assert_eq!(queued(&events), [SessionEvent::Respawned]);

        sink.control().world_packet(WorldPacket::Respawn);
        sink.control().loaded();
        assert_eq!(sink.control().placed(), Delivery::Queued);

        assert_eq!(queued(&events), [SessionEvent::WorldChanged]);
    }

    #[test]
    fn two_packets_before_one_placement_are_one_world_change() {
        let (sink, events) = joined();

        sink.control().world_packet(WorldPacket::Login);
        sink.control().world_packet(WorldPacket::Respawn);
        assert_eq!(sink.control().placed(), Delivery::Queued);
        assert_eq!(sink.control().placed(), Delivery::Ignored);

        assert_eq!(queued(&events), [SessionEvent::WorldChanged]);
    }

    #[test]
    fn a_world_change_not_yet_read_isnt_queued_twice() {
        let (sink, events) = joined();
        sink.control().world_packet(WorldPacket::Respawn);
        assert_eq!(sink.control().placed(), Delivery::Queued);

        sink.control().world_packet(WorldPacket::Respawn);
        assert_eq!(sink.control().placed(), Delivery::AlreadyPending);
        assert_eq!(queued(&events), [SessionEvent::WorldChanged]);
        // Read: the next one is reported again.
        sink.control().world_packet(WorldPacket::Respawn);
        assert_eq!(sink.control().placed(), Delivery::Queued);

        assert_eq!(queued(&events), [SessionEvent::WorldChanged]);
    }

    // --- What the session let go of (Plan.md P15.11, P15.12) ---

    #[test]
    fn a_let_go_is_reported_once_for_each_reason_until_it_is_read() {
        let (sink, events) = joined();
        let control = sink.control();

        assert_eq!(control.let_go(LetGo::Leash), Delivery::Queued);
        assert_eq!(control.let_go(LetGo::Leash), Delivery::AlreadyPending);
        assert_eq!(control.let_go(LetGo::ToolGuard), Delivery::Queued);
        assert_eq!(control.let_go(LetGo::ItemUsed), Delivery::Queued);
        assert_eq!(control.let_go(LetGo::ItemUsed), Delivery::AlreadyPending);

        assert_eq!(
            queued(&events),
            [
                SessionEvent::LetGo(LetGo::Leash),
                SessionEvent::LetGo(LetGo::ToolGuard),
                SessionEvent::LetGo(LetGo::ItemUsed),
            ]
        );
        assert_eq!(control.let_go(LetGo::Leash), Delivery::Queued);
        assert_eq!(queued(&events), [SessionEvent::LetGo(LetGo::Leash)]);
    }

    #[test]
    fn nothing_is_let_go_of_before_the_bot_joined() {
        let (sink, events, ..) = start(8);

        assert_eq!(sink.control().let_go(LetGo::Leash), Delivery::Ignored);
        apply(&sink, Mapped::Spawn);

        assert_eq!(queued(&events), [SessionEvent::Joined]);
    }

    #[test]
    fn a_flood_of_world_changes_and_let_gos_never_fills_the_queue() {
        // Whatever a server makes a session report, and however slowly its
        // actor reads: one world change and one let-go a reason are pending
        // at most, in a queue that holds one event before chat is dropped.
        let (sink, events, ..) = start(1);
        apply(&sink, Mapped::Spawn);
        let control = sink.control();

        for _ in 0..1_000 {
            control.world_packet(WorldPacket::Respawn);
            control.placed();
            for reason in LetGo::ALL {
                control.let_go(reason);
            }
        }

        assert_eq!(
            queued(&events),
            [
                SessionEvent::Joined,
                SessionEvent::WorldChanged,
                SessionEvent::LetGo(LetGo::Leash),
                SessionEvent::LetGo(LetGo::ToolGuard),
                SessionEvent::LetGo(LetGo::ItemUsed),
            ]
        );
    }

    #[rstest]
    #[case::after_a_terminal_event(false)]
    #[case::after_a_close(true)]
    fn nothing_new_is_reported_once_the_session_ended(#[case] closed: bool) {
        let (sink, events) = joined();
        let control = sink.control();
        control.world_packet(WorldPacket::Login);
        if closed {
            control.close();
        } else {
            sink.terminate(kicked());
        }

        assert_eq!(
            control.world_packet(WorldPacket::Login),
            Delivery::SessionEnded
        );
        assert_eq!(control.placed(), Delivery::SessionEnded);
        assert_eq!(control.let_go(LetGo::Leash), Delivery::SessionEnded);

        let left = queued(&events);
        assert!(
            !left.contains(&SessionEvent::WorldChanged)
                && !left.contains(&SessionEvent::LetGo(LetGo::Leash)),
            "{left:?}"
        );
    }

    #[tokio::test]
    async fn a_world_change_and_a_let_go_wake_a_waiting_consumer() {
        let (sink, mut events) = joined();
        let control = sink.control();
        let waiting = tokio::spawn(async move {
            let first = events.next().await;
            let second = events.next().await;
            (first, second)
        });
        tokio::task::yield_now().await;

        control.let_go(LetGo::ToolGuard);
        control.world_packet(WorldPacket::Login);
        control.placed();

        let (first, second) = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, Some(SessionEvent::LetGo(LetGo::ToolGuard)));
        assert_eq!(second, Some(SessionEvent::WorldChanged));
    }

    // --- Screens, clicks and sign editors (ADR-0051) ---

    use fleet_core::script::BlockPosition;
    use fleet_core::script::menu::{ClickFate, ClickGate, SignDesk, SignSide, WindowId};

    /// Two screens the session opened, in order.
    fn screens() -> (ScreenId, ScreenId) {
        let mut gate = ClickGate::new();
        (gate.opened(WindowId::new(1)), gate.opened(WindowId::new(2)))
    }

    fn fate(screen: ScreenId) -> ClickResult {
        ClickResult {
            screen,
            slot: Some(0),
            fate: ClickFate::Ok,
        }
    }

    fn editors() -> (SignEditor, SignEditor) {
        let mut desk = SignDesk::new();
        let at = BlockPosition { x: 1, y: 64, z: 2 };
        (
            desk.opened(at, SignSide::Front),
            desk.opened(at, SignSide::Back),
        )
    }

    #[test]
    fn a_screens_events_are_delivered_in_order() {
        let (sink, events) = joined();
        let control = sink.control();
        let (screen, _) = screens();
        assert_eq!(control.menu(MenuReport::Opened(screen)), Delivery::Queued);
        let opened = queued(&events);

        for report in [
            MenuReport::Updated(screen),
            MenuReport::Clicked(fate(screen)),
            MenuReport::Closed(screen),
        ] {
            assert_eq!(control.menu(report), Delivery::Queued, "{report:?}");
        }

        assert_eq!(opened, [SessionEvent::MenuOpened(screen)]);
        assert_eq!(
            queued(&events),
            [
                SessionEvent::MenuUpdated(screen),
                SessionEvent::Clicked(fate(screen)),
                SessionEvent::MenuClosed(screen),
            ]
        );
    }

    #[test]
    fn an_update_is_pending_once_a_screen_until_it_is_read() {
        let (sink, events) = joined();
        let control = sink.control();
        let (screen, _) = screens();
        control.menu(MenuReport::Opened(screen));

        assert_eq!(control.menu(MenuReport::Updated(screen)), Delivery::Queued);
        assert_eq!(
            control.menu(MenuReport::Updated(screen)),
            Delivery::AlreadyPending
        );
        let first = queued(&events);
        assert_eq!(control.menu(MenuReport::Updated(screen)), Delivery::Queued);

        assert_eq!(
            first,
            [
                SessionEvent::MenuOpened(screen),
                SessionEvent::MenuUpdated(screen)
            ]
        );
        assert_eq!(queued(&events), [SessionEvent::MenuUpdated(screen)]);
    }

    #[test]
    fn a_new_screen_drops_what_is_unread_of_the_screens_before_it() {
        let (sink, events) = joined();
        let control = sink.control();
        let (old, new) = screens();
        control.menu(MenuReport::Opened(old));
        control.menu(MenuReport::Updated(old));

        assert_eq!(control.menu(MenuReport::Opened(new)), Delivery::Queued);

        assert_eq!(queued(&events), [SessionEvent::MenuOpened(new)]);
    }

    #[test]
    fn the_close_of_a_screen_whose_opening_was_read_outlives_the_next_screen() {
        let (sink, events) = joined();
        let control = sink.control();
        let (old, new) = screens();
        control.menu(MenuReport::Opened(old));
        assert_eq!(queued(&events), [SessionEvent::MenuOpened(old)]);
        control.menu(MenuReport::Updated(old));
        control.menu(MenuReport::Closed(old));

        control.menu(MenuReport::Opened(new));

        assert_eq!(
            queued(&events),
            [SessionEvent::MenuClosed(old), SessionEvent::MenuOpened(new)]
        );
    }

    #[test]
    fn a_screen_that_closes_before_its_opening_was_read_takes_it_along() {
        let (sink, events) = joined();
        let control = sink.control();
        let (screen, _) = screens();
        control.menu(MenuReport::Opened(screen));
        control.menu(MenuReport::Updated(screen));

        assert_eq!(control.menu(MenuReport::Closed(screen)), Delivery::Ignored);

        assert_eq!(queued(&events), []);
    }

    #[test]
    fn a_fate_is_unread_until_the_events_deliver_it_and_is_never_dropped() {
        let (sink, events) = joined();
        let control = sink.control();
        let (old, new) = screens();
        control.menu(MenuReport::Opened(old));
        assert_eq!(queued(&events), [SessionEvent::MenuOpened(old)]);
        control.menu(MenuReport::Clicked(fate(old)));

        let before = control.fate_unread();
        control.menu(MenuReport::Opened(new));
        let delivered = queued(&events);

        assert!(before);
        assert!(!control.fate_unread());
        assert_eq!(
            delivered,
            [
                SessionEvent::Clicked(fate(old)),
                SessionEvent::MenuOpened(new)
            ]
        );
    }

    #[test]
    fn a_new_sign_editor_replaces_one_unread() {
        let (sink, events) = joined();
        let control = sink.control();
        let (first, second) = editors();

        control.menu(MenuReport::SignOpened(first));
        assert_eq!(
            control.menu(MenuReport::SignOpened(second)),
            Delivery::Queued
        );

        assert_eq!(queued(&events), [SessionEvent::SignOpened(second)]);
    }

    #[test]
    fn a_flood_of_screens_never_fills_the_queue() {
        let (sink, events) = joined();
        let control = sink.control();
        let mut gate = ClickGate::new();
        let mut desk = SignDesk::new();
        let at = BlockPosition { x: 0, y: 0, z: 0 };

        for n in 0..10_000 {
            let screen = gate.opened(WindowId::new(n % 100 + 1));
            control.menu(MenuReport::Opened(screen));
            control.menu(MenuReport::Updated(screen));
            control.menu(MenuReport::SignOpened(desk.opened(at, SignSide::Front)));
            if n % 2 == 0 {
                control.menu(MenuReport::Closed(screen));
            }
        }

        let left = queued(&events);
        assert!(!left.is_empty() && left.len() <= 3, "{left:?}");
    }

    #[test]
    fn nothing_of_a_screen_is_reported_before_the_join_or_after_the_end() {
        let (sink, events, ..) = start(8);
        let control = sink.control();
        let (screen, _) = screens();

        let before = control.menu(MenuReport::Opened(screen));
        apply(&sink, Mapped::Spawn);
        sink.terminate(kicked());
        let after = control.menu(MenuReport::Opened(screen));

        assert_eq!(before, Delivery::Ignored);
        assert_eq!(after, Delivery::SessionEnded);
        assert!(
            !queued(&events).contains(&SessionEvent::MenuOpened(screen)),
            "no screen reported"
        );
    }
}
