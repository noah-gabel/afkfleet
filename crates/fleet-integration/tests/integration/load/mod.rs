//! P13.4's load test (Plan.md P13.4): 200 bots on one managed agent and 20
//! Members with a live events socket each, on the loopback harness: the real
//! server, the real agent, the bots on the fake connector, every user on
//! fleet-client. It measures; it asserts only what must hold under any load.
//!
//! Its name starts with `load_`, which the default and `ci` profiles leave
//! out: `just load` runs it alone and prints its report, the `LOAD` lines.
//!
//! WORK IN PROGRESS: the first stage, the setup and chat's latency.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleet_api_types::accounts::GrantLevel;
use fleet_api_types::events::EventMessageKind;
use fleet_client::{LiveEvents, LiveStop, LiveUpdate, MemoryStore, RefetchReason};
use fleet_core::chat::IncomingChat;
use fleet_core::id::BotId;
use fleet_core::mc::SessionEvent;
use fleet_testkit::mc::EmitOutcome;
use tokio::task::JoinSet;

use crate::harness::agent::RunOptions;
use crate::harness::client::Member;
use crate::harness::server::{Limits, ServerOptions};
use crate::harness::world::World;

/// The bots, all on one agent.
const BOTS: usize = 200;
/// The Members, each with one socket.
const MEMBERS: usize = 20;
/// The bots each Member is given: 400 grants in all.
const GRANTS_EACH: usize = 20;
/// How many Members see each bot.
const VIEWERS_A_BOT: usize = MEMBERS * GRANTS_EACH / BOTS;

/// What the sockets saw, and when each line was sent.
#[derive(Debug, Default)]
struct Board {
    /// When each line was handed to its bot's session, by its text.
    sent: Mutex<HashMap<String, Instant>>,
    /// How long each line took to each socket that got it.
    took: Mutex<Vec<Duration>>,
    /// `ready` fetches: one a socket when nothing goes wrong.
    ready: AtomicUsize,
    /// Fetches the server asked for because a socket fell behind.
    resyncs: AtomicUsize,
    /// Fetches for a frame the client couldn't read.
    unreadable: AtomicUsize,
    /// Times a socket went down.
    disconnects: AtomicUsize,
}

impl Board {
    /// Notes that the line `text` goes out now.
    fn sending(&self, text: &str) {
        self.sent
            .lock()
            .unwrap()
            .insert(text.to_owned(), Instant::now());
    }

    /// Notes that the line `text` reached a socket now, if it's one of ours.
    fn arrived(&self, text: &str) {
        let now = Instant::now();
        let sent = self.sent.lock().unwrap().get(text).copied();
        if let Some(sent) = sent {
            self.took.lock().unwrap().push(now.duration_since(sent));
        }
    }

    /// How many lines reached a socket since the last [`Board::take`].
    fn arrivals(&self) -> usize {
        self.took.lock().unwrap().len()
    }

