//! The internal event bus (Plan.md P10.8, P11.5; ADR-0019, S70; ADR-0020,
//! R13): what agents report about their bots, as the server takes it in,
//! and the server's own hints, for the live events sockets to filter per
//! viewer and send on.
//!
//! - **What it carries:** [`BusEvent`]s. An agent's: a bot's new state, a
//!   death, an alert, the outcome of a chat message the server sent, and a
//!   chat message stored with its row's ID; with a user's line, published by
//!   its send, and a line its send couldn't hand on. Only values the
//!   stream's edge validated or cleaned, and nothing secret: no report
//!   holds a token, and session requests never come here. The server's own
//!   ([`LiveHints`]): which bots may read differently, a mode that changed,
//!   a user whose role changed, each fact-free (S32, S33).
//! - **When:** chat once its batch committed, so the live console shows
//!   exactly what the history stores and merges with it by ID; a user's
//!   line once its send's transaction committed, before the line goes to
//!   the agent, so its outcome always comes after it (S34); the server's
//!   hints once their commit returned (S42); everything else at once, never
//!   behind chat.
//! - **How:** a tokio `broadcast` of [`EVENT_BUS_CAPACITY`]. Publishing never
//!   waits, and an event nobody listens to is dropped at once. A subscriber
//!   that falls behind is told it lagged, and resyncs (§6 row 12).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot};
use fleet_core::id::{AgentId, BotId, ChatRequestId, UserId};
use fleet_proto::control::ChatFailedReason;
use tokio::sync::broadcast;

use crate::ports::chat::ListedChat;
use crate::ports::live_hints::{LiveHints, ModeEvent};

/// How many events the bus keeps for a subscriber that falls behind, as the
/// runtime's own event buffer (ADR-0013).
pub const EVENT_BUS_CAPACITY: usize = 1024;

/// One event on the bus (R13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusEvent {
    /// What an agent reported, or a user's line on its way to one.
    Agent(LiveEvent),
    /// What these bots read as may have changed: the server's own hint
    /// (S32, S42).
    Bots(Box<[BotId]>),
    /// A mode was created, changed or deleted (S33).
    Mode(ModeEvent),
    /// A user's role changed (R11).
    RoleChanged {
        /// Whose.
        user: UserId,
    },
}

impl BusEvent {
    /// The agent's event this is, if it's one.
    #[must_use]
    pub const fn agent(&self) -> Option<&LiveEvent> {
        match self {
            Self::Agent(event) => Some(event),
            Self::Bots(_) | Self::Mode(_) | Self::RoleChanged { .. } => None,
        }
    }
}

/// An event about one bot, from its agent or on its way to one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveEvent {
    /// The bot it's about.
    pub bot: BotId,
    /// The agent that reported it, which P11.5 shows only to Admins and the
    /// Owner (S54).
    pub agent: AgentId,
    /// The generation of the stream that reported it.
    pub generation: u64,
    /// When the server received it, by its own clock (S5).
    pub at: DateTime<Utc>,
    /// What happened.
    pub kind: LiveEventKind,
}

/// What happened, in a [`LiveEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveEventKind {
    /// The bot's new state.
    Status(BotSnapshot),
    /// The bot died and respawns.
    Died,
    /// An alert for a human.
    Alert(BotNotification),
    /// A chat message the server sent went out (P11.3).
    ChatSent(ChatRequestId),
    /// A chat message the server sent didn't, and why (P11.3).
    ChatFailed(ChatRequestId, ChatFailedReason),
    /// A chat message as its row stores it, the sender's username with a
    /// user's line.
    Chat {
        /// The message, its row's ID the history's cursor.
        line: ListedChat,
        /// A user's line: its chat request, as the send answered it (S34).
        request: Option<ChatRequestId>,
    },
    /// A user's line was stored, and its send didn't hand it to the agent
    /// (S34, R35).
    ChatNotSent {
        /// Its chat request.
        request: ChatRequestId,
        /// Why.
        why: NotSent,
    },
}

/// Why a stored user's line didn't go to its agent (R35).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSent {
    /// The agent didn't hold the bot, its stream was gone, or its queue was
    /// full: its sender got 409 `bot_unreachable`.
    Unreachable,
    /// The line couldn't be put on the wire: its sender got the internal
    /// error.
    Unsendable,
}

