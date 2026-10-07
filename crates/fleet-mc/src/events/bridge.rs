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
use fleet_core::mc::{SessionEvent, SessionEvents};
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
    /// A death already reported and not yet answered with a respawn.
    DuplicateDeath,
    /// Nothing the session reports, such as a second spawn.
    Ignored,
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
    /// Chat is being dropped: the next drop continues the burst.
    dropping_chat: bool,
}

/// Where the session is, as far as its events go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
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

/// The death the session has reported, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Death {
    /// None since the last respawn.
    #[default]
    None,
    /// The bot died before `Joined`; `Died` is queued right after it.
    Held,
    /// `Died` was queued and not yet answered with a respawn.
    Reported,
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

    /// The bot respawned, so its next death is reported again.
    fn respawned(&mut self) {
        if self.death == Death::Reported {
            self.death = Death::None;
        }
    }

    /// Tears the bridge down and drops what's queued.
    fn close(&mut self) {
        self.phase = Phase::Closed;
        self.death = Death::None;
        self.queue.clear();
    }

    fn next(&mut self) -> Next {
        if self.phase == Phase::Closed {
            return Next::End;
        }
        match self.queue.pop_front() {
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

    fn respawned(&self) {
        self.lock().respawned();
    }

    fn close(&self) {
        self.lock().close();
        self.ready.notify_one();
    }

    fn next(&self) -> Next {
        self.lock().next()
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

    /// The bot respawned, so its next death is reported again.
    pub(crate) fn respawned(&self) {
        self.source.shared.respawned();
    }

    /// Tears the bridge down: queued events are dropped, and
    /// [`McEvents::next`] returns `None`.
    pub(crate) fn close(&self) {
        self.source.shared.close();
    }
}

/// A session's events, in order (the `SessionEvents` of fleet-mc's
/// connector).
///
/// It keeps ADR-0010's contract: only chat is dropped when the queue is full,
/// and it's counted; `Joined` comes from the first spawn, and a death before
/// it is held until after it; `Died` comes once until the respawn; the first
/// terminal event is the last event, after which [`next`](Self::next)
/// returns `None`, as it does once the session is torn down.
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
    fn second_death_report_is_dropped_until_the_respawn() {
        let (sink, events, ..) = start(8);
        apply(&sink, Mapped::Spawn);

        assert_eq!(apply(&sink, Mapped::Death), Delivery::Queued);
        assert_eq!(apply(&sink, Mapped::Death), Delivery::DuplicateDeath);
        sink.respawned();
        assert_eq!(apply(&sink, Mapped::Death), Delivery::Queued);

        assert_eq!(
            queued(&events),
            [SessionEvent::Joined, SessionEvent::Died, SessionEvent::Died]
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
            sink.close();
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

        sink.close();

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

        sink.close();

        assert_eq!(waiting.await.unwrap(), None);
    }

    #[test]
    fn events_and_their_next_future_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        let (_, mut events, ..) = start(8);

        assert_send(&events.next());
        assert_send(&events);
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
}
