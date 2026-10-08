//! [`ChatTicket`]: how a caller matches a queued message to its outcome.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Names one queued user chat message, so its caller can match the later
/// `ChatSent` or `ChatFailed` event to it.
///
/// Tickets are unique across the whole fleet for as long as the agent runs,
/// even across actor restarts, so the agent can map one to the control
/// plane's request id (P10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChatTicket(u64);

impl ChatTicket {
    /// Returns the ticket's number. The first ticket is 1.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Hands out [`ChatTicket`]s. Clones share one counter: the fleet makes one
/// and gives a clone to every bot's queue (P4.7).
#[derive(Debug, Clone, Default)]
pub struct ChatTickets {
    last: Arc<AtomicU64>,
}

impl ChatTickets {
    /// Starts a counter whose first ticket is 1.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the next ticket.
    pub(crate) fn next(&self) -> ChatTicket {
        // Only uniqueness matters, so `Relaxed` is enough. At the chat rate
        // limit, the counter can't come anywhere near `u64::MAX`.
        let last = self.last.fetch_add(1, Ordering::Relaxed);
        ChatTicket(last.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_count_up_from_1() {
        let tickets = ChatTickets::new();

        let numbers: Vec<_> = (0..3).map(|_| tickets.next().get()).collect();

        assert_eq!(numbers, [1, 2, 3]);
    }

    #[test]
    fn clones_share_one_counter() {
        let first = ChatTickets::new();
        let second = first.clone();

        let tickets = [first.next(), second.next(), first.next(), second.next()];

        assert_eq!(tickets.map(ChatTicket::get), [1, 2, 3, 4]);
    }

    #[test]
    fn separate_counters_are_independent() {
        let first = ChatTickets::new();
        let _ = first.next();

        assert_eq!(ChatTickets::new().next().get(), 1);
    }
}
