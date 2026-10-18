//! [`Outbox`]: everything the agent sends the server (Plan.md P10.7;
//! ADR-0019, S66, R47–R51), bounded by class, and [`Outbound`], the
//! `Connect` request stream that drains it.
//!
//! - **Nothing on a bot's path waits.** One `std` mutex guards the state,
//!   never held across an `.await`; the request body is a `Stream` that
//!   tonic pulls when HTTP/2 has room, so there's no writer task and no
//!   channel.
//! - **The classes** (`state`): session requests on their own lane, one per
//!   bot, which refuses when full; each bot's latest status, its death and
//!   its removal; alerts and chat answers in order; chat, the oldest dropped
//!   past 64 per bot or 1024 in all. Past a never-drop class's cap, which an
//!   honest run can't reach, the item is refused, counted with one `warn`,
//!   and the stream ends, so the next hello resyncs.
//! - **The order:** the hello first; nothing until `Welcome`; statuses and
//!   heartbeats until the stream's full reconcile (the server would drop the
//!   rest, and deny session requests); then requests, statuses each with
//!   its bot's death, notices, removals and chat; a heartbeat only after one
//!   Welcome interval of nothing sent.
//! - **Across an outage:** the hello's snapshot is the truth, so statuses
//!   are dropped before it (`prepare`); a removal of a bot the next hello
//!   doesn't list is dropped, since that hello frees the bot; the rest waits,
//!   bounded, for the next reconcile, which drops what's left of a bot it
//!   doesn't give. The lane closes at a stream's end: its unsent requests
//!   are dropped, and the credentials fall back to their kept grants (S63).
//! - **Logs and `Debug`** show counts only, never chat.

mod state;
mod stream;

use core::fmt;
use core::num::NonZeroUsize;
use core::task::Waker;
use core::time::Duration;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::id::{BotId, ChatRequestId};
use fleet_proto::control::{ChatFailedReason, Hello, SessionRequest};
use tracing::{debug, warn};

use self::state::{Caps, Chat, LaneRefusal, Notice, Pushed, State};
pub use self::stream::Outbound;
use crate::managed::credentials::{LinkGeneration, SessionLink, Unsent};

/// The outbox; see the [module docs](self). Clones share it.
#[derive(Clone)]
pub struct Outbox {
    shared: Arc<Mutex<Inner>>,
}

/// What the mutex guards.
#[derive(Debug)]
struct Inner {
    state: State,
    /// The request stream's waker while it waits.
    waker: Option<Waker>,
    /// Whether this stream's refusal was logged at `warn`.
    refusal_warned: bool,
    /// Whether this burst of chat drops was logged at `warn`.
    chat_warned: bool,
}

impl Inner {
    /// Wakes the request stream: something waits.
    fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    /// Logs what `pushed` says, once at `warn` per stream or burst.
    fn pushed(&mut self, pushed: Pushed, class: &'static str) {
        match pushed {
            Pushed::Queued => {}
            Pushed::DroppedOldest if self.chat_warned => {
                debug!(
                    dropped = self.state.counts().chat_dropped,
                    "the outbox dropped its oldest chat"
                );
            }
            Pushed::DroppedOldest => {
                self.chat_warned = true;
                warn!(
                    dropped = self.state.counts().chat_dropped,
                    "the outbox is full of chat and drops the oldest"
                );
            }
            Pushed::Refused if self.refusal_warned => {
                debug!(
                    class,
                    refused = self.state.counts().refused,
                    "the outbox refused an item"
                );
            }
            Pushed::Refused => {
                self.refusal_warned = true;
                warn!(
                    class,
                    refused = self.state.counts().refused,
                    "the outbox is full and refused an item; the stream ends, and the next hello resyncs"
                );
            }
        }
        self.wake();
    }
}

