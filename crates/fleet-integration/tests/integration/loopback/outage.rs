//! The server down, and sessions (P10.9; F's note; ADR-0019, S57, S63):
//!
//! - **A bot kicked while the server is down** rejoins with the grant its
//!   agent kept, since no stream can ask for one. Once its login is refused,
//!   its fresh request wipes that grant and fails at once without a stream,
//!   and the requests after its backoff find nothing kept either: it waits
//!   for the server, and connects only with the server's next grant.
//! - **A server restart:** the agents' bots get their sessions only once
//!   their streams are reconciled, never a denial first, though the
//!   restart's placement gate is still closed (S45): a restarted agent's
//!   bots are its own, not new placements.

use core::time::Duration;

use fleet_core::audit::AuditAction;
use fleet_core::disconnect::DisconnectReason;
use fleet_core::mc::SessionCredentials;
use fleet_core::resilience::RetryPolicy;
use rstest::rstest;

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// What the agent logs when a session request finds no stream.
const UNSENT: &str = "a session request couldn't be sent";
/// What the server logs for each grant.
const GRANTED: &str = "a session was granted";
/// What the server logs for each denial of its own.
const DENIED: &str = "a session request was denied";

/// Whether `message` is one of the server's session denials: its own, a
/// request for a bot the stream doesn't hold (yet), a full grant queue.
fn is_denial(message: &str) -> bool {
    message.starts_with("a session request") && message.contains("was denied")
}

#[tokio::test]
async fn a_bot_kicked_while_the_server_is_down_rejoins_with_its_kept_grant_and_a_fresh_request_waits()
 {
    let mut world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    // The bot's retries capped at 10 s: its attempt after the server is back
    // comes within it, never minutes later.
    agent.start(
        &world.ledger,
        RunOptions {
            retry: RetryPolicy::try_new(
                Duration::from_secs(5),
                Duration::from_secs(10),
                Duration::from_mins(5),
            )
            .unwrap(),
            ..RunOptions::capacity(2)
        },
    );
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    let id = bot.to_string();
    let lines_for = |message: &str| {
        world
            .logs
            .buffer()
            .lines_with(message)
            .unwrap()
            .into_iter()
            .filter(|line| line["bot_id"] == id.as_str())
            .count()
    };

    world.server.stop().await;
    // Kicked: its next session joins with the kept grant, while no server
    // runs; this one is held before it joins.
    world.ledger.hold(bot);
    world.ledger.end(
        bot,
        DisconnectReason::kicked(Some("multiplayer.disconnect.kicked"), "Kicked"),
    );
    world
        .logs
        .until("the kicked bot rejoined", async || {
            world.ledger.sessions(bot) == 2
        })
        .await;
    let rejoined = agent.fake().connects().last().unwrap().credentials.clone();
    // Its login is refused: its fresh request finds no stream and wipes the
    // kept grant, and the request after its backoff finds nothing kept.
    let unsent = lines_for(UNSENT);
    let granted = lines_for(GRANTED);
    world.ledger.end(bot, DisconnectReason::AuthRejected);
    world
        .logs
        .until(
            "two requests found no stream after the refusal",
            async || lines_for(UNSENT) >= unsent + 2,
        )
        .await;
    let while_down = world.ledger.sessions(bot);
    world.server.start_again().await;
    world.online(&agent, bot, 0).await;

    assert!(
        matches!(&rejoined, SessionCredentials::Offline { username } if username.as_str() == "AfkBot1"),
        "{rejoined:?}"
    );
    let asked_fresh = world
        .logs
        .buffer()
        .lines_with("the bot's state changed")
        .unwrap()
        .iter()
        .any(|line| {
            line["span"]["bot_id"] == id.as_str()
                && line["state"]
                    .as_str()
                    .is_some_and(|state| state.contains("fresh: true"))
        });
    assert!(asked_fresh, "{}", world.logs.buffer().text());
    assert_eq!(while_down, 2, "no connect without the server");
    // Back with the server's next grant, its third session.
    assert_eq!(lines_for(GRANTED), granted + 1);
    assert_eq!(world.ledger.sessions(bot), 3);
    world.end(vec![agent]).await;
}

#[tokio::test]
async fn bots_get_their_sessions_once_reconciled_after_a_server_restart() {
    let mut world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    // Enrolled, never back after the restart: the placement gate stays
    // closed throughout.
    let other = world.agent("agent-b").await;
    agent.start(&world.ledger, RunOptions::capacity(3));
    let bots = [
        world.owner.bot("AfkBot1").await,
        world.owner.bot("AfkBot2").await,
        world.owner.bot("AfkBot3").await,
    ];
    for bot in bots {
        world.owner.start(bot).await;
    }
    for bot in bots {
        world.online(&agent, bot, 0).await;
    }

    agent.stop().await;
    world.server.stop().await;
    let granted_before = world.logs.count(GRANTED);
    world.server.start_again().await;
    agent.start(&world.ledger, RunOptions::capacity(3));
    for bot in bots {
        world.online(&agent, bot, 0).await;
    }

    // Three new grants, each after the reconcile, and no denial since.
    assert_eq!(world.logs.count(GRANTED), granted_before + 3);
    assert_eq!(
        world.logs.count_where(is_denial),
        0,
        "{}",
        world.logs.buffer().text()
    );
    for bot in bots {
        assert_eq!(world.ledger.sessions(bot), 2, "one per run");
        assert_eq!(world.server.bot(bot).await.agent, Some(agent.id));
    }
    assert_eq!(
        world.server.entries(&AuditAction::BOT_ASSIGN).await.len(),
        3
    );
    world.end(vec![agent, other]).await;
}

#[rstest]
#[case::its_own(DENIED, true)]
#[case::not_held("a session request for a bot the stream doesn't hold was denied", true)]
#[case::queue_full("a session request was denied: the stream's grant queue is full", true)]
#[case::a_grant(GRANTED, false)]
#[case::unsent(UNSENT, false)]
fn every_denial_line_of_the_servers_counts(#[case] message: &str, #[case] denial: bool) {
    assert_eq!(is_denial(message), denial);
}
