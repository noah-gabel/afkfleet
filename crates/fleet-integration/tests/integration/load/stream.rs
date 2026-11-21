//! A scripted fleet on the real stream (Plan.md P15.20, the user's answer
//! to PR #127; ADR-0043): 200 bots on one managed agent, its stream to the
//! real server through the loopback harness, the bots' modes scripted
//! through the API. It measures how late the agent's own heartbeat comes
//! there, beside real scripts.
//!
//! **What's measured.** The agent sends a heartbeat when it has sent
//! nothing else for the server's interval, so the stream is never silent
//! longer than that interval while the agent keeps up: a silence longer
//! than it is how late the heartbeat came. The harness's proxy times each
//! write the agent's side makes ([`Proxy::watch_writes`]); TLS hides what
//! a write holds, and HTTP/2's own pings are moved out of the way. The
//! interval is 1 s, the least the server takes, for many silences a
//! phase; a heartbeat late by some milliseconds is that late at 10 s too.
//!
//! **Where it runs.** The server, the agent, its 200 bots and the harness
//! share one runtime of four workers, as many as GitHub's arm64 runner has
//! cores: the agent has fewer workers to itself than in production, so
//! what it measures is an upper bound.
//!
//! **The phases:** every bot on `afk`; on the reference's four examples;
//! a quarter of them on the hot script that spends as much of its time as
//! a script can (P15.19's); all of them on it. It asserts only what must
//! hold at any load: every bot stays online, and the stream never ends.
//!
//! [`Proxy::watch_writes`]: crate::harness::proxy::Proxy::watch_writes

use std::time::{Duration, Instant};

use fleet_agent::managed::link::LinkTimings;
use fleet_api_types::script::ModeScriptRequest;
use fleet_core::id::ModeId;
use fleet_core::script::ScriptDraft;
use fleet_script::EXAMPLES;
use fleet_server::config::Heartbeats;

use super::scripted::{calibrate, hot, seen};
use super::{BOTS, Meter, fleet, percentile};
use crate::harness::agent::{Agent, RunOptions};
use crate::harness::server::{Limits, ServerOptions};
use crate::harness::world::World;

/// The server's heartbeat interval, in seconds: the least it takes.
const INTERVAL_SECS: u32 = 1;

/// How long each phase is measured.
const MEASURE: Duration = Duration::from_secs(20);

/// The runner's line when a script stops on a limit or an error.
const STOPPED: &str = "the bot's script stopped; the bot stays online and idle";

/// The runner's line when a script starts.
const STARTED: &str = "the bot's script started";

/// The runner's line a hook.
const HOOK: &str = "a hook of the bot's script ran";

/// `draft` as the API's body: the same names, without the level, which the
/// server sets.
fn request(draft: ScriptDraft) -> ModeScriptRequest {
    let mut body = serde_json::to_value(draft).expect("a draft's JSON");
    body.as_object_mut().expect("an object").remove("api");
    serde_json::from_value(body).expect("the API's body")
}

/// The silences between `writes`, sorted.
fn silences(writes: &[Instant]) -> Vec<Duration> {
    let mut gaps: Vec<Duration> = writes
        .windows(2)
        .filter_map(|pair| match pair {
            [earlier, later] => Some(later.duration_since(*earlier)),
            _ => None,
        })
        .collect();
    gaps.sort_unstable();
    gaps
}

