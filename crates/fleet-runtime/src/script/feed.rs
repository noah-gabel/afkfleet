//! [`ScriptFeed`] and [`ScriptInbox`]: how a session's events reach its
//! script (Plan.md P15.16, P15.33).
//!
//! A bot's actor is the one reader of its session's events. What a script
//! must hear of them, the actor puts into the session's feed; the runner
//! reads them from its inbox between hooks.
//!
//! **It's bounded whatever the server sends:**
//! - **A death, a respawn, a world change and each reason of a let-go are
//!   pending at most once,** like the port's own contract for the last two.
//!   One that comes again while it waits moves to the back, so the order of
//!   the last of each kind is kept: after "died, respawned, died" the
//!   script hears "respawned, died" and knows the bot is dead.
//! - **Chat is kept up to the feed's capacity;** a line beyond it is dropped
//!   and counted. And none is kept at all unless the runner asked for chat,
//!   which it does only while a script that declares patterns runs.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_core::chat::IncomingChat;
use fleet_core::mc::LetGo;
use tokio::sync::Notify;

/// Something of the bot's life that a script has a hook or a rule for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Life {
    /// The bot died.
    Died,
    /// The server confirmed the bot's respawn.
    Respawned,
    /// The server put the bot into another world.
    WorldChanged,
    /// The session let go of inputs by itself.
    LetGo(LetGo),
}

/// What a runner reads from its inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fed {
    /// Something of the bot's life.
    Life(Life),
    /// A chat message the bot received.
    Chat(IncomingChat),
}

#[derive(Debug, Default)]
struct State {
    life: VecDeque<Life>,
    chat: VecDeque<IncomingChat>,
    /// Whether the runner reads chat right now.
    wants_chat: bool,
    /// The chat lines dropped since the runner last asked.
    dropped: u64,
}

#[derive(Debug)]
struct Shared {
    /// How many chat lines wait at most.
    capacity: usize,
    state: Mutex<State>,
    /// Wakes the one inbox. `notify_one` keeps a permit when nobody waits
    /// yet, so no wake-up is lost between looking and waiting.
    ready: Notify,
}

impl Shared {
    /// Locks the state. Nothing here can leave it half-changed, so a
    /// poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A feed that keeps up to `chat_capacity` chat lines: the actor's side and
/// the runner's.
#[must_use]
pub fn script_feed(chat_capacity: usize) -> (ScriptFeed, ScriptInbox) {
    let shared = Arc::new(Shared {
        capacity: chat_capacity,
        state: Mutex::new(State::default()),
        ready: Notify::new(),
    });
    (
        ScriptFeed {
            shared: Arc::clone(&shared),
        },
        ScriptInbox { shared },
    )
}

/// The actor's side of a session's feed: where a bot's actor, the one
/// reader of its session's events, puts what a script must hear of them.
/// Clones feed the same inbox.
///
/// It's bounded whatever a server sends: a death, a respawn, a world
/// change and each reason of a let-go wait at most once (one that comes
/// again moves to the back), and chat is kept only while a script reads
/// it, up to the feed's capacity.
#[derive(Debug, Clone)]
pub struct ScriptFeed {
    shared: Arc<Shared>,
}

impl ScriptFeed {
    /// Tells the script about `event`. One of its kind that already waits
    /// moves to the back instead of waiting twice.
    pub fn life(&self, event: Life) {
        {
            let mut state = self.shared.lock();
            state.life.retain(|pending| *pending != event);
            state.life.push_back(event);
        }
        self.shared.ready.notify_one();
    }

    /// Hands the script a chat message the bot received. Returns whether
    /// it was kept: not while no script reads chat, and not beyond the
    /// feed's capacity, where it's counted as dropped.
    #[must_use = "a line that wasn't kept is one the script never reads"]
    pub fn chat(&self, chat: &IncomingChat) -> bool {
        {
            let mut state = self.shared.lock();
            if !state.wants_chat {
                return false;
            }
            if state.chat.len() >= self.shared.capacity {
                state.dropped = state.dropped.saturating_add(1);
                return false;
            }
            state.chat.push_back(chat.clone());
        }
        self.shared.ready.notify_one();
        true
    }
}

/// The runner's side of a session's feed: what a script's runner reads
/// between two hooks, the bot's life before chat.
#[derive(Debug)]
pub struct ScriptInbox {
    shared: Arc<Shared>,
}

impl ScriptInbox {
    /// Waits for the next thing the script must hear: the bot's life goes
    /// before chat.
    ///
    /// Cancel-safe: something leaves the feed only in the poll that
    /// returns it.
    pub(crate) async fn next(&mut self) -> Fed {
        loop {
            if let Some(fed) = self.take() {
                return fed;
            }
            self.shared.ready.notified().await;
        }
    }

    fn take(&self) -> Option<Fed> {
        let mut state = self.shared.lock();
        if let Some(event) = state.life.pop_front() {
            return Some(Fed::Life(event));
        }
        state.chat.pop_front().map(Fed::Chat)
    }

    /// Starts a script's reading in this session: whatever waits is from
    /// before it and is forgotten, and chat is kept from now on only if
    /// the script `reads_chat`.
    pub(crate) fn start(&mut self, reads_chat: bool) {
        let mut state = self.shared.lock();
        state.life.clear();
        state.chat.clear();
        state.wants_chat = reads_chat;
    }

    /// No script reads this session's feed any more: chat isn't kept.
    pub(crate) fn stop(&mut self) {
        let mut state = self.shared.lock();
        state.chat.clear();
        state.wants_chat = false;
    }

