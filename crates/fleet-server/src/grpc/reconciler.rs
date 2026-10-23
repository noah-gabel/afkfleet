//! The reconciler: which agent runs which bot, kept true (Plan.md P10.5;
//! ADR-0019, S32–S45, R24–R30).
//!
//! One task, owned by serve, runs a **pass** at startup, on every hint
//! ([`Hints`], which never waits: a full queue folds into one full pass), every
//! [`RECONCILE_INTERVAL`], and at the nearest bound (a bot being stopped, or
//! the placement gate). A pass:
//! 1. clears the bots whose `Removed` the registry noted (`bot.detach`);
//! 2. reads every bot, release record and enabled agent from one snapshot,
//!    and every live stream from the registry;
//! 3. plans with fleet-core's [`plan`];
//! 4. commits each write in its own audited transaction (detaches, the
//!    hellos' strays, unassignments, placements);
//! 5. only then tells the agents: a welcome's one full reconcile, or one
//!    message per bot, each checked against the stream's generation.
//!
//! **The live events** (ADR-0020, S42, R37): a pass ends by telling the
//! sockets, in one hint, which bots may read differently: every bot one of
//! its writes changed or a stream now holds, and every bot whose row now
//! names another agent, or an agent that shows otherwise than at the last
//! pass (its stream, its enabled flag, its certificate's expiry). The first
//! pass only notes what it saw.
//!
//! **Facts live in state**, never only in a hint: a hint that's lost costs
//! latency, never correctness. `QueueFull` and `NotConnected` mean the
//! stream is gone, and its next hello resyncs. Nothing here moves a bot by
//! time: only its agent's word does (S32, S44). **The placement gate**
//! (S45): after the server starts, nothing new is placed until every enabled
//! agent was welcomed, or [`PLACEMENT_GRACE`] passed. **A welcome waits for
//! its writes** (R29): when one of them fails, its full reconcile waits for
//! the next pass.

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use fleet_core::agent::{AgentStatus, status};
use fleet_core::bot::{BotSpec, LastReport, LastState, StickyState};
use fleet_core::chat::ChatRate;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::{AgentId, BotId};
use fleet_core::placement::{
    AgentFacts, AgentPlan, BotFacts, DetachReason, Facts, Held, HoldFacts, Let, Listed, Phase,
    PlaceFacts, ReleaseFacts, Run, StopWhy, Stopping, Untouchable, placeable, plan,
};
use fleet_core::system::Clock;
use fleet_proto::control::{Assignment, ServerMessage};
use tokio::sync::{Notify, mpsc};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::registry::{
    AgentRegistry, Hold, HoldEntry, LatestReport, LiveView, SendError, StopState, ViewPhase,
};
use crate::http::error::chain;
use crate::ports::assignments::{AssignError, Assignments};
use crate::ports::bots::{BotRecord, Fleet};
use crate::ports::live_hints::{LiveHints, NoLiveHints};
use crate::ports::reconcile::{Change, Reconcile};
use crate::ports::store::StoreError;

/// How often a full pass runs besides the hints.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a bot that stopped qualifying may take to rest before it's
/// unassigned anyway (S33, S42).
pub const STOP_BOUND: Duration = Duration::from_secs(60);

/// How long after the server starts nothing new is placed while an enabled
/// agent hasn't said hello yet (S45): an agent's keepalive notices the old
/// server within 30 s, reconnects within
/// [`RECONNECT_DELAY_MAX`](fleet_proto::control::RECONNECT_DELAY_MAX), and is
/// welcomed within [`WELCOME_TIMEOUT`](fleet_proto::control::WELCOME_TIMEOUT).
pub const PLACEMENT_GRACE: Duration = Duration::from_secs(90);

/// How many hints wait at most; more fold into one full pass.
pub const HINT_QUEUE: usize = 256;

/// What a pass's timing is: [`RECONCILE_INTERVAL`], [`STOP_BOUND`] and
/// [`PLACEMENT_GRACE`]; tests lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileTimings {
    /// How often a full pass runs.
    pub interval: Duration,
    /// How long a bot that stopped qualifying may take to rest.
    pub stop_bound: Duration,
    /// How long new placements wait after the start at most.
    pub grace: Duration,
}

