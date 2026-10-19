//! [`ManagedCredentials`]: a managed agent's
//! [`SessionCredentialProvider`] (Plan.md P10.6; ADR-0019, S3, S7, S62,
//! S63): each bot's session comes from the server, as a `SessionRequest`
//! over a [`SessionLink`] to the control stream.
//!
//! - **Correlation** (S3): every request gets the next number of this
//!   process, from 1, never repeated. An answer goes to the request waiting
//!   with its number and bot; one that nobody waits for any more (the actor
//!   dropped its request, or the wait's bound passed) is dropped, so a late
//!   grant never answers a later, fresh request. One that names another bot
//!   is dropped too, and the request keeps waiting.
//! - **No stream, no wait** (S62): a request the link can't send, and every
//!   request still waiting when its stream ends, fails at once, retryable:
//!   the server answers only on the stream that asked.
//! - **Expiry**: a token that expires within the connect timeout is never
//!   handed out (ADR-0013): that's a retryable error, and a `warn` that
//!   points at the clocks. The clock is the run's [`RuntimeClock`], read
//!   from tokio's, so paused time drives it.
//! - **Kept grants** (S63): the last grant handed out for each bot is kept,
//!   and handed out again only while the agent can't ask the server (the
//!   link has no stream), for a request that isn't fresh, and while it has
//!   more than the connect timeout left. A fresh request, any denial,
//!   [`forget`](ManagedCredentials::forget) and
//!   [`forget_all`](ManagedCredentials::forget_all) wipe them, and win over
//!   a grant already answered but not yet handed out: a grant is kept only
//!   if no wipe for its bot came since its request was noted.
//! - **Secrets**: a token lives only in its `SecretString`, wiped when
//!   dropped; nothing logged names a token, a name or a UUID, and the
//!   provider's `Debug` shows only how many requests wait.
//!
//! One `std` mutex guards the waiting requests, the kept grants and the
//! wipes; it's never held across an `.await` or while the link sends.

use core::fmt;
use core::num::NonZeroU64;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Mutex, MutexGuard, PoisonError};

use fleet_core::id::BotId;
use fleet_core::mc::{
    CredentialError, SessionCredentialProvider, SessionCredentials, SessionRequest,
};
use fleet_core::time;
use fleet_proto::control::{SessionDenied, SessionGrant, SessionRequestId};
use fleet_runtime::RuntimeClock;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{debug, error, warn};

/// The control stream as the provider sends on it: G's stream client
/// implements it (P10.7).
pub trait SessionLink: Send + Sync + 'static {
    /// Queues `request` on the live stream, never waiting: the stream's
    /// generation it went out on.
    ///
    /// # Errors
    /// [`Unsent`]: no welcomed stream, or its queue is full.
    fn send(&self, request: fleet_proto::control::SessionRequest)
    -> Result<LinkGeneration, Unsent>;
}

/// Which welcomed stream a request went out on: the link counts them up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkGeneration(u64);

impl LinkGeneration {
    /// Generation `generation`.
    #[must_use]
    pub const fn new(generation: u64) -> Self {
        Self(generation)
    }

    /// Its number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Why the link didn't send a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unsent {
    /// No stream is welcomed: the agent can't ask the server.
    #[error("no stream to the server")]
    NoStream,
    /// The stream's queue is full.
    #[error("the stream's queue is full")]
    QueueFull,
}

/// A session request's answer, as the stream reads it. Its `Debug` shows a
/// grant's kind and expiry, never the token, the name or the UUID, as
/// [`SessionGrant`]'s does.
#[derive(Debug)]
pub enum SessionAnswer {
    /// The bot's session.
    Grant(SessionGrant),
    /// No session.
    Denied(SessionDenied),
}

/// A passing problem: the actor backs off and asks again.
const RETRYABLE: CredentialError = CredentialError { retryable: true };

/// The managed provider; see the [module docs](self).
pub struct ManagedCredentials<L> {
    link: L,
    clock: RuntimeClock,
    connect_timeout: Duration,
    answer_bound: Duration,
    /// The last number handed out; the first is 1.
    numbers: AtomicU64,
    state: Mutex<State>,
}

/// What the mutex guards.
#[derive(Default)]
struct State {
    /// The requests waiting for their answer, by their number.
    waiting: HashMap<SessionRequestId, Waiting>,
    /// Each bot's last grant handed out (S63).
    kept: HashMap<BotId, SessionCredentials>,
    /// The wipes of each bot's kept grant, counted while a request for it
    /// is out: its grant is kept only if none came since it was noted.
    wipes: HashMap<BotId, Wipes>,
    /// The latest stream generation that ended.
    ended: Option<LinkGeneration>,
}

/// A bot's wipes while requests for it are out.
#[derive(Default)]
struct Wipes {
    /// How many came; only compared, so it may wrap.
    count: u64,
    /// How many of the bot's requests are out: the entry goes with the
    /// last one.
    out: usize,
}

