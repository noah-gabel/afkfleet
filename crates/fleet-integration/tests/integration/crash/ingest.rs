//! A kill in the middle of the ingest's batch (Plan.md P13.5; ADR-0019,
//! U16's crash case; ADR-0033). The ingest writes what agents report in one
//! unaudited transaction a tick: chat lines, and a bot's crossing into or
//! out of Paused or Failed.
//!
//! - **Killed right before a batch commits:** none of its lines is stored,
//!   never a part of them, and the ingest stores the next ones after the
//!   restart.
//! - **U16's crash case:** a bot is paused by a duplicate login (a human
//!   plays the account); once its pause is on disk the server is killed, so
//!   no stream's end is recorded, and the agent dies with its memory.
//!   Started again, both hold the bot Paused, and it never joins again by
//!   itself. The pause reaches the disk two ways, and the case runs both:
//!   while the agent's stream is up, in the ingest's batch (U16); while the
//!   agent is cut off, in its next hello's transaction (S40).

use fleet_core::bot::{BotState, LastState};
use fleet_core::chat::IncomingChat;
use fleet_core::disconnect::DisconnectReason;
use fleet_core::mc::SessionEvent;
use fleet_server::infra::sqlite::{self, DatabaseOptions};
use fleet_server::ports::store::Store as _;
use fleet_testkit::mc::EmitOutcome;

use super::child::Trip;
use super::{after_the_fault, assert_sound, serve_in_child};
use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

fn options() -> ServerOptions {
    ServerOptions {
        backups: true,
        ..ServerOptions::default()
    }
}

#[tokio::test]
async fn a_server_killed_before_an_ingest_batch_commits_stores_none_of_its_lines() {
    let mut world = World::new(options()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(2));
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    world.server.stop().await;
    let mut child = serve_in_child(
        &world,
        options(),
        Trip::Commit {
            table: "chat_messages".to_owned(),
            nth: 1,
            hold: false,
        },
        false,
    )
    .await;

    // The bot hears 50 lines; the agent sends them once its stream is
    // back, and the server's next batch holds them.
    for n in 0..50 {
        let heard = SessionEvent::Chat(IncomingChat::system(&format!("lost line {n}")));
        assert_eq!(world.ledger.emit(bot, heard), EmitOutcome::Queued);
    }
    child.says("TRIPPED").await;
    child.kill(&world);

    assert_sound(&world.server.config().database).await;
    world.server.start_again().await;
    assert_eq!(
        world.server.chat_lines(bot).await,
        0,
        "a part of the killed batch was stored"
    );
    // The ingest stores again once the agent is back.
    world.online(&agent, bot, 0).await;
    for n in 0..10 {
        let heard = SessionEvent::Chat(IncomingChat::system(&format!("line {n}")));
        assert_eq!(world.ledger.emit(bot, heard), EmitOutcome::Queued);
    }
    world
        .logs
        .until("the next lines are stored", async || {
            world.server.chat_lines(bot).await == 10
        })
        .await;
    assert_eq!(world.ledger.sessions(bot), 1, "the bot ran on through it");
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}

/// What the agent logs once a stream welcomed it.
const CONNECTED: &str = "the agent is connected";

/// What a bot's actor logs when it pauses.
const PAUSED: &str = "the bot paused; Resume connects it again";

#[rstest::rstest]
#[case::in_the_ingest_s_batch(true)]
#[case::in_the_next_hello(false)]
#[tokio::test]
async fn a_bot_paused_before_the_server_and_its_agent_both_die_stays_paused(
    #[case] while_connected: bool,
) {
    let mut world = World::new(options()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(2));
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    world.server.stop().await;
    // A human logs in with the account: the bot is kicked, and pauses.
    let human_logs_in = || {
        world.ledger.end(
            bot,
            DisconnectReason::kicked(
                Some("multiplayer.disconnect.duplicate_login"),
                "You logged in from another location",
            ),
        );
    };
    if !while_connected {
        // While the agent is cut off: its next hello carries the pause.
        human_logs_in();
        world
            .logs
            .until("the bot pauses", async || world.logs.count(PAUSED) == 1)
            .await;
    }
    // In a process of its own, for a kill that records no stream's end.
    let child = serve_in_child(&world, options(), Trip::Never, false).await;
    if while_connected {
        // On a welcomed stream: the ingest's next batch carries the pause.
        world
            .logs
            .until("the agent is back", async || {
                world.logs.count(CONNECTED) == 2
            })
            .await;
        human_logs_in();
    }
    // The batch with its crossing is on disk once another process reads it.
    let reader = sqlite::open_migrated(&world.server.config().database, DatabaseOptions::default())
        .await
        .unwrap();
    world
        .logs
        .until("the pause is stored", async || {
            let row = reader.bots().find(bot).await.unwrap().unwrap();
            row.last
                .is_some_and(|last| matches!(last.state, LastState::Paused(_)))
        })
        .await;
    reader.close().await;

    // Both die: the server by its kill, the agent with all it remembered.
    child.kill(&world);
    agent.crash(&world.ledger).await;

    assert_sound(&world.server.config().database).await;
    world.server.start_again().await;
    agent.start(&world.ledger, RunOptions::capacity(2));
    world
        .logs
        .until("the agent holds the bot paused", async || {
            world.held(&agent, bot).is_some_and(|(_, report)| {
                report.is_some_and(|report| matches!(report.state, BotState::Paused { .. }))
            })
        })
        .await;
    world.reconciled(&agent, 0).await;
    assert_eq!(
        world.ledger.sessions(bot),
        1,
        "a paused bot joined again by itself"
    );
    assert!(!world.ledger.live(bot));
    let row = world.server.bot(bot).await;
    assert_eq!(row.agent, Some(agent.id));
    assert!(
        row.last
            .is_some_and(|last| matches!(last.state, LastState::Paused(_))),
        "{row:?}"
    );
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}