impl Default for ReconcileTimings {
    fn default() -> Self {
        Self {
            interval: RECONCILE_INTERVAL,
            stop_bound: STOP_BOUND,
            grace: PLACEMENT_GRACE,
        }
    }
}

/// The bound a hint queue overflowed: one full pass catches up.
#[derive(Debug, Default)]
struct Overflow {
    full: AtomicBool,
    notify: Notify,
}

/// The reconciler's hint queue's sending side; clones share it. See the
/// [module docs](self).
#[derive(Debug, Clone)]
pub struct Hints {
    queue: mpsc::Sender<Change>,
    overflow: Arc<Overflow>,
}

/// The reconciler's hint queue's receiving side.
#[derive(Debug)]
pub struct HintInbox {
    queue: mpsc::Receiver<Change>,
    overflow: Arc<Overflow>,
}

/// A hint queue of `depth` hints, at least one.
#[must_use]
pub fn hints(depth: usize) -> (Hints, HintInbox) {
    let (queue, inbox) = mpsc::channel(depth.max(1));
    let overflow = Arc::new(Overflow::default());
    (
        Hints {
            queue,
            overflow: Arc::clone(&overflow),
        },
        HintInbox {
            queue: inbox,
            overflow,
        },
    )
}

impl Reconcile for Hints {
    fn changed(&self, change: Change) {
        match self.queue.try_send(change) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                if !self.overflow.full.swap(true, Ordering::AcqRel) {
                    warn!("the reconciler's hints overflowed; a full pass catches up");
                }
                self.overflow.notify.notify_one();
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                debug!("a hint for a reconciler that ended was dropped");
            }
        }
    }
}

/// The reconciler's task; see the [module docs](self).
#[derive(Debug)]
pub struct Reconciler {
    registry: AgentRegistry,
    store: Arc<dyn Assignments>,
    clock: Arc<dyn Clock>,
    inbox: HintInbox,
    timings: ReconcileTimings,
    allow_offline: bool,
    /// Every bot's chat rate, sent in its spec (ADR-0020, S3).
    chat_rate: ChatRate,
    /// When the task was made: the placement gate's start.
    started: Instant,
    /// When the last pass started. A bound it found passed was acted on, or
    /// its write failed and the interval tries again: only a later one wakes
    /// the task, so a failing write never makes it spin.
    last_pass: Instant,
    /// The agents welcomed since then.
    welcomed: HashSet<AgentId>,
    /// Whether the gate has opened; it stays open.
    gate_open: bool,
    /// Whether every hint sender is gone.
    hints_closed: bool,
    /// Tells the live events sockets which bots a pass changed (S42).
    live_hints: Arc<dyn LiveHints>,
    /// What the last pass that read the fleet saw of each bot's agent.
    observed: Option<Observed>,
}

impl Reconciler {
    /// The reconciler of `registry`'s streams over `store`, stamping with
    /// `clock`, hinted through `inbox`, timed by `timings`; offline accounts'
    /// bots run only when `allow_offline`, and every bot chats at
    /// `chat_rate`, `[rate_limits]`' setting.
    #[must_use]
    pub fn new(
        registry: AgentRegistry,
        store: Arc<dyn Assignments>,
        clock: Arc<dyn Clock>,
        inbox: HintInbox,
        timings: ReconcileTimings,
        allow_offline: bool,
        chat_rate: ChatRate,
    ) -> Self {
        Self {
            registry,
            store,
            clock,
            inbox,
            timings,
            allow_offline,
            chat_rate,
            started: Instant::now(),
            last_pass: Instant::now(),
            welcomed: HashSet::new(),
            gate_open: false,
            hints_closed: false,
            live_hints: Arc::new(NoLiveHints),
            observed: None,
        }
    }

    /// The same, telling the live events sockets through `live_hints` which
    /// bots each pass changed (ADR-0020, S42).
    #[must_use]
    pub fn with_live_hints(self, live_hints: Arc<dyn LiveHints>) -> Self {
        Self { live_hints, ..self }
    }

