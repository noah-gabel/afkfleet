//! The internal event bus (Plan.md P10.8; ADR-0019, S70): what agents
//! report about their bots, as the server takes it in, for P11.5's
//! WebSocket to filter per user and send on.
//!
//! - **What it carries:** a bot's new state, a death, an alert, the outcome
//!   of a chat message the server sent, and a chat message stored with its
//!   row's ID. Only values the stream's edge validated or cleaned, and
//!   nothing secret: no report holds a token, and session requests never
//!   come here.
//! - **When:** chat once its batch committed, so the live console shows
//!   exactly what the history stores and merges with it by ID; everything
//!   else at once, never behind chat.
//! - **How:** a tokio `broadcast` of [`EVENT_BUS_CAPACITY`]. Publishing never
//!   waits, and an event nobody listens to is dropped at once. A subscriber
//!   that falls behind is told it lagged, and resyncs (§6 row 12).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot};
use fleet_core::id::{AgentId, BotId, ChatRequestId};
use fleet_proto::control::ChatFailedReason;
use tokio::sync::broadcast;

use crate::ports::chat::{ChatEntry, ChatMessageId};

/// How many events the bus keeps for a subscriber that falls behind, as the
/// runtime's own event buffer (ADR-0013).
pub const EVENT_BUS_CAPACITY: usize = 1024;

/// One event on the bus.
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
    /// A chat message stored as row `id`.
    Chat {
        /// Its row's ID, the history's cursor.
        id: ChatMessageId,
        /// The message.
        entry: ChatEntry,
    },
}

/// The bus; clones share it. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<LiveEvent>>,
}

impl EventBus {
    /// An empty bus of [`EVENT_BUS_CAPACITY`].
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_BUS_CAPACITY);
        Self { sender }
    }

    /// Publishes `event` to every subscriber; never waits.
    pub fn publish(&self, event: LiveEvent) {
        // Without a subscriber the event goes nowhere, which is fine.
        let _ = self.sender.send(Arc::new(event));
    }

    /// A subscriber of every event published from now on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LiveEvent>> {
        self.sender.subscribe()
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

        assert_eq!(*events.try_recv().unwrap(), event(2));
        assert_eq!(*events.try_recv().unwrap(), event(3));
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
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
