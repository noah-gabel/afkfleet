//! [`ChatQueue`]: a bot's outbound chat, and [`ChatDelivery`], the task that
//! sends it.

use core::num::NonZeroUsize;

use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_core::mc::{SessionError, SessionHandle};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{ChatBucket, ChatError, ChatFailure, ChatTicket, ChatTickets};
use crate::clock::RuntimeClock;
use crate::event::{FleetEvent, FleetEventKind};
use crate::failure_log::{FailureKind, FailureLog, warn_or_debug};

/// One message in a bot's chat queue.
#[derive(Debug)]
enum Outgoing {
    /// User chat, with the ticket its caller got.
    User {
        ticket: ChatTicket,
        message: ChatMessage,
    },
    /// Chat from the bot's mode.
    Mode { message: ChatMessage },
}

/// The open side of a [`ChatQueue`]: where messages go while the bot is
/// Online.
#[derive(Debug)]
struct Open {
    sender: mpsc::Sender<Outgoing>,
    cancel: CancellationToken,
}

/// A bot's outbound chat queue (Plan.md P4.5; ADR-0013). The bot's actor owns
/// it.
///
/// While the bot is Online the queue is [open](Self::open): messages go to
/// the session through its [`ChatDelivery`] task, one at a time, in order.
/// Otherwise [`send`](Self::send) refuses with [`ChatError::NotOnline`]. The
/// bucket outlives the sessions, so a reconnect doesn't refill it.
#[derive(Debug)]
pub struct ChatQueue {
    bot_id: BotId,
    capacity: NonZeroUsize,
    bucket: ChatBucket,
    tickets: ChatTickets,
    open: Option<Open>,
}

impl ChatQueue {
    /// Builds a closed queue for `bot_id` that holds `capacity` messages
    /// while it's open. `bucket` is the bot's rate limit, and `tickets` the
    /// fleet's ticket counter.
    #[must_use]
    pub fn new(
        bot_id: BotId,
        capacity: NonZeroUsize,
        bucket: ChatBucket,
        tickets: ChatTickets,
    ) -> Self {
        Self {
            bot_id,
            capacity,
            bucket,
            tickets,
            open: None,
        }
    }

    /// Opens the queue for a session that just went Online. A queue that's
    /// still open for an earlier session is [closed](Self::close) first.
    ///
    /// Returns the session's [`ChatDelivery`], which the caller spawns, and
    /// the [`ModeChat`] handle for the session's mode runner. The delivery
    /// publishes on `events`, stamped by `clock`, and ends when `cancel` is
    /// cancelled, which [`close`](Self::close) does. The actor opens the
    /// queue before it starts the mode runner (ADR-0013).
    #[must_use = "nothing is sent until the delivery runs"]
    pub fn open<S: SessionHandle>(
        &mut self,
        session: S,
        events: broadcast::Sender<FleetEvent>,
        clock: RuntimeClock,
        cancel: CancellationToken,
    ) -> (ChatDelivery<S>, ModeChat) {
        self.close();
        let (sender, queue) = mpsc::channel(self.capacity.get());
        self.open = Some(Open {
            sender: sender.clone(),
            cancel: cancel.clone(),
        });
        let delivery = ChatDelivery {
            bot_id: self.bot_id,
            session,
            queue,
            events,
            clock,
            cancel,
            failures: FailureLog::default(),
        };
        let mode_chat = ModeChat {
            sender,
            bucket: self.bucket.clone(),
        };
        (delivery, mode_chat)
    }

    /// Closes the queue: the session's [`ChatDelivery`] stops and fails what's
    /// still queued with [`ChatFailure::Disconnected`].
    /// From then on [`send`](Self::send) and [`ModeChat::send`] refuse with
    /// [`ChatError::NotOnline`]. Closing a closed queue does nothing.
    ///
    /// The actor stops the mode runner before it closes the queue, so mode
    /// chat never meets a closed queue (ADR-0013).
    pub fn close(&mut self) {
        if let Some(open) = self.open.take() {
            open.cancel.cancel();
        }
    }

    /// Queues a user's chat message and returns its ticket. The outcome comes
    /// later as a `ChatSent` or `ChatFailed` event with that ticket.
    ///
    /// # Errors
    /// At once, without waiting: [`ChatError::NotOnline`] while the queue is
    /// closed, [`ChatError::QueueFull`] if the queue is full, and
    /// [`ChatError::RateLimited`] if the bucket is empty.
    pub fn send(&self, message: ChatMessage) -> Result<ChatTicket, ChatError> {
        let open = self.open.as_ref().ok_or(ChatError::NotOnline)?;
        let slot = admit(&open.sender, &self.bucket)?;
        let ticket = self.tickets.next();
        slot.send(Outgoing::User { ticket, message });
        Ok(ticket)
    }
}

