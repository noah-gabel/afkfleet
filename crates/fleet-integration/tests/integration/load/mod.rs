//! P13.4's load test (Plan.md P13.4, ADR-0032): 200 bots on one managed
//! agent and 20 Members with a live events socket each, on the loopback
//! harness: the real server, the real agent, the bots on the fake connector,
//! every user on fleet-client. It measures; it asserts only what must hold
//! under any load.
//!
//! Its name starts with `load_`, which the default and `ci` profiles leave
//! out: `just load` runs it alone and prints its report, the `LOAD` lines.
//!
//! In order: the fleet and its viewers, set up through the API; chat,
//! steady, in a burst and at rising rates, against the server's own counts,
//! with both metrics ports scraped at the middle rate; what the stored chat
//! takes on disk; a retention run while chat flows; every bot stopped and
//! started again with the sockets open; the agent's restart; the server's
//! and the agent's together. The process's CPU time and memory are read
//! after each.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleet_api_types::Open;
use fleet_api_types::accounts::GrantLevel;
use fleet_api_types::bots::{BotStateKind, BotStatusKind};
use fleet_api_types::events::EventMessageKind;
use fleet_client::{LiveEvents, LiveStop, LiveUpdate, MemoryStore, RefetchReason};
use fleet_core::chat::IncomingChat;
use fleet_core::id::BotId;
use fleet_core::mc::SessionEvent;
use fleet_server::app::events::{READ_AGAIN, RIGHTS_READ};
use fleet_testkit::mc::EmitOutcome;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

use crate::harness::agent::{Agent, RunOptions};
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

/// The test's own process, read from the OS: the server, the agent, every
/// client and the harness itself run in it, so its CPU time and memory are
/// an upper bound for the server's and the agent's together.
struct Meter {
    system: System,
    pid: Pid,
    /// The last reading: when, and the CPU time used until then.
    last: (Instant, Duration),
    /// The most memory any reading found, in bytes.
    peak: u64,
}

impl Meter {
    fn start() -> Self {
        let mut meter = Self {
            system: System::new().unwrap(),
            pid: sysinfo::get_current_pid().unwrap(),
            last: (Instant::now(), Duration::ZERO),
            peak: 0,
        };
        meter.last.1 = meter.read().0;
        meter
    }

    /// The CPU time the process used so far, and the memory it holds now.
    fn read(&mut self) -> (Duration, u64) {
        let what = ProcessRefreshKind::nothing().with_cpu().with_memory();
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::Some(&[self.pid]), true, what);
        let process = self.system.process(self.pid).expect("this process");
        let memory = process.memory();
        self.peak = self.peak.max(memory);
        (
            Duration::from_millis(process.accumulated_cpu_time()),
            memory,
        )
    }

    /// One report line for what `phase` used since the last one.
    fn lap(&mut self, phase: &str) {
        let (cpu, memory) = self.read();
        let wall = self.last.0.elapsed();
        let used = cpu.saturating_sub(self.last.1);
        self.last = (Instant::now(), cpu);
        println!(
            "LOAD process, {phase}: {} ms of CPU in {wall:.1?} ({:.0} % of one core); {} MiB held",
            used.as_millis(),
            100.0 * used.as_secs_f64() / wall.as_secs_f64().max(0.001),
            memory / (1024 * 1024),
        );
    }
}

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
    /// `bot` frames, on every socket together.
    bot_frames: AtomicUsize,
    /// What each socket's newest `bot` frame showed, by the socket's Member
    /// and the bot: what the status rests on, and the state. A bot whose
    /// agent is away keeps its last state, and its status is `unknown`.
    states: Mutex<HashMap<(usize, BotId), Shown>>,
}