    /// Runs a pass now, then one on every hint, every interval and at the
    /// nearest bound, until `stop` is cancelled. A pass in progress runs to
    /// its end: each of its writes is short, bounded by the pools' acquire
    /// timeout and SQLite's busy timeout.
    pub async fn run(mut self, stop: CancellationToken) {
        self.pass().await;
        let interval = self.timings.interval;
        let mut ticks = interval_at(Instant::now() + interval, interval);
        // A pass that took longer than a period isn't followed by a burst.
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let overflow = Arc::clone(&self.inbox.overflow);
        loop {
            let deadline = self.next_deadline();
            tokio::select! {
                biased;
                // Cancel-safe: waiting for the token loses nothing.
                () = stop.cancelled() => return,
                // Cancel-safe: `recv` loses no hint it didn't return.
                hint = self.inbox.queue.recv(), if !self.hints_closed => {
                    if hint.is_none() {
                        self.hints_closed = true;
                    }
                    // Fold every waiting hint into this pass.
                    while self.inbox.queue.try_recv().is_ok() {}
                }
                // Cancel-safe: a notification that arrives meanwhile stays
                // stored for the next call.
                () = overflow.notify.notified() => {}
                // Cancel-safe, as tokio documents `Interval::tick`.
                _ = ticks.tick() => {}
                // Cancel-safe: a sleep restarts from the same deadline.
                () = sleep_until_some(deadline) => {}
            }
            overflow.full.store(false, Ordering::Release);
            if stop.is_cancelled() {
                return;
            }
            self.pass().await;
        }
    }

    /// The nearest moment after the last pass's start a pass is due by
    /// time: a bot's stop bound, or the gate's end while it's closed.
    fn next_deadline(&self) -> Option<Instant> {
        let gate = (!self.gate_open)
            .then(|| self.started + self.timings.grace)
            .filter(|gate| *gate > self.last_pass);
        match (self.registry.next_deadline(self.last_pass), gate) {
            (Some(bound), Some(gate)) => Some(bound.min(gate)),
            (bound, gate) => bound.or(gate),
        }
    }

    /// Whether new placements may happen (S45): once every enabled agent was
    /// welcomed since the start, or the grace passed; then for good.
    fn gate(&mut self, enabled: &BTreeSet<AgentId>) -> bool {
        if !self.gate_open
            && (Instant::now() >= self.started + self.timings.grace
                || enabled.iter().all(|agent| self.welcomed.contains(agent)))
        {
            self.gate_open = true;
        }
        self.gate_open
    }

    /// One pass; see the [module docs](self).
    async fn pass(&mut self) {
        let mut touched = BTreeSet::new();
        self.planned(&mut touched).await;
        let touched: Vec<BotId> = touched.into_iter().collect();
        self.live_hints.bots(&touched);
    }