/// Measures the agent's stream for [`MEASURE`] and prints the phase's
/// line: its writes, its longest silence against the interval, and the
/// scripts that stopped meanwhile.
async fn measure(phase: &str, world: &World, agent: &Agent, meter: &mut Meter) {
    let interval = Duration::from_secs(u64::from(INTERVAL_SECS));
    let stopped = world.logs.count(STOPPED);
    let hooks = world.logs.count(HOOK);
    agent.proxy.watch_writes();
    tokio::time::sleep(MEASURE).await;
    let writes = agent.proxy.writes();
    let gaps = silences(&writes);
    let longest = gaps.last().copied().unwrap_or_default();
    let late: Vec<Duration> = gaps
        .iter()
        .filter_map(|gap| gap.checked_sub(interval))
        .collect();
    let heartbeat = match late.last() {
        Some(worst) => format!(
            "{} silences past it, the heartbeat late by {:?} at the median of those and {worst:?} at most",
            late.len(),
            percentile(&late, 50),
        ),
        None => "never silent past it".to_owned(),
    };
    println!(
        "LOAD stream {phase}: {} writes in {MEASURE:?}, the longest silence {longest:?} with a {interval:?} heartbeat: {heartbeat}; {} hooks ran, {} scripts stopped",
        writes.len(),
        world.logs.count(HOOK).saturating_sub(hooks),
        world.logs.count(STOPPED).saturating_sub(stopped),
    );
    meter.lap(phase);
}

/// Switches `bots` to `mode` through the API and waits until the runner
/// says `expected` scripts started in all.
async fn switch(world: &World, bots: &[fleet_core::id::BotId], mode: ModeId, expected: usize) {
    for bot in bots {
        world.owner.set_mode(*bot, mode).await;
    }
    world
        .logs
        .until("every switched bot's script started", async || {
            world.logs.count(STARTED) >= expected
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn load_scripted_bots_on_the_real_stream() {
    let rounds = tokio::task::spawn_blocking(calibrate)
        .await
        .expect("the calibration");
    let world = World::on_every_thread(ServerOptions {
        limits: Limits::LoadSetup,
        heartbeats: Heartbeats::try_from_secs(INTERVAL_SECS, 30).expect("heartbeats"),
        ..ServerOptions::default()
    })
    .await;
    let mut meter = Meter::start();
    let mut agent = world.agent("agent-a").await;
    agent.start(
        &world.ledger,
        RunOptions {
            // HTTP/2's pings out of the measured writes.
            timings: LinkTimings {
                keepalive_interval: Duration::from_secs(600),
                ..LinkTimings::default()
            },
            ..RunOptions::capacity(BOTS + 50)
        },
    );
    let bots = fleet(&world).await;
    // A farm as the examples expect one, so a script's first hook runs.
    for bot in &bots {
        world.ledger.show(*bot, seen());
    }
    let generation = world
        .held(&agent, bots[0])
        .map(|(generation, _)| generation);
    meter.lap("200 bots on afk, live");
    let mut examples = Vec::new();
    for (n, example) in EXAMPLES.iter().enumerate() {
        let mode = world
            .owner
            .scripted_mode(&format!("Example {n}"), request(example.draft()))
            .await;
        examples.push(mode.id);
    }
    let hot = world
        .owner
        .scripted_mode("Hot", request(hot(rounds)))
        .await
        .id;
    let quarter = BOTS / 4;

    measure("every bot on afk", &world, &agent, &mut meter).await;
    for (n, mode) in examples.iter().enumerate() {
        let on: Vec<_> = bots
            .iter()
            .copied()
            .skip(n)
            .step_by(examples.len())
            .collect();
        switch(&world, &on, *mode, 0).await;
    }
    switch(&world, &[], hot, BOTS).await;
    measure("every bot on the four examples", &world, &agent, &mut meter).await;
    switch(&world, &bots[..quarter], hot, BOTS + quarter).await;
    measure("a quarter of the bots hot", &world, &agent, &mut meter).await;
    switch(&world, &bots[quarter..], hot, 2 * BOTS).await;
    measure("every bot hot", &world, &agent, &mut meter).await;

    assert!(
        bots.iter().all(|bot| world.ledger.live(*bot)),
        "every bot stays online"
    );
    assert_eq!(
        world
            .held(&agent, bots[0])
            .map(|(generation, _)| generation),
        generation,
        "the stream never ended"
    );
}
