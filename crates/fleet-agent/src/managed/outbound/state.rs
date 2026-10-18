//! The outbox's state (ADR-0019, S66): what waits to go to the server, by
//! class, and the stream's phase, which decides what may go now. Plain
//! data, no IO and no clock: [`super::Outbox`] holds it under its mutex and
//! the request stream reads it.

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::id::{BotId, ChatRequestId};
use fleet_proto::control::{
    AgentMessage, BotEvent, BotEventKind, ChatFailedReason, Hello, SessionRequest,
};

/// How much each class may hold (S66). The honest maximum fits under each
/// never-drop cap twice over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    /// Session requests waiting: one per bot, `max_bots`.
    pub(crate) lane: usize,
    /// Bots with a status, a death or a removal waiting: 2 × `max_bots`.
    pub(crate) per_bot: usize,
    /// Alerts and chat answers waiting: 2 × `max_bots` × (`chat_queue` + 2).
    pub(crate) notices: usize,
    /// Chat waiting per bot.
    pub(crate) chat_per_bot: usize,
    /// Chat waiting in all.
    pub(crate) chat: usize,
}

/// Chat waiting per bot, at most.
pub(crate) const CHAT_PER_BOT: usize = 64;

/// Chat waiting in all, at most.
pub(crate) const CHAT_TOTAL: usize = 1024;

impl Caps {
    /// The caps of a fleet of `max_bots` bots whose chat queues hold
    /// `chat_queue` messages.
    pub(crate) const fn of(max_bots: usize, chat_queue: usize) -> Self {
        Self {
            lane: max_bots,
            per_bot: max_bots.saturating_mul(2),
            notices: max_bots
                .saturating_mul(2)
                .saturating_mul(chat_queue.saturating_add(2)),
            chat_per_bot: CHAT_PER_BOT,
            chat: CHAT_TOTAL,
        }
    }
}

/// A notice: never merged, sent in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Notice {
    /// The bot paused or failed and needs a human.
    Alert(BotNotification),
    /// A user's chat message was sent.
    ChatSent(ChatRequestId),
    /// A user's chat message wasn't.
    ChatFailed(ChatRequestId, ChatFailedReason),
}

/// Chat a bot received or its mode sent: the oldest is dropped first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Chat {
    /// Received from the server.
    Received(IncomingChat),
    /// Sent by the bot's mode.
    ModeSent(ChatMessage),
}

/// What happened to an item pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pushed {
    /// It waits.
    Queued,
    /// It waits, and the oldest chat of its bot or of all was dropped for it.
    DroppedOldest,
    /// A never-drop class is full: it was refused, and the stream must end
    /// so the next hello resyncs.
    Refused,
}

/// Why a session request wasn't queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneRefusal {
    /// No welcomed stream.
    NoStream,
    /// The lane is full.
    Full,
}

/// Where the stream is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// No stream: between attempts.
    Closed,
    /// Opened with its hello, which waits until sent; nothing more goes
    /// until `Welcome`.
    Opened {
        generation: u64,
        hello: Option<Box<Hello>>,
    },
    /// Welcomed, not reconciled: statuses and heartbeats only.
    Welcomed { generation: u64, interval: Duration },
    /// Reconciled: everything, in [`State::next`]'s order.
    Reconciled { generation: u64, interval: Duration },
}

/// What the request stream of `generation` does next.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Next {
    /// Send this.
    Send(AgentMessage),
    /// Nothing now: wait, with a heartbeat after `interval` of nothing.
    Wait {
        /// The heartbeat's interval, once welcomed.
        heartbeat: Option<Duration>,
    },
    /// The stream ends: it's no longer the outbox's stream, or the outbox
    /// overflowed.
    End,
}

/// What the outbox counts, for its `Debug` and its log lines; never chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Chat dropped for newer chat.
    pub(crate) chat_dropped: u64,
    /// Items of a never-drop class refused because it was full.
    pub(crate) refused: u64,
    /// Session requests replaced, or dropped at a stream's end, unsent.
    pub(crate) requests_dropped: u64,
}