    /// One pass's work, noting in `touched` each bot it changed, held, or
    /// whose agent shows otherwise since the last pass.
    async fn planned(&mut self, touched: &mut BTreeSet<BotId>) {
        self.last_pass = Instant::now();
        touched.extend(self.detach_released().await);
        let fleet = match self.store.fleet().await {
            Ok(fleet) => fleet,
            Err(failure) => {
                warn!(
                    error = %chain(&failure),
                    "the reconciler couldn't read the bots; the next pass tries again"
                );
                return;
            }
        };
        let views = self.registry.placement_view();
        let observed = Observed::of(&fleet, &views, self.clock.now());
        if let Some(before) = self.observed.replace(observed.clone()) {
            touched.extend(observed.changed_since(&before));
        }
        let untouchable = untouchable(&fleet);
        // An untouchable bot keeps what's known of it too.
        let stored: HashSet<BotId> = fleet
            .bots
            .iter()
            .map(|record| record.bot.id)
            .chain(untouchable.iter().map(|frozen| frozen.bot))
            .collect();
        self.registry.retain_latest(&|bot| stored.contains(&bot));
        // Only a welcomed stream is in the view: the registry starts empty.
        self.welcomed.extend(views.iter().map(|view| view.agent));
        let gate_open = self.gate(&fleet.enabled_agents);
        let known = Known::new(&self.registry, &fleet, &views);
        let bots = known.bot_facts(self.allow_offline);
        let releases: Vec<ReleaseFacts> = fleet
            .releases
            .iter()
            .map(|record| ReleaseFacts {
                bot: record.bot,
                agent: record.agent,
                identity: record.identity.clone(),
            })
            .collect();
        let agents = known.agent_facts(&fleet);
        let plan = plan(&Facts {
            bots: &bots,
            releases: &releases,
            untouchable: &untouchable,
            agents: &agents,
            gate_open,
        });

        // The writes first, each committed before any message says so.
        let mut held_back = HashSet::new();
        for detach in &plan.detach {
            let last = known.report_of(detach.agent, detach.bot);
            match self
                .store
                .detach(detach.bot, detach.agent, detach.reason, last)
                .await
            {
                Ok(changed) => {
                    detached(changed, detach.bot, detach.agent, detach.reason);
                    if changed {
                        touched.insert(detach.bot);
                    }
                }
                Err(failure) => {
                    held_back.insert(detach.agent);
                    failed(&failure, detach.bot, detach.agent, "detach");
                }
            }
        }
        // Before the welcome's full reconcile, so the fence outlives its
        // stream (S43).
        for stray in &plan.stray {
            let assigned_to = known
                .records
                .get(&stray.bot)
                .and_then(|record| record.bot.agent);
            match self.store.stray(stray.bot, stray.agent, assigned_to).await {
                Ok(true) => warn!(
                    bot_id = %stray.bot,
                    agent_id = %stray.agent,
                    "an agent runs a bot the server didn't give it; the bot is fenced until it's removed"
                ),
                Ok(false) => {}
                Err(failure) => {
                    held_back.insert(stray.agent);
                    failed(&failure, stray.bot, stray.agent, "stray");
                }
            }
        }
        for part in &plan.agents {
            if part.full && held_back.contains(&part.agent) {
                continue;
            }
            self.apply(part, &known, touched).await;
        }
    }

    /// Clears the bots whose `Removed` the registry noted; a note whose
    /// write fails goes back for the next pass. The bots it changed.
    async fn detach_released(&self) -> Vec<BotId> {
        let mut changed_bots = Vec::new();
        for released in self.registry.take_released() {
            let last = self
                .registry
                .latest(released.bot)
                .filter(|report| report.agent == released.agent)
                .map(|report| report.last());
            match self
                .store
                .detach(released.bot, released.agent, DetachReason::Removed, last)
                .await
            {
                Ok(changed) => {
                    detached(changed, released.bot, released.agent, DetachReason::Removed);
                    if changed {
                        changed_bots.push(released.bot);
                    }
                }
                Err(failure) => {
                    failed(&failure, released.bot, released.agent, "detach");
                    self.registry.requeue_released(released);
                }
            }
        }
        changed_bots
    }

