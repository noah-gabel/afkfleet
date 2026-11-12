//! A kill during a retention run (Plan.md P13.5; ADR-0020, S45; ADR-0033).
//! A run deletes in batches, each its own transaction; the server is killed
//! right before the second batch of one run commits. The first batch is
//! gone, whole, nothing of the second is, no line a bot keeps is touched,
//! and the next run finishes the job.

use fleet_core::chat::IncomingChat;
use fleet_core::id::BotId;
use fleet_core::mc::SessionEvent;
use fleet_core::retention::ChatBound;
use fleet_server::app::retention::BATCH;
use fleet_testkit::mc::EmitOutcome;

use super::child::Trip;
use super::{after_the_fault, assert_sound, serve_in_child};
use crate::harness::agent::RunOptions;
use crate::harness::server::{Limits, ServerOptions};
use crate::harness::world::World;

/// How many bots hear lines.
const BOTS: usize = 12;

/// How many lines each bot hears: 200 more than it keeps, so a run deletes
/// 2,400 lines, two full batches and a third.
const LINES_A_BOT: usize = 300;

/// How many lines each bot hears at a time: under the 64 events a session
/// queues, and together under what one agent's stream carries a tick, so
/// none is dropped.
const AT_A_TIME: usize = 50;

/// How many lines the database holds for `bots` together.
async fn stored(world: &World, bots: &[BotId]) -> usize {
    let mut lines = 0;
    for bot in bots {
        lines += world.server.chat_lines(*bot).await;
    }
    lines
}

/// Has each of `bots` hear [`LINES_A_BOT`] lines, and waits until every one
/// is stored.
async fn hear(world: &World, bots: &[BotId]) {
    let mut each = 0;
    while each < LINES_A_BOT {
        for bot in bots {
            for n in each..each + AT_A_TIME {
                let heard = SessionEvent::Chat(IncomingChat::system(&format!("line {n}")));
                assert_eq!(world.ledger.emit(*bot, heard), EmitOutcome::Queued);
            }
        }
        each += AT_A_TIME;
        world
            .logs
            .until(&format!("{each} lines a bot stored"), async || {
                stored(world, bots).await >= each * bots.len()
            })
            .await;
    }
    assert_eq!(stored(world, bots).await, LINES_A_BOT * bots.len());
}

#[tokio::test]
async fn a_server_killed_in_the_middle_of_a_retention_run_keeps_what_it_should_and_the_next_run_finishes()
 {
    let options = ServerOptions {
        retention_by_hand: true,
        backups: true,
        // Twelve bots made and started at once, as the load test's setup.
        limits: Limits::LoadSetup,
        ..ServerOptions::default()
    };
    let batch = usize::try_from(BATCH).unwrap();
    let kept = usize::try_from(ChatBound::MIN_LINES).unwrap();
    let mut world = World::new(options).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(BOTS));
    let mut bots = Vec::new();
    for n in 1..=BOTS {
        let bot = world.owner.bot(&format!("AfkBot{n}")).await;
        world.owner.start(bot).await;
        bots.push(bot);
    }
    for bot in &bots {
        world.online(&agent, *bot, 0).await;
    }
    hear(&world, &bots).await;
    world.server.stop().await;
    // The server starts one retention run once it listens, and stops right
    // before its second batch of chat commits.
    let mut child = serve_in_child(
        &world,
        options,
        Trip::Commit {
            table: "chat_messages".to_owned(),
            nth: 2,
            hold: false,
        },
        true,
    )
    .await;

    child.says("TRIPPED").await;
    child.kill(&world);

    assert_sound(&world.server.config().database).await;
    world.server.start_again().await;
    // One batch is gone, whole, and nothing of the second.
    assert_eq!(stored(&world, &bots).await, LINES_A_BOT * BOTS - batch);
    // The next run finishes the job, and the one after it has none left.
    world.server.retention_run().await;
    for bot in &bots {
        assert_eq!(world.server.chat_lines(*bot).await, kept);
    }
    world.server.retention_run().await;
    assert_eq!(stored(&world, &bots).await, kept * BOTS);
    // The newest lines are the ones each bot kept.
    let newest = world.owner.history(bots[0]).await;
    assert_eq!(
        newest.items.first().map(|line| line.text.as_str()),
        Some(format!("line {}", LINES_A_BOT - 1).as_str())
    );
    for bot in &bots {
        world.online(&agent, *bot, 0).await;
        assert_eq!(world.ledger.sessions(*bot), 1);
    }
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}