impl Outbox {
    /// An empty outbox for a fleet of `max_bots` bots whose chat queues hold
    /// `chat_queue` messages, no stream.
    #[must_use]
    pub fn new(max_bots: NonZeroUsize, chat_queue: NonZeroUsize) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Inner {
                state: State::new(Caps::of(max_bots.get(), chat_queue.get())),
                waker: None,
                refusal_warned: false,
                chat_warned: false,
            })),
        }
    }

    /// Locks the state, poison-tolerantly: every section leaves it
    /// consistent.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Before the hello's snapshot (R48): the statuses waiting are dropped.
    pub fn prepare(&self) {
        self.lock().state.prepare();
    }

    /// Opens a stream with `hello`: its generation, and the request stream
    /// that sends it, then what the stream may send.
    #[must_use]
    pub fn open(&self, hello: Hello) -> (LinkGeneration, Outbound) {
        let mut inner = self.lock();
        let generation = inner.state.open(hello);
        inner.refusal_warned = false;
        inner.wake();
        drop(inner);
        (
            LinkGeneration::new(generation),
            Outbound::new(Arc::clone(&self.shared), generation),
        )
    }

    /// The stream of `generation` was welcomed, with heartbeats every
    /// `interval` of nothing sent.
    pub fn welcomed(&self, generation: LinkGeneration, interval: Duration) {
        let mut inner = self.lock();
        inner.state.welcomed(generation.get(), interval);
        inner.wake();
    }

    /// The stream of `generation` took its full reconcile, which gives the
    /// agent `given` (R49).
    pub fn reconciled(&self, generation: LinkGeneration, given: &BTreeSet<BotId>) {
        let mut inner = self.lock();
        inner.state.reconciled(generation.get(), given);
        inner.wake();
    }

    /// The stream of `generation` ended.
    pub fn ended(&self, generation: LinkGeneration) {
        let mut inner = self.lock();
        inner.state.ended(generation.get());
        inner.wake();
    }

    /// Whether the stream of `generation` is welcomed and hasn't ended: a
    /// one-shot of its applies only then.
    #[must_use]
    pub fn is_live(&self, generation: LinkGeneration) -> bool {
        self.lock().state.live() == Some(generation.get())
    }

    /// Whether a never-drop class overflowed during the current stream,
    /// which ended it.
    #[must_use]
    pub fn overflowed(&self) -> bool {
        self.lock().state.overflowed()
    }

    /// Queues a bot's status.
    pub fn status(&self, snapshot: BotSnapshot) {
        let mut inner = self.lock();
        let pushed = inner.state.status(snapshot);
        inner.pushed(pushed, "per_bot");
    }

    /// Queues a bot's death.
    pub fn died(&self, bot: BotId, at: DateTime<Utc>) {
        let mut inner = self.lock();
        let pushed = inner.state.died(bot, at);
        inner.pushed(pushed, "per_bot");
    }

    /// Queues a bot's removal: its last event.
    pub fn removed(&self, bot: BotId, at: DateTime<Utc>) {
        let mut inner = self.lock();
        let pushed = inner.state.removed(bot, at);
        inner.pushed(pushed, "per_bot");
    }

    /// Queues a bot's alert.
    pub fn alert(&self, bot: BotId, at: DateTime<Utc>, notification: BotNotification) {
        self.notice(bot, at, Notice::Alert(notification));
    }

    /// Queues the answer that a user's chat message was sent.
    pub fn chat_sent(&self, bot: BotId, at: DateTime<Utc>, request: ChatRequestId) {
        self.notice(bot, at, Notice::ChatSent(request));
    }

    /// Queues the answer that a user's chat message wasn't sent.
    pub fn chat_failed(
        &self,
        bot: BotId,
        at: DateTime<Utc>,
        request: ChatRequestId,
        reason: ChatFailedReason,
    ) {
        self.notice(bot, at, Notice::ChatFailed(request, reason));
    }

    fn notice(&self, bot: BotId, at: DateTime<Utc>, notice: Notice) {
        let mut inner = self.lock();
        let pushed = inner.state.notice(bot, at, notice);
        inner.pushed(pushed, "notices");
    }

    /// Queues chat a bot received.
    pub fn received(&self, bot: BotId, at: DateTime<Utc>, chat: IncomingChat) {
        self.chat(bot, at, Chat::Received(chat));
    }

    /// Queues chat a bot's mode sent.
    pub fn mode_sent(&self, bot: BotId, at: DateTime<Utc>, message: ChatMessage) {
        self.chat(bot, at, Chat::ModeSent(message));
    }

    fn chat(&self, bot: BotId, at: DateTime<Utc>, chat: Chat) {
        let mut inner = self.lock();
        let pushed = inner.state.chat(bot, at, chat);
        inner.pushed(pushed, "chat");
    }
}

/// The lane (S62, S63, R51): `NoStream` until a stream is welcomed and
/// after it ends, `QueueFull` when the lane is.
impl SessionLink for Outbox {
    fn send(&self, request: SessionRequest) -> Result<LinkGeneration, Unsent> {
        let mut inner = self.lock();
        let sent = inner.state.request(request);
        if sent.is_ok() {
            inner.wake();
        }
        drop(inner);
        sent.map(LinkGeneration::new)
            .map_err(|refusal| match refusal {
                LaneRefusal::NoStream => Unsent::NoStream,
                LaneRefusal::Full => Unsent::QueueFull,
            })
    }
}

/// Counts only: how much waits by class, and what was dropped or refused.
impl fmt::Debug for Outbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        let [lane, per_bot, notices, chat] = inner.state.waiting();
        let counts = inner.state.counts();
        f.debug_struct("Outbox")
            .field("lane", &lane)
            .field("per_bot", &per_bot)
            .field("notices", &notices)
            .field("chat", &chat)
            .field("chat_dropped", &counts.chat_dropped)
            .field("refused", &counts.refused)
            .field("requests_dropped", &counts.requests_dropped)
            .finish()
    }
}