/// The outbox's state; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct State {
    caps: Caps,
    /// The last generation handed out; the first stream is 1.
    generation: u64,
    phase: Phase,
    /// A never-drop class overflowed during this stream.
    overflowed: bool,
    /// One unsent session request per bot, in order.
    lane: VecDeque<SessionRequest>,
    /// Each bot's latest status.
    statuses: BTreeMap<BotId, BotSnapshot>,
    /// Each bot's latest death.
    died: BTreeMap<BotId, DateTime<Utc>>,
    /// Each bot's removal.
    removed: BTreeMap<BotId, DateTime<Utc>>,
    /// Alerts and answers, in order.
    notices: VecDeque<(BotId, DateTime<Utc>, Notice)>,
    /// Chat, in order.
    chat: VecDeque<(BotId, DateTime<Utc>, Chat)>,
    /// How much chat each bot has waiting.
    chat_of: HashMap<BotId, usize>,
    counts: Counts,
}

impl State {
    /// An empty outbox with `caps`, no stream.
    pub(crate) fn new(caps: Caps) -> Self {
        Self {
            caps,
            generation: 0,
            phase: Phase::Closed,
            overflowed: false,
            lane: VecDeque::new(),
            statuses: BTreeMap::new(),
            died: BTreeMap::new(),
            removed: BTreeMap::new(),
            notices: VecDeque::new(),
            chat: VecDeque::new(),
            chat_of: HashMap::new(),
            counts: Counts::default(),
        }
    }

    /// What it counted.
    pub(crate) const fn counts(&self) -> Counts {
        self.counts
    }

    /// How many items wait, by class: lane, per-bot, notices, chat.
    pub(crate) fn waiting(&self) -> [usize; 4] {
        [
            self.lane.len(),
            self.per_bot_bots(),
            self.notices.len(),
            self.chat.len(),
        ]
    }

    /// Whether a never-drop class overflowed during the current stream.
    pub(crate) const fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// The generation of the current stream, if it's welcomed.
    pub(crate) const fn live(&self) -> Option<u64> {
        match self.phase {
            Phase::Welcomed { generation, .. } | Phase::Reconciled { generation, .. } => {
                Some(generation)
            }
            Phase::Closed | Phase::Opened { .. } => None,
        }
    }

    /// How many bots have a status, a death or a removal waiting.
    fn per_bot_bots(&self) -> usize {
        let bots: BTreeSet<&BotId> = self
            .statuses
            .keys()
            .chain(self.died.keys())
            .chain(self.removed.keys())
            .collect();
        bots.len()
    }

    /// Whether `bot` is new to the per-bot class and the class is full.
    fn per_bot_full(&self, bot: BotId) -> bool {
        let known = self.statuses.contains_key(&bot)
            || self.died.contains_key(&bot)
            || self.removed.contains_key(&bot);
        !known && self.per_bot_bots() >= self.caps.per_bot
    }

    /// Before the hello's snapshot: the statuses waiting are dropped, since
    /// the hello carries the truth, and one published after the snapshot
    /// waits.
    pub(crate) fn prepare(&mut self) {
        self.statuses.clear();
    }

    /// Opens a stream with `hello`: its generation. A removal of a bot the
    /// hello doesn't list is dropped, since that hello frees the bot (S32).
    pub(crate) fn open(&mut self, hello: Hello) -> u64 {
        let listed: BTreeSet<BotId> = hello.running.iter().map(|bot| bot.bot_id).collect();
        self.removed.retain(|bot, _| listed.contains(bot));
        self.generation = self.generation.saturating_add(1);
        self.overflowed = false;
        self.phase = Phase::Opened {
            generation: self.generation,
            hello: Some(Box::new(hello)),
        };
        self.generation
    }

