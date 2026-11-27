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
//! - **Screens, clicks and sign editors are bounded as fleet-mc's bridge
//!   bounds them** (ADR-0051, C2): a screen that opens drops what's unread
//!   of the screens before it, all but the close of one whose opening was
//!   read; a screen that closes before its opening was read takes the
//!   opening and its update with it and is never heard of; an update waits
//!   once a screen; a sign editor replaces one unread. A click's fate is
//!   always kept: no action goes out while one is unread. So at most an
//!   opening, an update, a close, a fate and a sign editor wait at once.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_core::chat::IncomingChat;
use fleet_core::mc::LetGo;
use fleet_core::script::menu::{ClickResult, ScreenId, SignEditor};
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
    /// A screen opened: the server's or the bot's own inventory. Every key
    /// and button was let go (ADR-0036, M8).
    MenuOpened(ScreenId),
    /// The open screen's content changed.
    MenuUpdated(ScreenId),
    /// The screen closed.
    MenuClosed(ScreenId),
    /// An action's fate (ADR-0036, R1).
    Clicked(ClickResult),
    /// The server opened a sign editor (ADR-0036, M16).
    SignOpened(SignEditor),
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
    /// moves to the back instead of waiting twice; a screen's events are
    /// bounded as the module says.
    pub fn life(&self, event: Life) {
        {
            let mut state = self.shared.lock();
            push_life(&mut state.life, event);
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

/// Puts `event` behind what waits in `life`, by the module's rules.
fn push_life(life: &mut VecDeque<Life>, event: Life) {
    match event {
        Life::MenuOpened(_) => {
            // What's unread of the screens before goes, but the close of
            // one whose opening was read: the script must hear it closed.
            let unread: Vec<ScreenId> = life
                .iter()
                .filter_map(|pending| match pending {
                    Life::MenuOpened(screen) => Some(*screen),
                    _ => None,
                })
                .collect();
            life.retain(|pending| match pending {
                Life::MenuOpened(_) | Life::MenuUpdated(_) | Life::SignOpened(_) => false,
                Life::MenuClosed(screen) => !unread.contains(screen),
                _ => true,
            });
        }
        Life::MenuClosed(closed) => {
            let opening_unread = life.contains(&Life::MenuOpened(closed));
            life.retain(|pending| {
                !matches!(pending, Life::MenuOpened(screen) | Life::MenuUpdated(screen) if *screen == closed)
            });
            if opening_unread {
                // Never heard of: it opened and closed unread.
                return;
            }
        }
        Life::SignOpened(_) => life.retain(|pending| !matches!(pending, Life::SignOpened(_))),
        _ => life.retain(|pending| *pending != event),
    }
    life.push_back(event);
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

    fn screens() -> [ScreenId; 3] {
        use fleet_core::script::menu::{ClickGate, WindowId};
        let mut gate = ClickGate::new();
        [1, 2, 3].map(|window| gate.opened(WindowId::new(window)))
    }

    fn lives(inbox: &ScriptInbox) -> Vec<Life> {
        waiting(inbox)
            .into_iter()
            .filter_map(|fed| match fed {
                Fed::Life(life) => Some(life),
                Fed::Chat(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_screen_its_update_its_close_and_a_fate_arrive_in_order() {
        use fleet_core::script::menu::{ClickFate, ClickResult};
        let (feed, inbox) = script_feed(4);
        let [chest, ..] = screens();
        let fate = ClickResult {
            screen: chest,
            slot: Some(3),
            fate: ClickFate::Ok,
        };
        feed.life(Life::MenuOpened(chest));
        assert_eq!(lives(&inbox), [Life::MenuOpened(chest)]);

        feed.life(Life::MenuUpdated(chest));
        feed.life(Life::MenuUpdated(chest));
        feed.life(Life::Clicked(fate));
        feed.life(Life::MenuUpdated(chest));
        let before_the_close = lives(&inbox);
        feed.life(Life::MenuUpdated(chest));
        feed.life(Life::MenuClosed(chest));

        // An update waits once, at the back; a close takes its screen's
        // update along, which has nothing left to update.
        assert_eq!(
            before_the_close,
            [Life::Clicked(fate), Life::MenuUpdated(chest)]
        );
        assert_eq!(lives(&inbox), [Life::MenuClosed(chest)]);
    }

    #[test]
    fn a_screen_that_opens_drops_what_is_unread_of_the_one_before() {
        let (feed, inbox) = script_feed(4);
        let [first, second, _] = screens();

        feed.life(Life::MenuOpened(first));
        feed.life(Life::MenuUpdated(first));
        feed.life(Life::MenuOpened(second));

        assert_eq!(lives(&inbox), [Life::MenuOpened(second)]);
    }

    #[test]
    fn a_screen_whose_opening_was_read_keeps_its_close_when_another_opens() {
        let (feed, mut inbox) = script_feed(4);
        let [first, second, _] = screens();
        feed.life(Life::MenuOpened(first));
        assert_eq!(inbox.take(), Some(Fed::Life(Life::MenuOpened(first))));

        feed.life(Life::MenuClosed(first));
        feed.life(Life::MenuOpened(second));

        assert_eq!(
            lives(&inbox),
            [Life::MenuClosed(first), Life::MenuOpened(second)]
        );
        let _ = &mut inbox;
    }

    #[test]
    fn a_screen_that_closes_before_its_opening_was_read_is_never_heard_of() {
        let (feed, inbox) = script_feed(4);
        let [first, ..] = screens();

        feed.life(Life::MenuOpened(first));
        feed.life(Life::MenuUpdated(first));
        feed.life(Life::MenuClosed(first));

        assert_eq!(lives(&inbox), []);
    }

    #[test]
    fn a_sign_editor_replaces_one_unread() {
        use fleet_core::script::BlockPosition;
        use fleet_core::script::menu::{SignDesk, SignSide};
        let (feed, inbox) = script_feed(4);
        let mut desk = SignDesk::new();
        let first = desk.opened(BlockPosition { x: 0, y: 0, z: 0 }, SignSide::Front);
        let second = desk.opened(BlockPosition { x: 1, y: 0, z: 0 }, SignSide::Front);

        feed.life(Life::SignOpened(first));
        feed.life(Life::SignOpened(second));

        assert_eq!(lives(&inbox), [Life::SignOpened(second)]);
    }

    #[test]
    fn a_server_that_opens_screens_in_a_loop_cant_fill_the_feed() {
        use fleet_core::script::menu::{ClickGate, WindowId};
        let (feed, inbox) = script_feed(4);
        let mut gate = ClickGate::new();

        for n in 0..10_000 {
            let screen = gate.opened(WindowId::new(n % 100 + 1));
            feed.life(Life::MenuOpened(screen));
            feed.life(Life::MenuUpdated(screen));
            if n % 3 == 0 {
                feed.life(Life::MenuClosed(screen));
            }
        }

        assert!(lives(&inbox).len() <= 3);
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