/// A `bot` frame's status: what it rests on, and its state.
type Shown = (Option<BotStatusKind>, Option<BotStateKind>);

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

    /// How many of a Member's bots their socket last showed in `state`.
    fn shown(&self, state: BotStateKind) -> usize {
        self.states
            .lock()
            .unwrap()
            .values()
            .filter(|shown| shown.1 == Some(state))
            .count()
    }

    /// How many of a Member's bots their socket last showed online as their
    /// agent reports it, not as last known.
    fn reported_online(&self) -> usize {
        let online = (Some(BotStatusKind::Reported), Some(BotStateKind::Online));
        self.states
            .lock()
            .unwrap()
            .values()
            .filter(|shown| **shown == online)
            .count()
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

    /// Waits until no line arrived for `quiet`, or `bound` passed: for a
    /// phase that may lose lines, where no count says it's over.
    async fn settled(&self, quiet: Duration, bound: Duration) {
        let _ = tokio::time::timeout(bound, async {
            let mut last = (self.arrivals(), Instant::now());
            while last.1.elapsed() < quiet {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let now = self.arrivals();
                if now != last.0 {
                    last = (now, Instant::now());
                }
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

/// Member `member`'s socket, read until it's stopped.
async fn read(mut socket: LiveEvents<MemoryStore>, board: Arc<Board>, member: usize) {
    loop {
        // No timeout: the socket waits for the server's next frame, and its
        // stopper ends it.
        match socket.recv().await {
            Ok(LiveUpdate::Event(event)) => match event.kind.known() {
                Some(EventMessageKind::Chat) => {
                    if let Some(chat) = &event.line {
                        board.arrived(chat.text.as_str());
                    }
                }
                Some(EventMessageKind::Bot) => {
                    board.bot_frames.fetch_add(1, Ordering::Relaxed);
                    if let (Some(bot), Some(shown)) = (event.bot_id, &event.bot) {
                        let status = &shown.status;
                        board.states.lock().unwrap().insert(
                            (member, bot),
                            (
                                status.kind.known(),
                                status.state.as_ref().and_then(Open::known),
                            ),
                        );
                    }
                }
                _ => {}
            },
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

/// How the one agent runs: room for every bot and fifty more, its metrics
/// endpoint on.
fn running() -> RunOptions {
    RunOptions {
        metrics: true,
        ..RunOptions::capacity(BOTS + 50)
    }
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
        for (m, member) in members.iter().enumerate() {
            let socket = member.live_events();
            stops.push(socket.stopper());
            readers.spawn(read(socket, Arc::clone(&board), m));
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

/// How many of the log's lines say `message`, by a scan of its text. The
/// harness's own count parses every line as JSON, which takes seconds on
/// this case's log and holds the one thread meanwhile, so a read in flight
/// would look that much slower.
fn said(world: &World, message: &str) -> usize {
    world.logs.buffer().text().matches(message).count()
}

/// How long the rights' reads took that the log holds past its first
/// `skip` ones, sorted, from their own lines' `took_us`.
fn rights_reads(world: &World, skip: usize) -> Vec<Duration> {
    const FIELD: &str = "\"took_us\":";
    let text = world.logs.buffer().text();
    let mut took: Vec<Duration> = text
        .lines()
        .filter(|line| line.contains(RIGHTS_READ))
        .skip(skip)
        .filter_map(|line| {
            let digits = line.split_once(FIELD)?.1;
            let end = digits.find(|c: char| !c.is_ascii_digit())?;
            digits[..end].parse().ok()
        })
        .map(Duration::from_micros)
        .collect();
    took.sort_unstable();
    took
}

/// What many commits cost the open sockets: every bot stopped through the
/// API, then every bot started again, as fast as the API answers. Each
/// commit moves the count every socket judges its rights by, and each
/// changes a bot two Members see.
async fn every_bot_stopped_and_started(world: &World, board: &Board, bots: &[BotId]) {
    let pairs = BOTS * VIEWERS_A_BOT;
    for (what, state) in [
        ("stopped", BotStateKind::Stopped),
        ("started", BotStateKind::Online),
    ] {
        let reads = said(world, RIGHTS_READ);
        let again = said(world, READ_AGAIN);
        let frames = board.bot_frames.load(Ordering::Relaxed);
        let begun = Instant::now();
        for bot in bots {
            if state == BotStateKind::Stopped {
                world.owner.stop(*bot).await;
            } else {
                world.owner.start(*bot).await;
            }
        }
        let asked = begun.elapsed();
        world
            .logs
            .until(&format!("every viewer shown its bots {what}"), async || {
                board.shown(state) == pairs
            })
            .await;
        let shown = begun.elapsed();
        let took = rights_reads(world, reads);
        println!(
            "LOAD {BOTS} bots {what}: asked in {asked:?}, every viewer shown the last after {shown:?}; {} bot frames on the sockets",
            board.bot_frames.load(Ordering::Relaxed) - frames,
        );
        println!(
            "LOAD {BOTS} bots {what}: the rights were read {} times, p50 {:?}, max {:?}; {} reads done again",
            took.len(),
            percentile(&took, 50),
            took.last().copied().unwrap_or_default(),
            said(world, READ_AGAIN) - again,
        );
    }
}

/// Waits until `done`, looking every 10 ms, and fails with `what` after two
/// minutes: a wait that costs the same whatever the log holds.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let waited = tokio::time::timeout(Duration::from_mins(2), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{what}: not within two minutes");
}

/// The agent stops and starts again with every bot running, the sockets
/// open: how long until every bot plays again and every viewer is shown it,
/// and what the server's passes over 200 bots cost the sockets.
async fn the_agent_restarts(world: &World, board: &Board, bots: &[BotId], agent: &mut Agent) {
    let pairs = BOTS * VIEWERS_A_BOT;
    let before: Vec<usize> = bots.iter().map(|bot| world.ledger.sessions(*bot)).collect();
    let reads = said(world, RIGHTS_READ);
    let again = said(world, READ_AGAIN);
    let frames = board.bot_frames.load(Ordering::Relaxed);
    let begun = Instant::now();
    agent.stop().await;
    let stopped = begun.elapsed();
    until("every viewer shown that no bot is reported", || {
        board.reported_online() == 0
    })
    .await;
    let dark = begun.elapsed();
    agent.start(&world.ledger, running());
    until("every bot in a new session", || {
        bots.iter().zip(&before).all(|(bot, sessions)| {
            world.ledger.sessions(*bot) > *sessions && world.ledger.live(*bot)
        })
    })
    .await;
    let live = begun.elapsed();
    until("every viewer shown its bots online again", || {
        board.reported_online() == pairs
    })
    .await;
    let shown = begun.elapsed();
    let took = rights_reads(world, reads);
    println!(
        "LOAD the agent restarts: stopped after {stopped:?}, every viewer shown its bots as last known after {dark:?}; started again, every bot in a new session after {live:?}, every viewer shown it online after {shown:?}"
    );
    println!(
        "LOAD the agent restarts: {} bot frames on the sockets; the rights were read {} times, p50 {:?}, max {:?}; {} reads done again",
        board.bot_frames.load(Ordering::Relaxed) - frames,
        took.len(),
        percentile(&took, 50),
        took.last().copied().unwrap_or_default(),
        said(world, READ_AGAIN) - again,
    );
}

/// The server and the agent both restart, as a host's reboot does: no
/// stream, no socket and no session grant survives. How long until every
/// bot plays again, every socket is live again and every viewer is shown
/// its bots online, and how fast the new server granted the sessions.
async fn both_restart(world: &mut World, board: &Board, bots: &[BotId], agent: &mut Agent) {
    let pairs = BOTS * VIEWERS_A_BOT;
    let before: Vec<usize> = bots.iter().map(|bot| world.ledger.sessions(*bot)).collect();
    let ready = board.ready.load(Ordering::Relaxed);
    let disconnects = board.disconnects.load(Ordering::Relaxed);
    let begun = Instant::now();
    agent.stop().await;
    world.server.restart().await;
    let restarted = begun.elapsed();
    agent.start(&world.ledger, running());
    until("every bot in a new session", || {
        bots.iter().zip(&before).all(|(bot, sessions)| {
            world.ledger.sessions(*bot) > *sessions && world.ledger.live(*bot)
        })
    })
    .await;
    let live = begun.elapsed();
    let granted = world.server.series("afkfleet_session_grants_total");
    until("every socket live again", || {
        board.ready.load(Ordering::Relaxed) >= ready + MEMBERS
    })
    .await;
    let sockets = begun.elapsed();
    until("every viewer shown its bots online again", || {
        board.reported_online() == pairs
    })
    .await;
    let shown = begun.elapsed();
    println!(
        "LOAD both restart: the server back after {restarted:?}; every bot in a new session after {live:?} ({granted:.0} sessions granted by then, {:.0} a second since the agent's start); every socket live again after {sockets:?}, every viewer shown its bots online after {shown:?}; {} disconnects seen by the sockets",
        granted / live.saturating_sub(restarted).as_secs_f64().max(0.001),
        board.disconnects.load(Ordering::Relaxed) - disconnects,
    );
}

/// One `GET /metrics` on `port`, as a scraper sends it: how long the whole
/// answer took and how many bytes it was, or `None` when it wasn't a 200.
async fn scrape(port: SocketAddr) -> Option<(Duration, usize)> {
    let begun = Instant::now();
    let answered = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(port).await.ok()?;
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .ok()?;
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.ok()?;
        Some(answer)
    })
    .await
    .ok()??;
    answered
        .starts_with(b"HTTP/1.1 200")
        .then(|| (begun.elapsed(), answered.len()))
}

/// What a port's scrapes gave: whose port, every scrape's time, the largest
/// answer in bytes, and how many weren't answered with a 200.
type Scrapes = (&'static str, Vec<Duration>, usize, usize);

/// Scrapes `whose` `port` every quarter second until it's stopped.
async fn scrapes(whose: &'static str, port: SocketAddr, stop: Arc<AtomicUsize>) -> Scrapes {
    let (mut took, mut bytes, mut failed) = (Vec::new(), 0, 0);
    while stop.load(Ordering::Relaxed) == 0 {
        match scrape(port).await {
            Some((time, size)) => {
                took.push(time);
                bytes = bytes.max(size);
            }
            None => failed += 1,
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    took.sort_unstable();
    (whose, took, bytes, failed)
}

/// One report line for a port's scrapes.
fn scraped((whose, took, bytes, failed): &Scrapes) -> String {
    format!(
        "LOAD metrics, {whose}: {} scrapes at 1000 lines/s, p50 {:?}, max {:?}, at most {bytes} bytes; {failed} not answered",
        took.len(),
        percentile(took, 50),
        took.last().copied().unwrap_or_default(),
    )
}

/// A retention run while the bots hear 1000 lines a second: every bot holds
/// about twice the 100 lines it may keep by now, so the run deletes about a
/// hundred lines a bot, a thousand rows a batch, with its pause between.
/// What the run did and how long it took, and what its commits cost the
/// sockets; the chat's own report line shows what it cost the chat.
async fn retention_at_the_peak(world: &World, board: &Board, bots: &[BotId]) {
    let reads = said(world, RIGHTS_READ);
    let again = said(world, READ_AGAIN);
    let run = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let begun = Instant::now();
        let run = world.server.retention_run().await;
        (run, begun.elapsed())
    };
    let ((), (run, took)) = tokio::join!(sustained(world, board, bots, 1000, 6), run);
    let read = rights_reads(world, reads);
    println!(
        "LOAD a retention run at 1000 lines/s: {} lines past their bot's bound deleted in {} batches within {took:?}; {} phases failed; the rights were read {} times meanwhile, p50 {:?}; {} reads done again",
        run.deleted.chat_by_size,
        run.batches,
        run.failed.len(),
        read.len(),
        percentile(&read, 50),
        said(world, READ_AGAIN) - again,
    );
    assert!(run.failed.is_empty(), "the run's phases: {:?}", run.failed);
    assert!(run.deleted.chat_by_size > 0, "the run had lines to delete");
}

/// The server's count of chat rows its ingest wrote or skipped, and of
/// those it dropped before, from its own metrics: whole numbers.
fn ingest(world: &World) -> (usize, usize) {
    let count = |name| {
        let value = world.server.series(name);
        format!("{value:.0}").parse::<usize>().unwrap()
    };
    (
        count("afkfleet_ingest_rows_total"),
        count("afkfleet_ingest_chat_dropped_total"),
    )
}

/// Chat at `rate` lines a second for `seconds`, spread over every bot, a
/// tick every 10 ms: one report line with what arrived, how late, and what
/// the server says it stored and dropped. Whatever the rate, every line is
/// either counted as dropped or at each of its viewers, and no socket is
/// told to fetch everything again.
async fn sustained(world: &World, board: &Board, bots: &[BotId], rate: usize, seconds: usize) {
    let (stored, dropped) = ingest(world);
    let resyncs = board.resyncs.load(Ordering::Relaxed);
    let a_tick = rate / 100;
    let begun = Instant::now();
    let mut sent = 0;
    let mut refused = 0;
    for tick in 0..seconds * 100 {
        for _ in 0..a_tick {
            let text = format!("{rate} a second, line {sent}");
            board.sending(&text);
            let heard = SessionEvent::Chat(IncomingChat::system(&text));
            if world.ledger.emit(bots[sent % BOTS], heard) != EmitOutcome::Queued {
                refused += 1;
            }
            sent += 1;
        }
        // The next tick's own time, so a slow tick doesn't lower the rate.
        let next = Duration::from_millis(10) * u32::try_from(tick + 1).unwrap();
        tokio::time::sleep(next.saturating_sub(begun.elapsed())).await;
    }
    let emitted = begun.elapsed();
    board
        .settled(Duration::from_secs(2), Duration::from_secs(30))
        .await;
    let took = board.take();
    let (stored_now, dropped_now) = ingest(world);
    println!(
        "{}",
        latencies(
            &format!("{rate} lines/s for {seconds} s (sent in {emitted:.1?})"),
            sent * VIEWERS_A_BOT,
            &took
        )
    );
    println!(
        "LOAD {rate} lines/s: the server stored {} rows and dropped {}; {refused} lines a session didn't take; {} resyncs",
        stored_now - stored,
        dropped_now - dropped,
        board.resyncs.load(Ordering::Relaxed) - resyncs,
    );
    let (stored, dropped) = (stored_now - stored, dropped_now - dropped);
    assert_eq!(refused, 0, "{rate} lines/s: every session took its lines");
    assert_eq!(stored + dropped, sent, "{rate} lines/s: stored or counted");
    assert_eq!(
        took.len(),
        stored * VIEWERS_A_BOT,
        "{rate} lines/s: every stored line at each of its viewers"
    );
    assert_eq!(
        board.resyncs.load(Ordering::Relaxed),
        resyncs,
        "{rate} lines/s: no socket fell behind the bus"
    );
}

#[tokio::test]
async fn load_200_bots_and_20_sockets() {
    let mut world = World::new(ServerOptions {
        limits: Limits::LoadSetup,
        metrics: true,
        retention_by_hand: true,
        ..ServerOptions::default()
    })
    .await;
    let mut meter = Meter::start();
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, running());
    let bots = fleet(&world).await;
    meter.lap("200 bots created and started");
    let (rows_before, _) = ingest(&world);
    let bytes_before = world.server.database_bytes();
    let viewers = Viewers::join(&world, &bots).await;
    let board = Arc::clone(&viewers.board);
    meter.lap("20 Members joined, their sockets ready");
    tokio::time::sleep(Duration::from_secs(5)).await;
    meter.lap("idle, 200 bots online and 20 sockets open");

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
    meter.lap("20 lines/s for 10 s");
    // A burst: five lines on every bot at once.
    let burst = 5 * BOTS;
    let begun = Instant::now();
    let (burst_took, burst_refused) =
        chat(&world, &board, &bots, "burst", burst, Duration::ZERO).await;
    let burst_done = begun.elapsed();
    meter.lap("a burst of 1000 lines");
    // Rising rates, up to and past what one agent's stream may hold a tick.
    for rate in [500, 1000, 2000, 4000] {
        // Both metrics ports are scraped while the middle rate runs.
        let stop = Arc::new(AtomicUsize::new(0));
        let mut scrapers = JoinSet::new();
        if rate == 1000 {
            let (server, agent) = (world.server.metrics_port(), agent.metrics_port());
            scrapers.spawn(scrapes("the server's port", server, Arc::clone(&stop)));
            scrapers.spawn(scrapes("the agent's port", agent, Arc::clone(&stop)));
        }
        sustained(&world, &board, &bots, rate, 5).await;
        stop.store(1, Ordering::Relaxed);
        while let Some(done) = scrapers.join_next().await {
            let done = done.unwrap();
            assert_eq!(done.3, 0, "{}: every scrape answered", done.0);
            println!("{}", scraped(&done));
        }
        meter.lap(&format!("{rate} lines/s and its 2 s of quiet"));
    }
    // Before anything deletes: what the chat stored so far takes on disk.
    let rows = ingest(&world).0 - rows_before;
    let bytes = world.server.database_bytes().saturating_sub(bytes_before);
    println!(
        "LOAD chat on disk: {rows} lines of about 20 characters grew the database and its log by {} KiB, at most {} bytes a line (the log keeps its largest size, so what was moved into the database counts twice)",
        bytes / 1024,
        bytes / u64::try_from(rows.max(1)).unwrap(),
    );
    retention_at_the_peak(&world, &board, &bots).await;
    meter.lap("a retention run at 1000 lines/s");
    every_bot_stopped_and_started(&world, &board, &bots).await;
    meter.lap("200 bots stopped and started again");
    the_agent_restarts(&world, &board, &bots, &mut agent).await;
    meter.lap("the agent restarted with 200 bots");
    both_restart(&mut world, &board, &bots, &mut agent).await;
    meter.lap("the server and the agent restarted");
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
    println!(
        "LOAD process: at most {} MiB held at any reading",
        meter.peak / (1024 * 1024)
    );
    assert_eq!(steady_took.len(), steady * VIEWERS_A_BOT, "steady lines");
    assert_eq!(burst_took.len(), burst * VIEWERS_A_BOT, "burst lines");
    world.end(vec![agent]).await;
}