    /// The stream of `generation` was welcomed: heartbeats every `interval`
    /// of nothing sent, statuses until the reconcile.
    pub(crate) fn welcomed(&mut self, generation: u64, interval: Duration) {
        if matches!(self.phase, Phase::Opened { generation: current, .. } if current == generation)
        {
            self.phase = Phase::Welcomed {
                generation,
                interval,
            };
        }
    }

    /// The stream of `generation` took its full reconcile, which gives the
    /// agent `given`: everything may go now, and what waits for a bot it
    /// doesn't give, but its removal, is dropped.
    pub(crate) fn reconciled(&mut self, generation: u64, given: &BTreeSet<BotId>) {
        let Phase::Welcomed {
            generation: current,
            interval,
        } = self.phase
        else {
            return;
        };
        if current != generation {
            return;
        }
        self.phase = Phase::Reconciled {
            generation,
            interval,
        };
        self.statuses.retain(|bot, _| given.contains(bot));
        self.died.retain(|bot, _| given.contains(bot));
        self.notices.retain(|(bot, _, _)| given.contains(bot));
        self.chat.retain(|(bot, _, _)| given.contains(bot));
        self.chat_of.retain(|bot, _| given.contains(bot));
    }

    /// The stream of `generation` ended: the lane closes and its unsent
    /// requests are dropped; everything else waits for the next one.
    pub(crate) fn ended(&mut self, generation: u64) {
        let current = match self.phase {
            Phase::Closed => return,
            Phase::Opened { generation, .. }
            | Phase::Welcomed { generation, .. }
            | Phase::Reconciled { generation, .. } => generation,
        };
        if current != generation {
            return;
        }
        self.phase = Phase::Closed;
        let dropped = u64::try_from(self.lane.len()).unwrap_or(u64::MAX);
        self.counts.requests_dropped = self.counts.requests_dropped.saturating_add(dropped);
        self.lane.clear();
    }

    /// Ends whichever stream is current, as [`State::ended`] does: its
    /// generation, if one was open.
    pub(crate) fn close(&mut self) -> Option<u64> {
        let generation = match self.phase {
            Phase::Closed => return None,
            Phase::Opened { generation, .. }
            | Phase::Welcomed { generation, .. }
            | Phase::Reconciled { generation, .. } => generation,
        };
        self.ended(generation);
        Some(generation)
    }

    /// Queues `request` for the welcomed stream: its generation. A newer
    /// request for a bot replaces its unsent one.
    pub(crate) fn request(&mut self, request: SessionRequest) -> Result<u64, LaneRefusal> {
        let generation = self.live().ok_or(LaneRefusal::NoStream)?;
        if let Some(unsent) = self
            .lane
            .iter_mut()
            .find(|unsent| unsent.bot_id == request.bot_id)
        {
            *unsent = request;
            self.counts.requests_dropped = self.counts.requests_dropped.saturating_add(1);
            return Ok(generation);
        }
        if self.lane.len() >= self.caps.lane {
            return Err(LaneRefusal::Full);
        }
        self.lane.push_back(request);
        Ok(generation)
    }

    /// Refuses an item of a never-drop class: counted, and the stream ends.
    fn refuse(&mut self) -> Pushed {
        self.counts.refused = self.counts.refused.saturating_add(1);
        self.overflowed = true;
        Pushed::Refused
    }

    /// Queues a bot's status: its latest, never older than the one
    /// waiting.
    pub(crate) fn status(&mut self, snapshot: BotSnapshot) -> Pushed {
        let bot = snapshot.bot_id;
        if self.per_bot_full(bot) {
            return self.refuse();
        }
        let newer = self
            .statuses
            .get(&bot)
            .is_none_or(|waiting| snapshot.since >= waiting.since);
        if newer {
            self.statuses.insert(bot, snapshot);
        }
        Pushed::Queued
    }

    /// Queues a bot's death.
    pub(crate) fn died(&mut self, bot: BotId, at: DateTime<Utc>) -> Pushed {
        if self.per_bot_full(bot) {
            return self.refuse();
        }
        self.died.insert(bot, at);
        Pushed::Queued
    }