/// The bus; clones share it. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<BusEvent>>,
}

impl EventBus {
    /// An empty bus of [`EVENT_BUS_CAPACITY`].
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_BUS_CAPACITY);
        Self { sender }
    }

    /// Publishes the agent's `event` to every subscriber; never waits.
    pub fn publish(&self, event: LiveEvent) {
        self.send(BusEvent::Agent(event));
    }

    /// A subscriber of every event published from now on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<BusEvent>> {
        self.sender.subscribe()
    }

    /// Publishes `event`; never waits.
    fn send(&self, event: BusEvent) {
        // Without a subscriber the event goes nowhere, which is fine.
        let _ = self.sender.send(Arc::new(event));
    }
}

impl LiveHints for EventBus {
    fn bots(&self, bots: &[BotId]) {
        if !bots.is_empty() {
            self.send(BusEvent::Bots(bots.into()));
        }
    }

    fn mode(&self, event: ModeEvent) {
        self.send(BusEvent::Mode(event));
    }

    fn role_changed(&self, user: UserId) {
        self.send(BusEvent::RoleChanged { user });
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::bot::BotState;
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    use super::*;

    fn event(n: u8) -> LiveEvent {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        LiveEvent {
            bot: BotId::new_v7(at, [n; 10]).unwrap(),
            agent: AgentId::new_v7(at, [1; 10]).unwrap(),
            generation: 1,
            at,
            kind: LiveEventKind::Status(BotSnapshot {
                bot_id: BotId::new_v7(at, [n; 10]).unwrap(),
                state: BotState::Stopped,
                since: at,
                last_disconnect: None,
            }),
        }
    }

    #[test]
    fn a_subscriber_gets_every_event_published_after_it_subscribed() {
        let bus = EventBus::new();
        bus.publish(event(1));
        let mut events = bus.subscribe();

        bus.publish(event(2));
        bus.publish(event(3));

        assert_eq!(*events.try_recv().unwrap(), BusEvent::Agent(event(2)));
        assert_eq!(*events.try_recv().unwrap(), BusEvent::Agent(event(3)));
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn the_servers_hints_reach_subscribers_in_order_and_no_bots_none() {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let bot = BotId::new_v7(at, [9; 10]).unwrap();
        let user = UserId::new_v7(at, [8; 10]).unwrap();
        let mode = fleet_core::id::ModeId::new_v7(at, [7; 10]).unwrap();
        let bus = EventBus::new();
        let mut events = bus.subscribe();

        bus.bots(&[]);
        bus.bots(&[bot]);
        bus.mode(ModeEvent {
            id: mode,
            was: None,
        });
        bus.role_changed(user);

        assert_eq!(
            *events.try_recv().unwrap(),
            BusEvent::Bots(vec![bot].into())
        );
        assert_eq!(
            *events.try_recv().unwrap(),
            BusEvent::Mode(ModeEvent {
                id: mode,
                was: None
            })
        );
        assert_eq!(*events.try_recv().unwrap(), BusEvent::RoleChanged { user });
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn only_an_agents_event_reads_as_one() {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let user = UserId::new_v7(at, [8; 10]).unwrap();

        assert_eq!(BusEvent::Agent(event(1)).agent(), Some(&event(1)));
        assert_eq!(BusEvent::Bots(Box::new([])).agent(), None);
        assert_eq!(BusEvent::RoleChanged { user }.agent(), None);
    }

    #[test]
    fn with_no_subscriber_an_event_is_dropped_at_once() {
        let bus = EventBus::new();

        for n in 0..=255 {
            bus.publish(event(n));
        }
        let mut events = bus.subscribe();

        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn a_subscriber_that_falls_behind_is_told_how_many_it_missed() {
        let bus = EventBus::new();
        let mut events = bus.subscribe();

        for n in 0..EVENT_BUS_CAPACITY + 3 {
            bus.publish(event(u8::try_from(n % 256).unwrap()));
        }

        assert_eq!(events.recv().await, Err(RecvError::Lagged(3)));
    }
}
