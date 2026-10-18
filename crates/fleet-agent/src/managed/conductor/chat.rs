//! [`ChatRequests`]: which server request each chat ticket answers
//! (ADR-0019, S65). The conductor maps a ticket inline, right after
//! `send_chat` returns it, so the bot's `ChatSent` or `ChatFailed` can't
//! beat its mapping; the map is bounded, evicting the oldest.

use core::hash::Hash;
use std::collections::{HashMap, VecDeque};

use fleet_core::id::{BotId, ChatRequestId};

/// The tickets `T` waiting for their outcome, at most `cap`, the oldest
/// evicted first: the fleet's `ChatTicket`s.
#[derive(Debug)]
pub(crate) struct ChatRequests<T> {
    cap: usize,
    requests: HashMap<T, (ChatRequestId, BotId)>,
    order: VecDeque<T>,
}

impl<T: Copy + Eq + Hash> ChatRequests<T> {
    /// An empty map of at most `cap` tickets: `max_bots` × `chat_queue`,
    /// every message the fleet can hold.
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            requests: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Notes that `ticket` answers `request` for `bot`; past the cap, the
    /// oldest ticket is forgotten and returned.
    pub(crate) fn insert(&mut self, ticket: T, request: ChatRequestId, bot: BotId) -> Option<T> {
        if self.requests.insert(ticket, (request, bot)).is_none() {
            self.order.push_back(ticket);
        }
        if self.requests.len() <= self.cap {
            return None;
        }
        // The oldest still waiting; one taken already left only its order.
        while let Some(oldest) = self.order.pop_front() {
            if self.requests.remove(&oldest).is_some() {
                return Some(oldest);
            }
        }
        None
    }

    /// The request `ticket` answers, taken out.
    pub(crate) fn take(&mut self, ticket: T) -> Option<(ChatRequestId, BotId)> {
        let taken = self.requests.remove(&ticket);
        if taken.is_some() {
            self.order.retain(|waiting| *waiting != ticket);
        }
        taken
    }

    /// How many tickets wait.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.requests.len()
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn request(n: u8) -> ChatRequestId {
        ChatRequestId::new_v7(DateTime::UNIX_EPOCH, [n; 10]).unwrap()
    }

    fn bot(n: u8) -> BotId {
        BotId::new_v7(DateTime::UNIX_EPOCH, [n; 10]).unwrap()
    }

    /// `count` tickets, numbered from 1.
    fn tickets(count: u64) -> Vec<u64> {
        (1..=count).collect()
    }

    #[test]
    fn a_ticket_answers_its_request_once() {
        let mut map = ChatRequests::new(4);
        let [one, two] = tickets(2).try_into().unwrap();
        map.insert(one, request(1), bot(1));
        map.insert(two, request(2), bot(2));

        assert_eq!(map.take(two), Some((request(2), bot(2))));
        assert_eq!(map.take(two), None);
        assert_eq!(map.take(one), Some((request(1), bot(1))));
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn past_its_cap_the_oldest_ticket_is_forgotten() {
        let mut map = ChatRequests::new(2);
        let [one, two, three] = tickets(3).try_into().unwrap();
        map.insert(one, request(1), bot(1));
        map.insert(two, request(2), bot(1));

        let evicted = map.insert(three, request(3), bot(1));

        assert_eq!(evicted, Some(one));
        assert_eq!(map.take(one), None);
        assert_eq!(map.take(two), Some((request(2), bot(1))));
        assert_eq!(map.take(three), Some((request(3), bot(1))));
    }

    #[test]
    fn the_map_never_holds_more_than_its_cap() {
        // A sweep: any number of tickets, any take order.
        for count in 0..40 {
            let mut map = ChatRequests::new(8);
            for ticket in tickets(count) {
                map.insert(ticket, request(1), bot(1));
                assert!(map.len() <= 8, "{count}");
            }
        }
    }
}