    /// Queues a bot's removal.
    pub(crate) fn removed(&mut self, bot: BotId, at: DateTime<Utc>) -> Pushed {
        if self.per_bot_full(bot) {
            return self.refuse();
        }
        self.removed.insert(bot, at);
        Pushed::Queued
    }

    /// Queues a notice.
    pub(crate) fn notice(&mut self, bot: BotId, at: DateTime<Utc>, notice: Notice) -> Pushed {
        if self.notices.len() >= self.caps.notices {
            return self.refuse();
        }
        self.notices.push_back((bot, at, notice));
        Pushed::Queued
    }

    /// Queues chat, dropping its bot's oldest past its cap, or the oldest
    /// of all past the total.
    pub(crate) fn chat(&mut self, bot: BotId, at: DateTime<Utc>, chat: Chat) -> Pushed {
        let mut pushed = Pushed::Queued;
        if self.chat_of.get(&bot).copied().unwrap_or(0) >= self.caps.chat_per_bot
            && let Some(oldest) = self.chat.iter().position(|(owner, _, _)| *owner == bot)
        {
            self.drop_chat(oldest);
            pushed = Pushed::DroppedOldest;
        }
        if self.chat.len() >= self.caps.chat {
            self.drop_chat(0);
            pushed = Pushed::DroppedOldest;
        }
        self.chat.push_back((bot, at, chat));
        *self.chat_of.entry(bot).or_insert(0) += 1;
        pushed
    }

    /// Drops the chat at `index`, counted.
    fn drop_chat(&mut self, index: usize) {
        if let Some((bot, _, _)) = self.chat.remove(index) {
            self.forget_chat_of(bot);
            self.counts.chat_dropped = self.counts.chat_dropped.saturating_add(1);
        }
    }

    /// One chat of `bot` left the queue.
    fn forget_chat_of(&mut self, bot: BotId) {
        if let Some(count) = self.chat_of.get_mut(&bot) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.chat_of.remove(&bot);
            }
        }
    }

    /// What the request stream of `generation` sends next: the hello first;
    /// nothing until `Welcome`; then statuses only until the reconcile; then
    /// session requests, statuses each with its bot's death, notices,
    /// removals and chat.
    pub(crate) fn next(&mut self, generation: u64) -> Next {
        if self.overflowed {
            return Next::End;
        }
        match &mut self.phase {
            Phase::Closed => Next::End,
            Phase::Opened {
                generation: current,
                hello,
            } => {
                if *current != generation {
                    return Next::End;
                }
                hello
                    .take()
                    .map_or(Next::Wait { heartbeat: None }, |hello| {
                        Next::Send(AgentMessage::Hello(*hello))
                    })
            }
            Phase::Welcomed {
                generation: current,
                interval,
            } => {
                if *current != generation {
                    return Next::End;
                }
                let interval = *interval;
                self.statuses.pop_first().map_or(
                    Next::Wait {
                        heartbeat: Some(interval),
                    },
                    |(_, status)| Next::Send(AgentMessage::Status(status)),
                )
            }
            Phase::Reconciled {
                generation: current,
                interval,
            } => {
                if *current != generation {
                    return Next::End;
                }
                let interval = *interval;
                self.reconciled_next().map_or(
                    Next::Wait {
                        heartbeat: Some(interval),
                    },
                    Next::Send,
                )
            }
        }
    }

    /// The next message of a reconciled stream, in its order.
    fn reconciled_next(&mut self) -> Option<AgentMessage> {
        if let Some(request) = self.lane.pop_front() {
            return Some(AgentMessage::SessionRequest(request));
        }
        // A bot's status, then its death, by bot.
        let first_status = self.statuses.keys().next().copied();
        let first_death = self.died.keys().next().copied();
        let bot = match (first_status, first_death) {
            (Some(status), Some(death)) => Some(status.min(death)),
            (status, death) => status.or(death),
        };
        if let Some(bot) = bot {
            if let Some(status) = self.statuses.remove(&bot) {
                return Some(AgentMessage::Status(status));
            }
            if let Some(at) = self.died.remove(&bot) {
                return Some(event(bot, at, BotEventKind::Died));
            }
        }
        if let Some((bot, at, notice)) = self.notices.pop_front() {
            let kind = match notice {
                Notice::Alert(notification) => BotEventKind::Alert(notification),
                Notice::ChatSent(request) => BotEventKind::ChatSent { request },
                Notice::ChatFailed(request, reason) => BotEventKind::ChatFailed { request, reason },
            };
            return Some(event(bot, at, kind));
        }
        if let Some((bot, at)) = self.removed.pop_first() {
            return Some(event(bot, at, BotEventKind::Removed));
        }
        let (bot, at, chat) = self.chat.pop_front()?;
        self.forget_chat_of(bot);
        let kind = match chat {
            Chat::Received(chat) => BotEventKind::ChatReceived(chat),
            Chat::ModeSent(message) => BotEventKind::ModeChatSent(message),
        };
        Some(event(bot, at, kind))
    }
}