impl State {
    /// Wipes `bot`'s kept grant, and the one a request for it that's out
    /// would keep.
    fn wipe(&mut self, bot: BotId) {
        self.kept.remove(&bot);
        if let Some(wipes) = self.wipes.get_mut(&bot) {
            wipes.count = wipes.count.wrapping_add(1);
        }
    }

    /// Wipes every kept grant, and every one a request that's out would
    /// keep.
    fn wipe_all(&mut self) {
        self.kept.clear();
        for wipes in self.wipes.values_mut() {
            wipes.count = wipes.count.wrapping_add(1);
        }
    }
}

/// A request waiting for its answer.
struct Waiting {
    bot: BotId,
    /// The stream it went out on, once the link said.
    generation: Option<LinkGeneration>,
    reply: oneshot::Sender<Result<SessionCredentials, CredentialError>>,
}

/// Removes its request from the waiting ones, and from its bot's requests
/// out, when dropped: the actor dropped the request, or it ended.
struct Pending<'a, L> {
    owner: &'a ManagedCredentials<L>,
    number: SessionRequestId,
    bot: BotId,
}

impl<L> Drop for Pending<'_, L> {
    fn drop(&mut self) {
        let mut state = self.owner.state();
        state.waiting.remove(&self.number);
        if let Entry::Occupied(mut entry) = state.wipes.entry(self.bot) {
            let wipes = entry.get_mut();
            wipes.out = wipes.out.saturating_sub(1);
            if wipes.out == 0 {
                entry.remove();
            }
        }
    }
}

impl<L> ManagedCredentials<L> {
    fn state(&self) -> MutexGuard<'_, State> {
        // Every section leaves the state consistent, so a poisoned lock's
        // value is still good.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The next request number, never 0 and never repeated; none once the
    /// numbers ran out.
    fn next_number(&self) -> Option<SessionRequestId> {
        let mut last = self.numbers.load(Ordering::Relaxed);
        loop {
            // Never wraps: once the numbers ran out, they stay out.
            let next = last.checked_add(1)?;
            match self.numbers.compare_exchange_weak(
                last,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return NonZeroU64::new(next).map(SessionRequestId::new),
                Err(current) => last = current,
            }
        }
    }

    /// Whether `credentials` may be handed out now: an online token only
    /// while it has more than the connect timeout left.
    fn lasts(&self, credentials: &SessionCredentials) -> bool {
        match credentials {
            SessionCredentials::Offline { .. } => true,
            SessionCredentials::Online { expires_at, .. } => {
                time::elapsed(self.clock.now(), *expires_at) > self.connect_timeout
            }
        }
    }

    /// `bot`'s kept grant, if it may be handed out now; one that may not
    /// is wiped.
    fn kept(&self, bot: BotId) -> Option<SessionCredentials> {
        let mut state = self.state();
        let kept = state.kept.get(&bot)?;
        if self.lasts(kept) {
            return Some(kept.clone());
        }
        state.kept.remove(&bot);
        None
    }
}

impl<L: SessionLink> ManagedCredentials<L> {
    /// A provider over `link`, reading `clock`, handing out no token that
    /// expires within `connect_timeout`, and waiting at most `answer_bound`
    /// for an answer (the runtime's session-request timeout).
    #[must_use]
    pub fn new(
        link: L,
        clock: RuntimeClock,
        connect_timeout: Duration,
        answer_bound: Duration,
    ) -> Self {
        Self {
            link,
            clock,
            connect_timeout,
            answer_bound,
            numbers: AtomicU64::new(0),
            state: Mutex::default(),
        }
    }

    /// Hands `answer`, as the stream read it, to the request waiting for it.
    pub fn answer(&self, answer: SessionAnswer) {
        let (number, bot, outcome) = match answer {
            SessionAnswer::Grant(grant) => (grant.id, grant.bot_id, Ok(grant.credentials)),
            SessionAnswer::Denied(denied) => (
                denied.id,
                denied.bot_id,
                Err(CredentialError {
                    retryable: denied.retryable,
                }),
            ),
        };
        let mut state = self.state();
        if outcome.is_err() {
            // The server's latest word on the bot (S63).
            state.wipe(bot);
        }
        let Some(waiting) = state.waiting.remove(&number) else {
            drop(state);
            debug!(
                bot_id = %bot,
                session_request = number.get().get(),
                "a session answer nobody waits for any more was dropped"
            );
            return;
        };
        if waiting.bot != bot {
            let expected = waiting.bot;
            state.waiting.insert(number, waiting);
            drop(state);
            warn!(
                bot_id = %expected,
                answered_bot_id = %bot,
                session_request = number.get().get(),
                "a session answer for another bot was dropped"
            );
            return;
        }
        drop(state);
        // The request was dropped meanwhile: nothing to tell.
        let _ = waiting.reply.send(outcome);
    }