    /// How many chat lines were dropped since this was last asked.
    pub(crate) fn take_dropped(&mut self) -> u64 {
        core::mem::take(&mut self.shared.lock().dropped)
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;

    fn line(text: &str) -> IncomingChat {
        IncomingChat::system(text)
    }

    /// What waits in `inbox` right now, in order.
    fn waiting(inbox: &ScriptInbox) -> Vec<Fed> {
        core::iter::from_fn(|| inbox.take()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn the_bots_life_arrives_in_order() {
        let (feed, mut inbox) = script_feed(4);

        feed.life(Life::Died);
        feed.life(Life::Respawned);
        feed.life(Life::WorldChanged);

        assert_eq!(inbox.next().await, Fed::Life(Life::Died));
        assert_eq!(inbox.next().await, Fed::Life(Life::Respawned));
        assert_eq!(inbox.next().await, Fed::Life(Life::WorldChanged));
    }

    #[test]
    fn each_kind_waits_at_most_once_and_the_last_of_each_keeps_its_place() {
        let (feed, inbox) = script_feed(4);

        feed.life(Life::Died);
        feed.life(Life::Respawned);
        feed.life(Life::Died);

        // The bot is dead: that's what the script hears last.
        assert_eq!(
            waiting(&inbox),
            [Fed::Life(Life::Respawned), Fed::Life(Life::Died)]
        );
    }

    #[test]
    fn a_server_that_sends_a_bot_back_and_forth_cant_fill_the_feed() {
        let (feed, inbox) = script_feed(4);

        for _ in 0..10_000 {
            feed.life(Life::WorldChanged);
            feed.life(Life::Died);
            feed.life(Life::Respawned);
            for reason in LetGo::ALL {
                feed.life(Life::LetGo(reason));
            }
        }

        assert_eq!(waiting(&inbox).len(), 3 + LetGo::ALL.len());
    }

    #[test]
    fn each_reason_of_a_let_go_waits_by_itself() {
        let (feed, inbox) = script_feed(4);

        feed.life(Life::LetGo(LetGo::Leash));
        feed.life(Life::LetGo(LetGo::ToolGuard));
        feed.life(Life::LetGo(LetGo::Leash));

        assert_eq!(
            waiting(&inbox),
            [
                Fed::Life(Life::LetGo(LetGo::ToolGuard)),
                Fed::Life(Life::LetGo(LetGo::Leash))
            ]
        );
    }

    #[test]
    fn chat_is_kept_only_while_a_script_reads_it() {
        let (feed, mut inbox) = script_feed(4);

        let before = feed.chat(&line("too early"));
        inbox.start(true);
        let while_reading = feed.chat(&line("Balance: $5"));
        inbox.stop();
        let after = feed.chat(&line("too late"));

        assert!(!before && while_reading && !after);
        assert_eq!(waiting(&inbox), []);
        assert_eq!(inbox.take_dropped(), 0, "what nobody reads isn't a drop");
    }

    #[test]
    fn a_script_without_patterns_keeps_no_chat() {
        let (feed, mut inbox) = script_feed(4);
        inbox.start(false);

        assert!(!feed.chat(&line("Balance: $5")));
        assert_eq!(waiting(&inbox), []);
    }

    #[test]
    fn chat_beyond_the_capacity_is_dropped_and_counted() {
        let (feed, mut inbox) = script_feed(2);
        inbox.start(true);

        let kept: Vec<bool> = (0..5)
            .map(|n| feed.chat(&line(&format!("line {n}"))))
            .collect();

        assert_eq!(kept, [true, true, false, false, false]);
        assert_eq!(inbox.take_dropped(), 3);
        assert_eq!(inbox.take_dropped(), 0);
        assert_eq!(
            waiting(&inbox),
            [Fed::Chat(line("line 0")), Fed::Chat(line("line 1"))]
        );
    }

    #[test]
    fn the_bots_life_goes_before_chat() {
        let (feed, mut inbox) = script_feed(4);
        inbox.start(true);

        assert!(feed.chat(&line("hello")));
        feed.life(Life::Died);

        assert_eq!(
            waiting(&inbox),
            [Fed::Life(Life::Died), Fed::Chat(line("hello"))]
        );
    }

    #[test]
    fn a_script_that_starts_hears_nothing_from_before() {
        let (feed, mut inbox) = script_feed(4);
        inbox.start(true);
        feed.life(Life::Died);
        feed.life(Life::Respawned);
        assert!(feed.chat(&line("old")));

        inbox.start(true);

        assert_eq!(waiting(&inbox), []);
    }

    #[tokio::test(start_paused = true)]
    async fn the_inbox_waits_until_something_is_fed() {
        let (feed, mut inbox) = script_feed(4);

        let nothing = tokio::time::timeout(Duration::from_secs(1), inbox.next()).await;
        feed.life(Life::Died);
        let then = tokio::time::timeout(Duration::from_secs(1), inbox.next()).await;

        assert!(nothing.is_err());
        assert_eq!(then, Ok(Fed::Life(Life::Died)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_that_is_dropped_loses_nothing() {
        let (feed, mut inbox) = script_feed(4);
        // The wait is polled, finds nothing, and is dropped by the timeout.
        let _ = tokio::time::timeout(Duration::from_millis(1), inbox.next()).await;

        feed.life(Life::Respawned);
        feed.life(Life::Died);

        assert_eq!(inbox.next().await, Fed::Life(Life::Respawned));
        assert_eq!(inbox.next().await, Fed::Life(Life::Died));
    }
}