/// A [`BotEvent`] as a message.
const fn event(bot_id: BotId, at: DateTime<Utc>, kind: BotEventKind) -> AgentMessage {
    AgentMessage::Event(BotEvent { bot_id, at, kind })
}

#[cfg(test)]
mod tests {
    use fleet_core::bot::{BotState, PauseReason};
    use fleet_core::chat::PlayerChatKind;
    use fleet_core::disconnect::ConflictKind;
    use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
    use fleet_proto::control::SessionRequestId;

    use super::*;

    const INTERVAL: Duration = Duration::from_secs(10);

    fn caps() -> Caps {
        Caps {
            lane: 2,
            per_bot: 3,
            notices: 3,
            chat_per_bot: 2,
            chat: 3,
        }
    }

    fn bot(n: u8) -> BotId {
        BotId::new_v7(DateTime::UNIX_EPOCH, [n; 10]).unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    fn snapshot(n: u8, since: i64) -> BotSnapshot {
        BotSnapshot {
            bot_id: bot(n),
            state: BotState::Stopped,
            since: at(since),
            last_disconnect: None,
        }
    }

    fn hello(running: Vec<BotSnapshot>) -> Hello {
        Hello {
            name: AgentName::try_from("agent-1").unwrap(),
            agent_version: AgentVersion::try_from("0.1.0").unwrap(),
            protocol_version: fleet_proto::PROTOCOL_VERSION,
            capacity: AgentCapacity::try_from(10).unwrap(),
            running,
        }
    }

    fn request(number: u64, n: u8) -> SessionRequest {
        SessionRequest {
            id: SessionRequestId::new(core::num::NonZeroU64::new(number).unwrap()),
            bot_id: bot(n),
            fresh: false,
        }
    }

    fn chat(text: &str) -> Chat {
        Chat::Received(IncomingChat::player(
            PlayerChatKind::Chat,
            "Steve",
            None,
            text,
        ))
    }

    fn sent(text: &str) -> BotEventKind {
        let Chat::Received(chat) = chat(text) else {
            unreachable!()
        };
        BotEventKind::ChatReceived(chat)
    }

    fn alert() -> Notice {
        Notice::Alert(BotNotification::Paused {
            reason: PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin,
            },
        })
    }

    /// An outbox with `caps()` whose stream is reconciled with `given`, and
    /// the stream's generation.
    fn reconciled(given: &[u8]) -> (State, u64) {
        reconciled_with(caps(), given)
    }

    /// [`reconciled`] with `caps`.
    fn reconciled_with(caps: Caps, given: &[u8]) -> (State, u64) {
        let mut state = State::new(caps);
        let generation = state.open(hello(Vec::new()));
        assert!(matches!(
            state.next(generation),
            Next::Send(AgentMessage::Hello(_))
        ));
        state.welcomed(generation, INTERVAL);
        state.reconciled(generation, &given.iter().map(|n| bot(*n)).collect());
        (state, generation)
    }