    /// The stream of `generation` ended: every request it carried fails at
    /// once, retryable.
    pub fn link_ended(&self, generation: LinkGeneration) {
        let mut state = self.state();
        state.ended = state.ended.max(Some(generation));
        let before = state.waiting.len();
        // Dropping a reply fails its request at once.
        state
            .waiting
            .retain(|_, waiting| waiting.generation.is_none_or(|sent| sent > generation));
        let failed = before.saturating_sub(state.waiting.len());
        drop(state);
        // Every end, so a refusal's wipe shows before it (S63).
        debug!(
            generation = generation.get(),
            failed, "the control stream's end was noted"
        );
    }

    /// Forgets `bot`'s kept grant: the server removed the bot, or is
    /// stopping it.
    pub fn forget(&self, bot: BotId) {
        self.state().wipe(bot);
    }

    /// Forgets every kept grant: the server refused the agent.
    pub fn forget_all(&self) {
        let mut state = self.state();
        let forgotten = state.kept.len();
        state.wipe_all();
        drop(state);
        debug!(forgotten, "every kept session grant was forgotten");
    }

    /// Asks the server for `request`'s session; see the [module
    /// docs](self).
    async fn ask(&self, request: SessionRequest) -> Result<SessionCredentials, CredentialError> {
        let bot = request.bot_id;
        if request.fresh {
            // The last session was rejected: it's never handed out again.
            self.state().wipe(bot);
        }
        let Some(number) = self.next_number() else {
            error!(bot_id = %bot, "the session request numbers ran out");
            return Err(CredentialError { retryable: false });
        };
        let (reply, answer) = oneshot::channel();
        let wipes = {
            let mut state = self.state();
            state.waiting.insert(
                number,
                Waiting {
                    bot,
                    generation: None,
                    reply,
                },
            );
            let wipes = state.wipes.entry(bot).or_default();
            wipes.out = wipes.out.saturating_add(1);
            wipes.count
        };
        let pending = Pending {
            owner: self,
            number,
            bot,
        };
        let sent = self.link.send(fleet_proto::control::SessionRequest {
            id: number,
            bot_id: bot,
            fresh: request.fresh,
        });
        match sent {
            Ok(generation) => {
                let mut state = self.state();
                if state.ended.is_some_and(|ended| ended >= generation) {
                    // Its stream ended before the request was noted.
                    return Err(RETRYABLE);
                }
                if let Some(waiting) = state.waiting.get_mut(&number) {
                    waiting.generation = Some(generation);
                }
            }
            Err(unsent) => {
                drop(pending);
                debug!(
                    bot_id = %bot,
                    session_request = number.get().get(),
                    fresh = request.fresh,
                    error = %unsent,
                    "a session request couldn't be sent"
                );
                // Without a stream, the kept grant stands in (S63).
                return match unsent {
                    Unsent::NoStream if !request.fresh => self.kept(bot).ok_or(RETRYABLE),
                    Unsent::NoStream | Unsent::QueueFull => Err(RETRYABLE),
                };
            }
        }
        debug!(
            bot_id = %bot,
            session_request = number.get().get(),
            fresh = request.fresh,
            "a session request was sent"
        );
        // A reply dropped by the stream's end, and the bound, are retryable.
        let outcome = match timeout(self.answer_bound, answer).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => Err(RETRYABLE),
        };
        let credentials = outcome?;
        if !self.lasts(&credentials) {
            if let SessionCredentials::Online { expires_at, .. } = &credentials {
                warn!(
                    bot_id = %bot,
                    session_request = number.get().get(),
                    expires_at = %expires_at,
                    connect_timeout_secs = self.connect_timeout.as_secs(),
                    "a granted session expires within the connect timeout; the agent's or the server's clock may be off"
                );
            }
            return Err(RETRYABLE);
        }
        let mut state = self.state();
        // A wipe since the request was noted wins (S63): the stream's
        // reader may have handled one after this grant, before this task
        // ran again.
        if state.wipes.get(&bot).is_some_and(|now| now.count == wipes) {
            state.kept.insert(bot, credentials.clone());
        }
        drop(state);
        drop(pending);
        Ok(credentials)
    }
}

/// How many requests wait, and how many grants are kept; never a session.
impl<L> fmt::Debug for ManagedCredentials<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        f.debug_struct("ManagedCredentials")
            .field("waiting", &state.waiting.len())
            .field("kept", &state.kept.len())
            .finish_non_exhaustive()
    }
}

impl<L: SessionLink> SessionCredentialProvider for ManagedCredentials<L> {
    fn session(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<SessionCredentials, CredentialError>> + Send {
        self.ask(request)
    }
}