/// Admits one message: reserves a queue slot, then takes a token. The slot
/// comes first, so a full queue uses up no token, and a refused message
/// gives its slot back.
fn admit<'a>(
    sender: &'a mpsc::Sender<Outgoing>,
    bucket: &ChatBucket,
) -> Result<mpsc::Permit<'a, Outgoing>, ChatError> {
    let slot = sender.try_reserve().map_err(|error| match error {
        TrySendError::Full(()) => ChatError::QueueFull,
        TrySendError::Closed(()) => ChatError::NotOnline,
    })?;
    if bucket.try_take() {
        Ok(slot)
    } else {
        Err(ChatError::RateLimited)
    }
}

/// The mode runner's handle to the chat queue of one session. Mode chat
/// shares the queue and the bucket with user chat, but has no ticket.
#[derive(Debug, Clone)]
pub struct ModeChat {
    sender: mpsc::Sender<Outgoing>,
    bucket: ChatBucket,
}

impl ModeChat {
    /// Queues a chat message of the bot's mode. If the session sends it, a
    /// `ModeChatSent` event follows; if it doesn't, that's only logged.
    ///
    /// # Errors
    /// As for [`ChatQueue::send`]: [`ChatError::NotOnline`] once the queue
    /// is closed, [`ChatError::QueueFull`] and [`ChatError::RateLimited`].
    pub fn send(&self, message: ChatMessage) -> Result<(), ChatError> {
        admit(&self.sender, &self.bucket)?.send(Outgoing::Mode { message });
        Ok(())
    }
}

/// Sends one session's queued chat, one message at a time, and publishes the
/// outcomes. [`ChatQueue::open`] returns it; the bot's actor spawns
/// [`run`](Self::run) next to the session.
#[derive(Debug)]
pub struct ChatDelivery<S> {
    bot_id: BotId,
    session: S,
    queue: mpsc::Receiver<Outgoing>,
    events: broadcast::Sender<FleetEvent>,
    clock: RuntimeClock,
    cancel: CancellationToken,
    failures: FailureLog,
}

impl<S: SessionHandle> ChatDelivery<S> {
    /// Sends queued messages until the queue is closed, then fails the ones
    /// still queued with
    /// [`ChatFailure::Disconnected`].
    pub async fn run(mut self) {
        loop {
            let next = tokio::select! {
                biased;
                // Both branches are cancel-safe: neither loses a message when
                // the other one wins. Cancellation comes first, so nothing is
                // sent once the queue is closed.
                () = self.cancel.cancelled() => break,
                next = self.queue.recv() => next,
            };
            // `None`: every sending end is gone, so nothing more can come.
            let Some(outgoing) = next else { break };
            self.deliver(outgoing).await;
        }
        self.fail_queued();
    }

    /// Sends one message and publishes or logs the outcome. A send that's
    /// already running finishes even if the queue is closed meanwhile, so its
    /// outcome is the real one. It needs no timeout of its own: every
    /// `SessionHandle` call has one.
    async fn deliver(&mut self, outgoing: Outgoing) {
        match outgoing {
            Outgoing::User { ticket, message } => {
                let kind = match self.session.send_chat(message).await {
                    Ok(()) => FleetEventKind::ChatSent { ticket },
                    Err(error) => {
                        debug!(ticket = ticket.get(), %error, "user chat wasn't sent");
                        FleetEventKind::ChatFailed {
                            ticket,
                            reason: ChatFailure::from(error),
                        }
                    }
                };
                self.publish(kind);
            }
            Outgoing::Mode { message } => {
                // One copy goes to the session, the other into the event.
                match self.session.send_chat(message.clone()).await {
                    Ok(()) => self.publish(FleetEventKind::ModeChatSent { message }),
                    // fleet-mc already warns once per session (ADR-0013).
                    Err(error @ SessionError::ChatUnavailable) => {
                        debug!(%error, "mode chat wasn't sent; skipped");
                    }
                    Err(error) => {
                        let level = self.failures.level(FailureKind::Session(error));
                        warn_or_debug!(level, %error, "mode chat wasn't sent; skipped");
                    }
                }
            }
        }
    }

    /// Closes the queue and fails what's still in it: user chat with
    /// [`ChatFailure::Disconnected`], mode chat with a log entry.
    fn fail_queued(&mut self) {
        self.queue.close();
        while let Ok(outgoing) = self.queue.try_recv() {
            match outgoing {
                Outgoing::User { ticket, .. } => self.publish(FleetEventKind::ChatFailed {
                    ticket,
                    reason: ChatFailure::Disconnected,
                }),
                Outgoing::Mode { .. } => debug!("mode chat dropped: the queue was closed"),
            }
        }
    }

    fn publish(&self, kind: FleetEventKind) {
        let event = FleetEvent {
            bot_id: self.bot_id,
            at: self.clock.now(),
            kind,
        };
        // An error only means nobody subscribes right now, so nobody misses
        // the event.
        let _ = self.events.send(event);
    }
}