    /// Everything the stream sends until it would wait.
    fn drain(state: &mut State, generation: u64) -> Vec<AgentMessage> {
        let mut sent = Vec::new();
        while let Next::Send(message) = state.next(generation) {
            sent.push(message);
        }
        sent
    }

    #[test]
    fn the_hello_goes_first_then_nothing_until_welcome() {
        let mut state = State::new(caps());
        state.status(snapshot(1, 1));
        let generation = state.open(hello(vec![snapshot(1, 0)]));

        let first = state.next(generation);
        let then = state.next(generation);

        assert_eq!(generation, 1);
        assert!(
            matches!(first, Next::Send(AgentMessage::Hello(_))),
            "{first:?}"
        );
        assert_eq!(then, Next::Wait { heartbeat: None });
        assert_eq!(state.live(), None);
    }

    #[test]
    fn before_the_reconcile_only_statuses_go_then_everything_in_order() {
        let mut state = State::new(caps());
        let generation = state.open(hello(Vec::new()));
        let _ = state.next(generation);
        state.welcomed(generation, INTERVAL);
        state.chat(bot(1), at(1), chat("hi"));
        state.notice(bot(1), at(2), alert());
        state.removed(bot(2), at(3));
        state.died(bot(1), at(4));
        state.status(snapshot(1, 5));
        assert_eq!(state.request(request(1, 1)), Ok(generation));

        let before = drain(&mut state, generation);
        state.reconciled(generation, &[bot(1), bot(2)].into());
        let after = drain(&mut state, generation);

        assert_eq!(before, [AgentMessage::Status(snapshot(1, 5))]);
        assert_eq!(
            after,
            [
                AgentMessage::SessionRequest(request(1, 1)),
                event(bot(1), at(4), BotEventKind::Died),
                event(
                    bot(1),
                    at(2),
                    BotEventKind::Alert(match alert() {
                        Notice::Alert(n) => n,
                        _ => unreachable!(),
                    })
                ),
                event(bot(2), at(3), BotEventKind::Removed),
                event(bot(1), at(1), sent("hi")),
            ]
        );
        assert_eq!(
            state.next(generation),
            Next::Wait {
                heartbeat: Some(INTERVAL)
            }
        );
    }

    #[test]
    fn a_bot_keeps_its_latest_status_never_an_older_one() {
        let (mut state, generation) = reconciled(&[1]);
        state.status(snapshot(1, 5));
        state.status(snapshot(1, 7));
        state.status(snapshot(1, 6));

        assert_eq!(
            drain(&mut state, generation),
            [AgentMessage::Status(snapshot(1, 7))]
        );
    }

    #[test]
    fn a_status_and_its_bots_death_go_together() {
        let (mut state, generation) = reconciled(&[1, 2]);
        state.died(bot(2), at(1));
        state.status(snapshot(1, 2));
        state.status(snapshot(2, 3));

        assert_eq!(
            drain(&mut state, generation),
            [
                AgentMessage::Status(snapshot(1, 2)),
                AgentMessage::Status(snapshot(2, 3)),
                event(bot(2), at(1), BotEventKind::Died),
            ]
        );
    }

    #[test]
    fn prepare_drops_the_statuses_waiting_and_keeps_the_rest() {
        let mut state = State::new(caps());
        state.status(snapshot(1, 1));
        state.died(bot(1), at(2));

        state.prepare();
        state.status(snapshot(2, 3));
        let generation = state.open(hello(vec![snapshot(1, 0)]));
        let _ = state.next(generation);
        state.welcomed(generation, INTERVAL);
        state.reconciled(generation, &[bot(1), bot(2)].into());

        assert_eq!(
            drain(&mut state, generation),
            [
                event(bot(1), at(2), BotEventKind::Died),
                AgentMessage::Status(snapshot(2, 3)),
            ]
        );
    }

