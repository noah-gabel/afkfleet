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
//!
//! # Rights when an event is sent (S30)
//! [`LiveRights`] keeps one snapshot of who may see which bot, [`Rights`],
//! tagged with the commit count it was read at, for every socket to share. A
//! socket whose count moved past its snapshot's asks for one at least that
//! new before it sends anything about a bot; one rebuild runs at a time, so
//! sockets that ask together share it. The count is read before the
//! snapshot's read transaction begins, so a commit counted after it moves
//! the count past the snapshot, and the next ask rebuilds again.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::authz::{
    Actor, GrantLevel, Permission, ResourceContext, UserRef, authorize, effective_level,
};
use fleet_core::bot::{BotNotification, BotSnapshot};
use fleet_core::chat::ChatFormats;
use fleet_core::id::{AccountId, AgentId, BotId, ChatRequestId, UserId};
use fleet_proto::control::ChatFailedReason;
use tokio::sync::{broadcast, watch};

use crate::app::server_settings;
use crate::ports::bots::{Access, BotAccess, UserAccess};
use crate::ports::chat::ListedChat;
use crate::ports::live_hints::{LiveHints, ModeEvent};
use crate::ports::server_settings::UnreadableFormats;
use crate::ports::store::{Store, StoreError};

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

/// Who may see which bot, as one read found it, and the commit count it was
/// read at (S30). Its rule is `GET /bots/{id}`'s: `authorize(ViewBot)` on the
/// bot's account, by its owner's role and the viewer's grant, both as read.
/// It also holds each bot's chat formats, which its live lines are read
/// with, as the history reads them (S79, R146); not the address they were
/// found by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rights {
    at: u64,
    bots: HashMap<BotId, BotAccess>,
    grants: HashMap<(AccountId, UserId), GrantLevel>,
    users: HashMap<UserId, UserAccess>,
    formats: HashMap<BotId, Arc<ChatFormats>>,
}

impl Rights {
    /// `access`, read at the commit count `at`.
    #[must_use]
    pub fn new(at: u64, access: Access) -> Self {
        Self {
            at,
            bots: access.bots.into_iter().map(|bot| (bot.bot, bot)).collect(),
            grants: access
                .grants
                .into_iter()
                .map(|grant| ((grant.account, grant.user), grant.level))
                .collect(),
            users: access
                .users
                .into_iter()
                .map(|user| (user.user, user))
                .collect(),
            formats: access
                .bot_formats
                .into_iter()
                .map(|bot| (bot.bot, bot.formats))
                .collect(),
        }
    }

    /// The commit count it was read at.
    #[must_use]
    pub const fn at(&self) -> u64 {
        self.at
    }

    /// The level `viewer` holds on `bot`, `None` when they may not see it or
    /// it's gone.
    #[must_use]
    pub fn level(&self, viewer: &Actor, bot: BotId) -> Option<GrantLevel> {
        let bot = self.bots.get(&bot)?;
        // One read holds every bot's owner; a snapshot without it judges the
        // bot seen by no one.
        let owner = self.users.get(&bot.owner)?;
        let owner = UserRef {
            id: owner.user,
            role: owner.role,
        };
        let grant = self.grants.get(&(bot.account, viewer.id)).copied();
        authorize(
            viewer,
            Permission::ViewBot,
            &ResourceContext::Account { owner, grant },
        )
        .ok()?;
        effective_level(viewer, owner, grant)
    }

    /// The chat formats `bot`'s lines are read with: its current server's,
    /// `None` when it has none.
    #[must_use]
    pub fn formats(&self, bot: BotId) -> Option<&ChatFormats> {
        self.formats.get(&bot).map(AsRef::as_ref)
    }

    /// Every bot `viewer` may see.
    #[must_use]
    pub fn visible(&self, viewer: &Actor) -> BTreeSet<BotId> {
        self.bots
            .keys()
            .copied()
            .filter(|bot| self.level(viewer, *bot).is_some())
            .collect()
    }

    /// Whether `viewer` still is who their socket opened as: there, with the
    /// same role, and enabled (R31).
    #[must_use]
    pub fn stands(&self, viewer: &Actor) -> bool {
        self.users
            .get(&viewer.id)
            .is_some_and(|user| user.enabled && user.role == viewer.role)
    }
}

/// The latest [`Rights`], shared by every socket, read again when a socket
/// needs one newer than it (S30; the [module docs](self)). Clones share it.
#[derive(Debug, Clone)]
pub struct LiveRights {
    inner: Arc<RightsCache>,
}