    /// Applies one agent's part: its writes, then its messages, noting in
    /// `touched` each bot they changed or held.
    async fn apply(&self, part: &AgentPlan, known: &Known<'_>, touched: &mut BTreeSet<BotId>) {
        // Unassignments first: a welcome whose decision can't be recorded
        // places nothing and waits for the next pass.
        let mut lets: Vec<&Let> = Vec::new();
        for release in &part.release {
            if let Some(reason) = release.unassign {
                let last = known.report_of(part.agent, release.bot);
                match self
                    .store
                    .unassign(release.bot, part.agent, reason, last)
                    .await
                {
                    Ok(true) => {
                        info!(
                            bot_id = %release.bot,
                            agent_id = %part.agent,
                            reason = reason.as_str(),
                            "a bot was unassigned from its agent"
                        );
                        touched.insert(release.bot);
                    }
                    Ok(false) => {}
                    Err(failure) => {
                        failed(&failure, release.bot, part.agent, "unassign");
                        if part.full {
                            return;
                        }
                        continue;
                    }
                }
            }
            lets.push(release);
        }
        let mut runs: Vec<(&Run, Cow<'_, BotRecord>)> = Vec::new();
        for run in &part.run {
            let Some(record) = known.records.get(&run.bot).copied() else {
                continue;
            };
            let mut record = Cow::Borrowed(record);
            if run.placed {
                match self.store.assign(run.bot, part.agent).await {
                    Ok(Some(assigned)) => {
                        info!(
                            bot_id = %run.bot,
                            agent_id = %part.agent,
                            "a bot was assigned to an agent"
                        );
                        touched.insert(run.bot);
                        // Its restore comes from the row its assignment read:
                        // a Resume or a Reset may have cleared the stored state
                        // since this pass read the fleet (S48).
                        if assigned.last != record.bot.last {
                            record.to_mut().bot.last = assigned.last;
                        }
                    }
                    Ok(None) => {
                        debug!(bot_id = %run.bot, "a bot changed before its placement");
                        continue;
                    }
                    Err(failure) => {
                        failed(&failure, run.bot, part.agent, "assign");
                        continue;
                    }
                }
            }
            runs.push((run, record));
        }
        if part.full {
            self.settle(part, known, (&runs, &lets), touched);
        } else {
            self.update(part, known, (&runs, &lets), touched);
        }
    }

    /// A welcome's one full reconcile, and its holds, noting the bots it
    /// held in `touched` once they're held.
    fn settle(
        &self,
        part: &AgentPlan,
        known: &Known<'_>,
        (runs, lets): (&[(&Run, Cow<'_, BotRecord>)], &[&Let]),
        touched: &mut BTreeSet<BotId>,
    ) {
        let now = self.clock.now();
        let mut assignments = Vec::new();
        let mut holds = Vec::new();
        for (run, record) in runs {
            let Some(spec) = spec_of(record, run, None, self.chat_rate) else {
                // The planner lets a bot without a server go; held so its
                // `Removed` still clears it.
                holds.push((run.bot, releasing(now)));
                continue;
            };
            assignments.push(Assignment {
                spec: spec.clone(),
                restore: known.restore(record),
            });
            let entry = HoldEntry {
                hold: Hold::Assigned,
                since: now,
                sent: Some(spec),
                stopping: self.stop_state(run, None),
            };
            holds.push((run.bot, entry));
        }
        holds.extend(lets.iter().map(|release| (release.bot, releasing(now))));
        let held: Vec<BotId> = holds.iter().map(|(bot, _)| *bot).collect();
        let result = self.registry.settle(
            part.agent,
            part.generation,
            holds,
            ServerMessage::Reconcile(assignments),
        );
        if result.is_ok() {
            touched.extend(held);
        }
        sent(part.agent, result);
    }

    /// A reconciled stream's changes, one message per bot, noting each bot
    /// held anew in `touched`.
    fn update(
        &self,
        part: &AgentPlan,
        known: &Known<'_>,
        (runs, lets): (&[(&Run, Cow<'_, BotRecord>)], &[&Let]),
        touched: &mut BTreeSet<BotId>,
    ) {
        let mut held = |bot: BotId, result: Result<(), SendError>| {
            if result.is_ok() {
                touched.insert(bot);
            }
            sent(part.agent, result);
        };
        let now = self.clock.now();
        for (run, record) in runs {
            if run.placed {
                let Some(spec) = spec_of(record, run, None, self.chat_rate) else {
                    continue;
                };
                let entry = HoldEntry {
                    hold: Hold::Assigned,
                    since: now,
                    sent: Some(spec.clone()),
                    stopping: None,
                };
                let message = ServerMessage::Assign(Assignment {
                    spec,
                    restore: known.restore(record),
                });
                let result = self.registry.hold_and_send(
                    part.agent,
                    part.generation,
                    run.bot,
                    entry,
                    Some(message),
                );
                held(run.bot, result);
                continue;
            }
            let Some(kept) = known.entry(part.agent, run.bot) else {
                // An assignment the agent never got: sent again.
                if let Some(result) = self.resend(part, known, run, record, now) {
                    held(run.bot, result);
                }
                continue;
            };
            let spec = spec_of(record, run, kept.sent.as_ref(), self.chat_rate);
            let entry = HoldEntry {
                hold: Hold::Assigned,
                since: kept.since,
                sent: spec.clone().or_else(|| kept.sent.clone()),
                stopping: self.stop_state(run, kept.stopping),
            };
            if entry == *kept {
                continue;
            }
            let message = spec
                .filter(|spec| kept.sent.as_ref() != Some(spec))
                .map(ServerMessage::Update);
            let result =
                self.registry
                    .hold_and_send(part.agent, part.generation, run.bot, entry, message);
            held(run.bot, result);
        }
        for undelivered in &part.undelivered {
            info!(
                bot_id = %undelivered.bot,
                agent_id = %part.agent,
                reason = undelivered.why.as_str(),
                "an assignment its agent never got isn't sent again: its next hello settles it"
            );
        }
        for release in lets.iter().filter(|release| release.send) {
            let result = self.registry.hold_and_send(
                part.agent,
                part.generation,
                release.bot,
                releasing(now),
                Some(ServerMessage::Unassign(release.bot)),
            );
            held(release.bot, result);
        }
    }

    /// Sends `run`'s bot, whose row names `part`'s agent but which its stream
    /// doesn't hold, again, and holds it: the hold's result, when it tried.
    fn resend(
        &self,
        part: &AgentPlan,
        known: &Known<'_>,
        run: &Run,
        record: &BotRecord,
        now: DateTime<Utc>,
    ) -> Option<Result<(), SendError>> {
        let spec = spec_of(record, run, None, self.chat_rate)?;
        let entry = HoldEntry {
            hold: Hold::Assigned,
            since: now,
            sent: Some(spec.clone()),
            stopping: self.stop_state(run, None),
        };
        let message = ServerMessage::Assign(Assignment {
            spec,
            restore: known.restore(record),
        });
        info!(
            bot_id = %run.bot,
            agent_id = %part.agent,
            "an assignment its agent never got is sent again"
        );
        Some(self.registry.hold_and_send(
            part.agent,
            part.generation,
            run.bot,
            entry,
            Some(message),
        ))
    }

    /// The stop state of `run`, which was `before`: a bound starts once.
    fn stop_state(&self, run: &Run, before: Option<StopState>) -> Option<StopState> {
        match (run.stopping?, before) {
            (StopWhy::Unqualified, Some(kept @ StopState::Unqualified { .. })) => Some(kept),
            (StopWhy::Unqualified, _) => Some(StopState::Unqualified {
                until: Instant::now() + self.timings.stop_bound,
            }),
            (StopWhy::Move, _) => Some(StopState::Move),
        }
    }
}

/// What one pass knows: the stored bots and the live streams, by ID.
struct Known<'a> {
    registry: &'a AgentRegistry,
    records: HashMap<BotId, &'a BotRecord>,
    views: &'a [LiveView],
    /// Each live stream's reports: its hello's, or its held bots' latest.
    reports: HashMap<(AgentId, BotId), &'a LatestReport>,
    /// Each reconciled stream's holds.
    entries: HashMap<(AgentId, BotId), &'a HoldEntry>,
}

impl<'a> Known<'a> {
    fn new(registry: &'a AgentRegistry, fleet: &'a Fleet, views: &'a [LiveView]) -> Self {
        let mut reports = HashMap::new();
        let mut entries = HashMap::new();
        for view in views {
            match &view.phase {
                ViewPhase::Listed(listed) => {
                    for report in listed {
                        reports.insert((view.agent, report.snapshot.bot_id), report);
                    }
                }
                ViewPhase::Reconciled(held) => {
                    for held in held {
                        entries.insert((view.agent, held.bot), &held.entry);
                        if let Some(report) = &held.report {
                            reports.insert((view.agent, held.bot), report);
                        }
                    }
                }
            }
        }
        Self {
            registry,
            records: fleet
                .bots
                .iter()
                .map(|record| (record.bot.id, record))
                .collect(),
            views,
            reports,
            entries,
        }
    }

    /// `agent`'s latest report of `bot`: on its live stream, or kept from an
    /// earlier one.
    fn report_of(&self, agent: AgentId, bot: BotId) -> Option<LastReport> {
        match self.reports.get(&(agent, bot)) {
            Some(report) => Some(report.last()),
            None => self
                .registry
                .latest(bot)
                .filter(|report| report.agent == agent)
                .map(|report| report.last()),
        }
    }

    /// The newest state known of `record`'s bot: stored, kept in memory, or
    /// in the hello of the agent its row names.
    fn last_of(&self, record: &BotRecord) -> Option<LastReport> {
        let bot = record.bot.id;
        let listed = record
            .bot
            .agent
            .and_then(|agent| self.reports.get(&(agent, bot)))
            .map(|report| report.last());
        let latest = self.registry.latest(bot).map(|report| report.last());
        [record.bot.last, latest, listed]
            .into_iter()
            .flatten()
            .max_by_key(|last| last.at)
    }

    /// The sticky state `record`'s bot starts in, if any (ADR-0013).
    fn restore(&self, record: &BotRecord) -> Option<StickyState> {
        self.last_of(record).and_then(|last| last.state.sticky())
    }

    /// `agent`'s hold of `bot` on its reconciled stream.
    fn entry(&self, agent: AgentId, bot: BotId) -> Option<&'a HoldEntry> {
        self.entries.get(&(agent, bot)).copied()
    }

    /// Every stored bot, as the planner sees it.
    fn bot_facts(&self, allow_offline: bool) -> Vec<BotFacts> {
        self.records
            .values()
            .map(|record| BotFacts {
                id: record.bot.id,
                identity: record.identity.clone(),
                qualifies: placeable(&PlaceFacts {
                    has_server: record.bot.server.is_some(),
                    desired: record.bot.desired,
                    owner_enabled: record.owner_enabled,
                    kind: record.kind,
                    status: record.status,
                    allow_offline,
                }),
                agent: record.bot.agent,
                sticky: self.restore(record).is_some(),
            })
            .collect()
    }

    /// Every live stream, as the planner sees it.
    fn agent_facts(&self, fleet: &Fleet) -> Vec<AgentFacts> {
        let now = Instant::now();
        self.views
            .iter()
            .map(|view| AgentFacts {
                id: view.agent,
                generation: view.generation,
                plannable: !view.halted && fleet.enabled_agents.contains(&view.agent),
                capacity: view.capacity,
                phase: match &view.phase {
                    ViewPhase::Listed(listed) => Phase::Welcomed {
                        listed: listed
                            .iter()
                            .map(|report| Listed {
                                bot: report.snapshot.bot_id,
                                rests: rests(report),
                            })
                            .collect(),
                    },
                    ViewPhase::Reconciled(held) => Phase::Reconciled {
                        holds: held
                            .iter()
                            .map(|held| HoldFacts {
                                bot: held.bot,
                                held: match held.entry.hold {
                                    Hold::Releasing => Held::Releasing,
                                    Hold::Assigned => Held::Assigned {
                                        stopping: held.entry.stopping.map(|stop| match stop {
                                            StopState::Unqualified { until } => {
                                                Stopping::Unqualified {
                                                    overdue: now >= until,
                                                }
                                            }
                                            StopState::Move => Stopping::Move,
                                        }),
                                        rests: held.report.as_ref().is_some_and(rests),
                                    },
                                },
                            })
                            .collect(),
                    },
                },
            })
            .collect()
    }
}

/// The bots of `fleet` that don't read back, as the planner leaves them
/// alone; each is logged at `error`, once a pass.
fn untouchable(fleet: &Fleet) -> Vec<Untouchable> {
    fleet
        .unreadable
        .iter()
        .map(|unreadable| {
            let problem = StoreError::Corrupt {
                table: unreadable.table,
                column: unreadable.column,
                rowid: unreadable.rowid,
            };
            error!(
                bot_id = %unreadable.bot,
                agent_id = ?unreadable.agent,
                error = %problem,
                "a bot doesn't read back: the reconciler leaves it alone until it's fixed"
            );
            Untouchable {
                bot: unreadable.bot,
                agent: unreadable.agent,
                identity: unreadable.identity.clone(),
            }
        })
        .collect()
}

/// Whether `report`'s state rests: Stopped, Paused or Failed.
fn rests(report: &LatestReport) -> bool {
    LastState::from(report.snapshot.state).rests()
}

/// `run`'s spec from `record`, with the server of the spec last sent while
/// the bot has none any more, and `chat_rate`.
fn spec_of(
    record: &BotRecord,
    run: &Run,
    sent: Option<&BotSpec>,
    chat_rate: ChatRate,
) -> Option<BotSpec> {
    let server = record
        .bot
        .server
        .clone()
        .or_else(|| sent.map(|spec| spec.server.clone()))?;
    Some(BotSpec {
        id: record.bot.id,
        account: record.account.clone(),
        server,
        mode: record.mode.clone(),
        desired: run.desired,
        conflict_texts: ConflictTexts::default(),
        chat_rate,
    })
}

/// A hold that waits for the bot's `Removed`.
const fn releasing(now: DateTime<Utc>) -> HoldEntry {
    HoldEntry {
        hold: Hold::Releasing,
        since: now,
        sent: None,
        stopping: None,
    }
}

/// Logs a detach that changed something.
fn detached(changed: bool, bot: BotId, agent: AgentId, reason: DetachReason) {
    if changed {
        info!(
            bot_id = %bot,
            agent_id = %agent,
            reason = reason.as_str(),
            "a bot was detached from its agent"
        );
    }
}

/// What a pass saw of each bot's agent (R37): the agent its row names, and
/// that agent's status as a bot's read shows it, with its stream's
/// generation. A change since the last pass changes what the bot reads as.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observed(BTreeMap<BotId, (Option<AgentId>, Option<AgentShown>)>);

/// An agent's status as a bot's read shows it, and its stream's generation.
type AgentShown = (AgentStatus, Option<u64>);

impl Observed {
    /// What `fleet` and the live streams `views` show at `now`.
    fn of(fleet: &Fleet, views: &[LiveView], now: DateTime<Utc>) -> Self {
        let streams: HashMap<AgentId, u64> = views
            .iter()
            .map(|view| (view.agent, view.generation))
            .collect();
        let shown = |agent: AgentId| {
            let generation = streams.get(&agent).copied();
            let shown = status(
                fleet.enabled_agents.contains(&agent),
                fleet.agent_expiry.get(&agent).copied(),
                generation.is_some(),
                now,
            );
            (shown, generation)
        };
        Self(
            fleet
                .bots
                .iter()
                .map(|record| {
                    let agent = record.bot.agent;
                    (record.bot.id, (agent, agent.map(shown)))
                })
                .collect(),
        )
    }

    /// The bots that show otherwise than in `before`, new ones included.
    fn changed_since<'a>(&'a self, before: &'a Self) -> impl Iterator<Item = BotId> + 'a {
        self.0
            .iter()
            .filter(|(bot, seen)| before.0.get(bot) != Some(seen))
            .map(|(bot, _)| *bot)
    }
}

/// Logs a write the next pass tries again.
fn failed(failure: &AssignError, bot: BotId, agent: AgentId, write: &'static str) {
    if failure.is_busy() {
        warn!(
            bot_id = %bot,
            agent_id = %agent,
            write,
            "the database was busy; the next pass tries again"
        );
    } else {
        error!(
            bot_id = %bot,
            agent_id = %agent,
            write,
            error = %chain(failure),
            "the reconciler couldn't write an assignment; the next pass tries again"
        );
    }
}

/// Logs a message that wasn't queued.
fn sent(agent: AgentId, result: Result<(), SendError>) {
    match result {
        Ok(()) => {}
        // The stream is gone or ending: its next hello resyncs.
        Err(SendError::NotConnected | SendError::QueueFull) => debug!(
            agent_id = %agent,
            "an agent's stream ended before the reconciler's message"
        ),
        Err(failure @ (SendError::TooLarge | SendError::Unwritable)) => error!(
            agent_id = %agent,
            error = %failure,
            "the reconciler's message for an agent can't be sent"
        ),
    }
}

/// Sleeps until `deadline`, or forever without one.
async fn sleep_until_some(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => core::future::pending().await,
    }
}

#[cfg(test)]
mod tests;