    #[test]
    fn a_removal_of_a_bot_the_hello_omits_is_dropped_and_one_it_lists_waits() {
        let mut state = State::new(caps());
        state.removed(bot(1), at(1));
        state.removed(bot(2), at(2));

        let generation = state.open(hello(vec![snapshot(2, 0)]));
        let _ = state.next(generation);
        state.welcomed(generation, INTERVAL);
        state.reconciled(generation, &BTreeSet::new());

        assert_eq!(
            drain(&mut state, generation),
            [event(bot(2), at(2), BotEventKind::Removed)]
        );
    }

    #[test]
    fn the_reconcile_drops_what_waits_for_a_bot_it_doesnt_give_but_its_removal() {
        let mut state = State::new(caps());
        state.chat(bot(1), at(1), chat("kept"));
        state.chat(bot(2), at(2), chat("dropped"));
        state.notice(bot(2), at(3), alert());
        state.died(bot(2), at(4));
        state.removed(bot(2), at(5));
        let generation = state.open(hello(vec![snapshot(2, 0)]));
        let _ = state.next(generation);
        state.welcomed(generation, INTERVAL);

        state.reconciled(generation, &[bot(1)].into());

        assert_eq!(
            drain(&mut state, generation),
            [
                event(bot(2), at(5), BotEventKind::Removed),
                event(bot(1), at(1), sent("kept")),
            ]
        );
    }

    #[test]
    fn the_lane_answers_no_stream_until_welcome_and_after_the_end() {
        let mut state = State::new(caps());
        let before = state.request(request(1, 1));
        let generation = state.open(hello(Vec::new()));
        let opened = state.request(request(2, 1));
        state.welcomed(generation, INTERVAL);
        let welcomed = state.request(request(3, 1));
        state.ended(generation);
        let after = state.request(request(4, 1));

        assert_eq!(before, Err(LaneRefusal::NoStream));
        assert_eq!(opened, Err(LaneRefusal::NoStream));
        assert_eq!(welcomed, Ok(generation));
        assert_eq!(after, Err(LaneRefusal::NoStream));
        assert_eq!(state.counts().requests_dropped, 1, "the unsent one");
    }

    #[test]
    fn a_newer_request_replaces_its_bots_unsent_one_and_a_full_lane_refuses() {
        let (mut state, generation) = reconciled(&[1, 2, 3]);
        state.request(request(1, 1)).unwrap();
        state.request(request(2, 2)).unwrap();

        let replaced = state.request(request(3, 1));
        let full = state.request(request(4, 3));

        assert_eq!(replaced, Ok(generation));
        assert_eq!(full, Err(LaneRefusal::Full));
        assert_eq!(
            drain(&mut state, generation),
            [
                AgentMessage::SessionRequest(request(3, 1)),
                AgentMessage::SessionRequest(request(2, 2)),
            ]
        );
        assert_eq!(state.counts().requests_dropped, 1);
    }

    #[test]
    fn chat_past_its_bots_cap_or_the_total_drops_the_oldest() {
        let (mut state, generation) = reconciled(&[1, 2]);
        state.chat(bot(1), at(1), chat("a"));
        state.chat(bot(1), at(2), chat("b"));
        let per_bot = state.chat(bot(1), at(3), chat("c"));
        state.chat(bot(2), at(4), chat("d"));
        let total = state.chat(bot(2), at(5), chat("e"));

        assert_eq!(per_bot, Pushed::DroppedOldest);
        assert_eq!(total, Pushed::DroppedOldest);
        assert_eq!(
            drain(&mut state, generation),
            [
                event(bot(1), at(3), sent("c")),
                event(bot(2), at(4), sent("d")),
                event(bot(2), at(5), sent("e")),
            ]
        );
        assert_eq!(state.counts().chat_dropped, 2);
        assert!(!state.overflowed());
    }

    #[test]
    fn a_full_per_bot_class_refuses_a_new_bot_and_ends_the_stream() {
        let (mut state, generation) = reconciled(&[1, 2, 3, 4]);
        state.status(snapshot(1, 1));
        state.died(bot(2), at(1));
        state.removed(bot(3), at(1));

        let known = state.status(snapshot(1, 2));
        let refused = state.removed(bot(4), at(2));

        assert_eq!(known, Pushed::Queued);
        assert_eq!(refused, Pushed::Refused);
        assert!(state.overflowed());
        assert_eq!(state.next(generation), Next::End);
        assert_eq!(state.counts().refused, 1);
    }