#[derive(Debug)]
struct RightsCache {
    store: Arc<dyn Store>,
    commits: watch::Receiver<u64>,
    /// The latest snapshot; never held across an await.
    latest: Mutex<Option<Arc<Rights>>>,
    /// One rebuild at a time, and the settings rows whose formats the last
    /// rebuild found unreadable, by rowid: a row is logged when it turns
    /// unreadable, not at every rebuild (S81).
    turn: tokio::sync::Mutex<BTreeSet<i64>>,
}

impl LiveRights {
    /// Rights over `store`'s bots, following its commit count.
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        let commits = store.commits();
        Self {
            inner: Arc::new(RightsCache {
                store,
                commits,
                latest: Mutex::new(None),
                turn: tokio::sync::Mutex::new(BTreeSet::new()),
            }),
        }
    }

    /// The commit count now, read without marking anyone's receiver.
    #[must_use]
    pub fn count(&self) -> u64 {
        *self.inner.commits.borrow()
    }

    /// A snapshot read at a commit count of at least `count`: the latest
    /// when it's that new, otherwise one read now. One read runs at a time;
    /// whoever waited for it takes it when it's new enough.
    ///
    /// # Errors
    /// The read's [`StoreError`].
    pub async fn at_least(&self, count: u64) -> Result<Arc<Rights>, StoreError> {
        if let Some(latest) = self.fresh(count) {
            return Ok(latest);
        }
        let mut logged = self.inner.turn.lock().await;
        if let Some(latest) = self.fresh(count) {
            return Ok(latest);
        }
        // Before the read: a commit counted after this lands after the
        // read's snapshot, or moves the count past it.
        let at = self.count();
        let access = self.inner.store.bots().access().await?;
        log_turned_unreadable(&mut logged, &access.unreadable_formats);
        let rights = Arc::new(Rights::new(at, access));
        *self.latest() = Some(Arc::clone(&rights));
        Ok(rights)
    }

    /// The latest snapshot, when it was read at `count` or later.
    fn fresh(&self, count: u64) -> Option<Arc<Rights>> {
        self.latest()
            .as_ref()
            .filter(|latest| latest.at >= count)
            .map(Arc::clone)
    }

    fn latest(&self) -> std::sync::MutexGuard<'_, Option<Arc<Rights>>> {
        self.inner
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Logs each row of `now` that `logged`, the last rebuild's, doesn't hold,
/// then keeps `now`'s: a row is logged when it turns unreadable, never at each
/// rebuild, since every commit moves the count (S81). The read bounds `now` at
/// the table's most rows.
fn log_turned_unreadable(logged: &mut BTreeSet<i64>, now: &[UnreadableFormats]) {
    for row in now.iter().filter(|row| !logged.contains(&row.rowid)) {
        server_settings::unreadable_formats(row);
    }
    *logged = now.iter().map(|row| row.rowid).collect();
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

    // --- Rights (S30) ---------------------------------------------------

    use fleet_core::authz::Role;

    use crate::ports::bots::GrantAccess;

    fn user(n: u8, role: Role) -> UserAccess {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        UserAccess {
            user: UserId::new_v7(at, [n; 10]).unwrap(),
            role,
            enabled: true,
        }
    }

    fn bot_of(n: u8, owner: &UserAccess) -> BotAccess {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        BotAccess {
            bot: BotId::new_v7(at, [n; 10]).unwrap(),
            account: AccountId::new_v7(at, [n; 10]).unwrap(),
            owner: owner.user,
        }
    }

    fn actor(user: &UserAccess) -> Actor {
        Actor {
            id: user.user,
            role: user.role,
        }
    }

    /// The Owner, two Admins and two Members, each with a bot; the second
    /// Admin's bot shared with the first Member at Control, the Owner's with
    /// the first Admin at View.
    struct People {
        owner: UserAccess,
        admin: UserAccess,
        other_admin: UserAccess,
        member: UserAccess,
        other_member: UserAccess,
        bots: [BotAccess; 5],
    }

    fn people() -> (People, Rights) {
        let owner = user(1, Role::Owner);
        let admin = user(2, Role::Admin);
        let other_admin = user(3, Role::Admin);
        let member = user(4, Role::Member);
        let other_member = user(5, Role::Member);
        let bots = [
            bot_of(11, &owner),
            bot_of(12, &admin),
            bot_of(13, &other_admin),
            bot_of(14, &member),
            bot_of(15, &other_member),
        ];
        let access = Access {
            bots: bots.to_vec(),
            grants: vec![
                GrantAccess {
                    account: bots[2].account,
                    user: member.user,
                    level: GrantLevel::Control,
                },
                GrantAccess {
                    account: bots[0].account,
                    user: admin.user,
                    level: GrantLevel::View,
                },
            ],
            users: vec![owner, admin, other_admin, member, other_member],
            ..Access::default()
        };
        let people = People {
            owner,
            admin,
            other_admin,
            member,
            other_member,
            bots,
        };
        (people, Rights::new(7, access))
    }

    #[test]
    fn each_viewer_holds_on_each_bot_what_get_bot_would_let_them() {
        use GrantLevel::{Control, Manage, View};
        let (p, rights) = people();
        // Rows: the Owner, the Admin, the other Admin, the Member, the other
        // Member; columns: the bots of the same five.
        let expected = [
            (&p.owner, [Some(Manage); 5]),
            (
                &p.admin,
                [Some(View), Some(Manage), None, Some(Manage), Some(Manage)],
            ),
            (
                &p.other_admin,
                [None, None, Some(Manage), Some(Manage), Some(Manage)],
            ),
            (&p.member, [None, None, Some(Control), Some(Manage), None]),
            (&p.other_member, [None, None, None, None, Some(Manage)]),
        ];

        for (viewer, levels) in expected {
            let held: Vec<Option<GrantLevel>> = p
                .bots
                .iter()
                .map(|bot| rights.level(&actor(viewer), bot.bot))
                .collect();
            assert_eq!(held, levels, "{:?}", viewer.role);
        }
    }

    #[test]
    fn the_visible_bots_are_those_with_a_level() {
        let (p, rights) = people();

        let seen = rights.visible(&actor(&p.member));

        assert_eq!(seen, BTreeSet::from([p.bots[2].bot, p.bots[3].bot]));
        assert_eq!(rights.visible(&actor(&p.owner)).len(), 5);
        assert_eq!(rights.at(), 7);
    }

    #[test]
    fn a_bot_the_snapshot_doesnt_hold_or_whose_owner_it_lacks_is_seen_by_no_one() {
        let (p, rights) = people();
        let stranger = user(9, Role::Member);
        let orphan = bot_of(19, &stranger);
        let mut access = Access {
            bots: vec![orphan],
            ..Access::default()
        };
        access.users.push(p.owner);
        let without_owner = Rights::new(1, access);

        assert_eq!(rights.level(&actor(&p.owner), orphan.bot), None);
        assert_eq!(without_owner.level(&actor(&p.owner), orphan.bot), None);
    }

    #[test]
    fn the_log_once_set_keeps_every_unreadable_row_it_was_given() {
        let rows: Vec<UnreadableFormats> = (0..101)
            .map(|rowid| UnreadableFormats {
                rowid,
                id: None,
                server: None,
                column: "chat_formats",
            })
            .collect();
        let mut logged = BTreeSet::new();

        log_turned_unreadable(&mut logged, &rows);

        assert_eq!(logged.len(), rows.len());
    }

    #[test]
    fn each_bot_is_read_with_its_own_formats_and_one_without_with_none() {
        use crate::ports::bots::BotFormats;
        let (p, _) = people();
        let formats = Arc::new(ChatFormats::try_new(&["{sender} : "]).unwrap());
        let rights = Rights::new(
            1,
            Access {
                bots: p.bots.to_vec(),
                bot_formats: vec![BotFormats {
                    bot: p.bots[0].bot,
                    formats: Arc::clone(&formats),
                }],
                ..Access::default()
            },
        );

        assert_eq!(rights.formats(p.bots[0].bot), Some(formats.as_ref()));
        assert_eq!(rights.formats(p.bots[1].bot), None);
    }

    #[test]
    fn a_viewer_stands_only_as_they_opened_enabled() {
        let (p, rights) = people();
        let promoted = Actor {
            id: p.member.user,
            role: Role::Admin,
        };
        let mut disabled = p.other_member;
        disabled.enabled = false;
        let without = Rights::new(
            1,
            Access {
                users: vec![disabled],
                ..Access::default()
            },
        );

        assert!(rights.stands(&actor(&p.member)));
        assert!(!rights.stands(&promoted));
        assert!(!without.stands(&actor(&p.other_member)));
        assert!(!without.stands(&actor(&p.owner)));
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