    /// Waits until `expected` lines arrived, or `bound` passed.
    async fn until(&self, expected: usize, bound: Duration) {
        let _ = tokio::time::timeout(bound, async {
            while self.arrivals() < expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
    }

    /// Takes the latencies gathered so far, sorted.
    fn take(&self) -> Vec<Duration> {
        let mut took = std::mem::take(&mut *self.took.lock().unwrap());
        took.sort_unstable();
        took
    }
}

/// One Member's socket, read until it's stopped.
async fn read(mut socket: LiveEvents<MemoryStore>, board: Arc<Board>) {
    loop {
        // No timeout: the socket waits for the server's next frame, and its
        // stopper ends it.
        match socket.recv().await {
            Ok(LiveUpdate::Event(event)) => {
                if event.kind.known() == Some(EventMessageKind::Chat)
                    && let Some(chat) = &event.line
                {
                    board.arrived(chat.text.as_str());
                }
            }
            Ok(LiveUpdate::Refetch(RefetchReason::Ready)) => {
                board.ready.fetch_add(1, Ordering::Relaxed);
            }
            Ok(LiveUpdate::Refetch(RefetchReason::Resync)) => {
                board.resyncs.fetch_add(1, Ordering::Relaxed);
            }
            Ok(LiveUpdate::Refetch(_)) => {
                board.unreadable.fetch_add(1, Ordering::Relaxed);
            }
            Ok(LiveUpdate::Disconnected { .. }) => {
                board.disconnects.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => return,
        }
    }
}

/// `sorted`'s value at `percent` of the way up.
fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    let last = sorted.len().saturating_sub(1);
    sorted
        .get((last * percent).div_ceil(100))
        .copied()
        .unwrap_or_default()
}

/// One report line for a phase's latencies.
fn latencies(phase: &str, expected: usize, sorted: &[Duration]) -> String {
    format!(
        "LOAD {phase}: {} of {expected} arrived; p50 {:?}, p95 {:?}, p99 {:?}, max {:?}",
        sorted.len(),
        percentile(sorted, 50),
        percentile(sorted, 95),
        percentile(sorted, 99),
        sorted.last().copied().unwrap_or_default(),
    )
}

/// The fleet: every bot its own offline account, all started on the one
/// agent and live.
async fn fleet(world: &World) -> Vec<BotId> {
    let begun = Instant::now();
    let mut bots = Vec::with_capacity(BOTS);
    for n in 0..BOTS {
        bots.push(world.owner.bot(&format!("AfkBot{n}")).await);
    }
    let created = begun.elapsed();
    let begun = Instant::now();
    for bot in &bots {
        world.owner.start(*bot).await;
    }
    let asked = begun.elapsed();
    world
        .logs
        .until("every bot's session live", async || {
            bots.iter().all(|bot| world.ledger.live(*bot))
        })
        .await;
    println!(
        "LOAD setup: {BOTS} bots created in {created:?}, their starts asked in {asked:?}, all live {:?} after the first",
        begun.elapsed()
    );
    bots
}

/// The viewers' sockets, read by tasks of their own.
struct Viewers {
    board: Arc<Board>,
    readers: JoinSet<()>,
    stops: Vec<LiveStop>,
}

impl Viewers {
    /// Twenty Members, each given twenty of `bots` so every bot has two of
    /// them, each with a socket that is ready.
    async fn join(world: &World, bots: &[BotId]) -> Self {
        let begun = Instant::now();
        let mut members = Vec::with_capacity(MEMBERS);
        for m in 0..MEMBERS {
            let member = Member::join(&world.server, &world.owner, &format!("member{m}")).await;
            for k in 0..GRANTS_EACH {
                let bot = bots[(m * (BOTS / MEMBERS) + k) % BOTS];
                let level = if k % 2 == 0 {
                    GrantLevel::View
                } else {
                    GrantLevel::Control
                };
                world.owner.grant(bot, &member.name, level).await;
            }
            members.push(member);
        }
        println!(
            "LOAD setup: {MEMBERS} Members with {} grants in {:?}",
            MEMBERS * GRANTS_EACH,
            begun.elapsed()
        );
        let board = Arc::new(Board::default());
        let mut readers = JoinSet::new();
        let mut stops = Vec::with_capacity(MEMBERS);
        for member in &members {
            let socket = member.live_events();
            stops.push(socket.stopper());
            readers.spawn(read(socket, Arc::clone(&board)));
        }
        world
            .logs
            .until("every socket ready", async || {
                board.ready.load(Ordering::Relaxed) >= MEMBERS
            })
            .await;
        Self {
            board,
            readers,
            stops,
        }
    }

    /// Stops every socket and waits for its reader.
    async fn leave(mut self) {
        for stop in &self.stops {
            stop.stop();
        }
        while self.readers.join_next().await.is_some() {}
    }
}

/// Has the bots hear `count` lines named after `phase`, a bot after the
/// other, `gap` apart, and waits for each to reach its viewers: the
/// latencies, sorted, and how many lines a session didn't take.
async fn chat(
    world: &World,
    board: &Board,
    bots: &[BotId],
    phase: &str,
    count: usize,
    gap: Duration,
) -> (Vec<Duration>, usize) {
    let mut refused = 0;
    for n in 0..count {
        let text = format!("{phase} line {n}");
        board.sending(&text);
        let heard = SessionEvent::Chat(IncomingChat::system(&text));
        if world.ledger.emit(bots[n % BOTS], heard) != EmitOutcome::Queued {
            refused += 1;
        }
        if !gap.is_zero() {
            tokio::time::sleep(gap).await;
        }
    }
    board
        .until(count * VIEWERS_A_BOT, Duration::from_secs(30))
        .await;
    (board.take(), refused)
}

#[tokio::test]
async fn load_200_bots_and_20_sockets() {
    let world = World::new(ServerOptions {
        limits: Limits::LoadSetup,
        ..ServerOptions::default()
    })
    .await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(BOTS + 50));
    let bots = fleet(&world).await;
    let viewers = Viewers::join(&world, &bots).await;
    let board = Arc::clone(&viewers.board);

    // Steady chat: twenty lines a second for ten seconds.
    let steady = 200;
    let (steady_took, steady_refused) = chat(
        &world,
        &board,
        &bots,
        "steady",
        steady,
        Duration::from_millis(50),
    )
    .await;
    // A burst: five lines on every bot at once.
    let burst = 5 * BOTS;
    let begun = Instant::now();
    let (burst_took, burst_refused) =
        chat(&world, &board, &bots, "burst", burst, Duration::ZERO).await;
    let burst_done = begun.elapsed();
    viewers.leave().await;

    println!(
        "{}",
        latencies(
            "steady chat, 20 lines/s",
            steady * VIEWERS_A_BOT,
            &steady_took
        )
    );
    println!(
        "{}",
        latencies("a burst of 1000 lines", burst * VIEWERS_A_BOT, &burst_took)
    );
    println!("LOAD burst: every line at its viewers {burst_done:?} after the first was heard");
    println!(
        "LOAD sockets: {} ready, {} resyncs, {} unreadable frames, {} disconnects; {} lines a session didn't take",
        board.ready.load(Ordering::Relaxed),
        board.resyncs.load(Ordering::Relaxed),
        board.unreadable.load(Ordering::Relaxed),
        board.disconnects.load(Ordering::Relaxed),
        steady_refused + burst_refused,
    );
    assert_eq!(steady_took.len(), steady * VIEWERS_A_BOT, "steady lines");
    assert_eq!(burst_took.len(), burst * VIEWERS_A_BOT, "burst lines");
    world.end(vec![agent]).await;
}