    #[test]
    fn a_full_notice_class_refuses_and_ends_the_stream() {
        let (mut state, generation) = reconciled(&[1]);
        for second in 0..3 {
            assert_eq!(state.notice(bot(1), at(second), alert()), Pushed::Queued);
        }

        let refused = state.notice(
            bot(1),
            at(4),
            Notice::ChatSent(ChatRequestId::new_v7(DateTime::UNIX_EPOCH, [9; 10]).unwrap()),
        );

        assert_eq!(refused, Pushed::Refused);
        assert_eq!(state.next(generation), Next::End);
    }

    #[test]
    fn closing_ends_whichever_stream_is_current_and_its_lane() {
        let (mut state, generation) = reconciled(&[1]);
        assert_eq!(state.request(request(1, 1)), Ok(generation));

        let closed = state.close();

        assert_eq!(closed, Some(generation));
        assert_eq!(state.next(generation), Next::End);
        assert_eq!(state.live(), None);
        assert_eq!(state.waiting()[0], 0, "the lane closed");
        assert_eq!(state.close(), None, "nothing is open any more");
    }

    #[test]
    fn the_next_stream_starts_without_the_overflow_and_with_what_still_waits() {
        let (mut state, generation) = reconciled(&[1]);
        for second in 0..4 {
            state.notice(bot(1), at(second), alert());
        }
        state.ended(generation);

        let next = state.open(hello(Vec::new()));

        assert_eq!(next, generation + 1);
        assert!(!state.overflowed());
        assert!(matches!(
            state.next(next),
            Next::Send(AgentMessage::Hello(_))
        ));
        assert_eq!(state.waiting()[2], 3);
    }

    #[test]
    fn an_old_streams_reader_gets_the_end() {
        let (mut state, generation) = reconciled(&[1]);
        state.ended(generation);
        let next = state.open(hello(Vec::new()));

        assert_eq!(state.next(generation), Next::End);
        assert!(matches!(
            state.next(next),
            Next::Send(AgentMessage::Hello(_))
        ));
    }

    #[test]
    fn a_stale_end_or_welcome_changes_nothing() {
        let mut state = State::new(caps());
        let first = state.open(hello(Vec::new()));
        state.ended(first);
        let second = state.open(hello(Vec::new()));

        state.welcomed(first, INTERVAL);
        state.ended(first);

        assert_eq!(state.live(), None);
        assert!(matches!(
            state.next(second),
            Next::Send(AgentMessage::Hello(_))
        ));
        state.welcomed(second, INTERVAL);
        assert_eq!(state.live(), Some(second));
    }

    #[test]
    fn nothing_is_lost_below_the_caps_whatever_the_order() {
        // A sweep over every order of a few pushes, below every cap.
        let pushes: [fn(&mut State, i64); 4] = [
            |state, second| {
                state.status(snapshot(1, second));
            },
            |state, second| {
                state.died(bot(1), at(second));
            },
            |state, second| {
                state.notice(bot(1), at(second), alert());
            },
            |state, second| {
                state.chat(bot(1), at(second), chat("x"));
            },
        ];
        for order in 0..256_u32 {
            let (mut state, generation) = reconciled_with(Caps::of(10, 4), &[1]);
            for step in 0..4 {
                let index = usize::try_from((order >> (step * 2)) & 3).unwrap();
                pushes[index](&mut state, i64::from(step));
            }
            let waiting: usize = state.waiting()[1..].iter().sum();

            let sent = drain(&mut state, generation);

            assert!(!state.overflowed(), "{order}");
            assert!(sent.len() >= waiting, "{order}: {sent:?}");
            assert_eq!(state.waiting(), [0; 4], "{order}");
        }
    }
}
