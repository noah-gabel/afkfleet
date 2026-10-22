//! [`EventTickets`]: the live events' tickets, held in memory (Plan.md
//! P11.5, §7.4; ADR-0020, S29).
//!
//! `POST /events/ticket` issues one to the session that asks, and
//! `GET /events` takes it, from that same session, to open one socket:
//! - **Single use.** Taking one removes it under the lock, so the first
//!   presentation burns it, whoever presents it, and two connects with one
//!   ticket never both pass. Whether the presenter is the session it was
//!   issued to is the connect's check, on the [`TicketGrant`].
//! - **[`EVENT_TICKET_TTL`] (30 s)** on the server's `Clock`. Expired
//!   tickets are dropped at every issue and take, and by the housekeeping
//!   ([`EventTickets::purge_expired`]).
//! - **At most [`TICKETS_PER_USER`] a user:** a new one evicts the user's
//!   oldest, so a client that leaks tickets only ever spends its own.
//! - **Only each ticket's SHA-256 is held,** as stored tokens are: a
//!   presented ticket is looked up by its hash, then compared in constant
//!   time, as an access token's hash is found. A restart drops every
//!   ticket; the app asks for a new one before every connect anyway.
//!
//! The lock is a std `Mutex`, never held across an `.await`, and `Debug`
//! shows only how many tickets are held.

use core::fmt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::auth::{EVENT_TICKET_TTL, challenge_expired, expires_in};
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::Clock;
use secrecy::SecretString;

use crate::ports::tokens::{MintTokenError, TokenHash, TokenKind, Tokens};

/// The most unspent tickets one user holds at once.
pub const TICKETS_PER_USER: usize = 3;

/// A ticket just issued, for its response.
#[derive(Debug)]
pub struct IssuedTicket {
    /// The ticket's text, for the client only.
    pub ticket: SecretString,
    /// Its seconds left: [`EVENT_TICKET_TTL`]'s.
    pub expires_in: u64,
}

/// Whose a taken ticket was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TicketGrant {
    /// The user it was issued to.
    pub user: UserId,
    /// The session that asked for it.
    pub session: SessionId,
}

/// Why a presented ticket opens nothing. The connect answers every one
/// alike; the reason is only for its `debug` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TicketRefusal {
    /// Not shaped like a ticket: another kind of token, or anything else.
    #[error("not shaped like a ticket")]
    Malformed,
    /// No such ticket held: never issued, used, evicted, or dropped.
    #[error("no such ticket")]
    Unknown,
    /// Held, but older than [`EVENT_TICKET_TTL`]; burned all the same.
    #[error("the ticket expired")]
    Expired,
}

/// One held ticket.
struct Ticket {
    hash: TokenHash,
    user: UserId,
    session: SessionId,
    issued_at: DateTime<Utc>,
}

/// The tickets; clones share them. See the [module docs](self).
#[derive(Clone)]
pub struct EventTickets {
    clock: Arc<dyn Clock>,
    tokens: Arc<dyn Tokens>,
    held: Arc<Mutex<HashMap<[u8; TokenHash::LEN], Ticket>>>,
}

/// Counts only: no ticket and no hash.
impl fmt::Debug for EventTickets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventTickets")
            .field("held", &self.held())
            .finish_non_exhaustive()
    }
}

impl EventTickets {
    /// No tickets yet; minted by `tokens`, timed on `clock` (the wall clock,
    /// like the sessions').
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, tokens: Arc<dyn Tokens>) -> Self {
        Self {
            clock,
            tokens,
            held: Arc::default(),
        }
    }

    /// Locks the tickets. A panic while holding them leaves them consistent,
    /// so a poisoned lock is used as it is.
    fn tickets(&self) -> MutexGuard<'_, HashMap<[u8; TokenHash::LEN], Ticket>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Issues a ticket to `user`'s `session`, evicting their oldest when
    /// they hold [`TICKETS_PER_USER`] already.
    ///
    /// # Errors
    /// [`MintTokenError`] when the ticket can't be minted; nothing is held
    /// then.
    pub fn issue(&self, user: UserId, session: SessionId) -> Result<IssuedTicket, MintTokenError> {
        // Minted outside the lock: it reads the random source.
        let minted = self.tokens.mint(TokenKind::EventTicket)?;
        let now = self.clock.now();
        let mut tickets = self.tickets();
        drop_expired(&mut tickets, now);
        let mut theirs: Vec<_> = tickets
            .iter()
            .filter(|(_, ticket)| ticket.user == user)
            .map(|(key, ticket)| (ticket.issued_at, *key))
            .collect();
        if theirs.len() >= TICKETS_PER_USER {
            theirs.sort_unstable();
            let evicted = theirs.len() + 1 - TICKETS_PER_USER;
            for (_, key) in theirs.iter().take(evicted) {
                tickets.remove(key);
            }
        }
        tickets.insert(
            *minted.hash.as_bytes(),
            Ticket {
                hash: minted.hash,
                user,
                session,
                issued_at: now,
            },
        );
        drop(tickets);
        Ok(IssuedTicket {
            ticket: minted.token,
            expires_in: expires_in(now, EVENT_TICKET_TTL, now),
        })
    }

    /// Takes the ticket `presented` is: it's gone afterwards, whatever the
    /// answer.
    ///
    /// # Errors
    /// [`TicketRefusal`]: not shaped like a ticket, not held, or expired.
    pub fn take(&self, presented: &str) -> Result<TicketGrant, TicketRefusal> {
        let hash = self
            .tokens
            .hash_presented(TokenKind::EventTicket, presented)
            .ok_or(TicketRefusal::Malformed)?;
        let now = self.clock.now();
        let mut tickets = self.tickets();
        // Removed whatever comes of it: the first presentation burns it.
        let taken = tickets.remove(hash.as_bytes());
        drop_expired(&mut tickets, now);
        drop(tickets);
        // Found by its hash's bytes, then compared in constant time, as an
        // access token's hash is found.
        let ticket = taken
            .filter(|ticket| ticket.hash.ct_eq(&hash))
            .ok_or(TicketRefusal::Unknown)?;
        if challenge_expired(ticket.issued_at, EVENT_TICKET_TTL, now) {
            return Err(TicketRefusal::Expired);
        }
        Ok(TicketGrant {
            user: ticket.user,
            session: ticket.session,
        })
    }

    /// Drops every expired ticket; returns how many.
    #[must_use]
    pub fn purge_expired(&self) -> usize {
        let now = self.clock.now();
        drop_expired(&mut self.tickets(), now)
    }

    /// How many tickets are held.
    #[must_use]
    pub fn held(&self) -> usize {
        self.tickets().len()
    }
}

/// Drops the tickets expired at `now`; returns how many.
fn drop_expired(tickets: &mut HashMap<[u8; TokenHash::LEN], Ticket>, now: DateTime<Utc>) -> usize {
    let before = tickets.len();
    tickets.retain(|_, ticket| !challenge_expired(ticket.issued_at, EVENT_TICKET_TTL, now));
    before - tickets.len()
}
