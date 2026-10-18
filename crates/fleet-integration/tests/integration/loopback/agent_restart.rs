//! An agent restart (P10.9): no duplicate bots. A restarted agent gets its
//! own bots back from the server and runs each once; no other agent takes
//! them while it's away, and the server never moves them (ADR-0019, S32,
//! S39). A restart while the old connection still looks open to the
//! server, after a host crash, is refused `AlreadyConnected` until that
//! stream ends of silence, and then welcomed, with no identity conflict
//! recorded (group C's note for H, S26).

use fleet_core::audit::AuditAction;

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// What the agent logs when an attempt ends and it tries again.
const RECONNECTS: &str = "the control stream ended; the agent reconnects";

#[tokio::test]
async fn a_restarted_agent_runs_its_bots_once() {
    let world = World::new(ServerOptions::default()).await;
    let mut a = world.agent("agent-a").await;
    let mut b = world.agent("agent-b").await;
    a.start(&world.ledger, RunOptions::capacity(2));
    let one = world.owner.bot("AfkBot1").await;
    let two = world.owner.bot("AfkBot2").await;
    let three = world.owner.bot("AfkBot3").await;
    world.owner.start(one).await;
    world.owner.start(two).await;
    let first = world.online(&a, one, 0).await;
    world.online(&a, two, 0).await;
    // A is full, so B runs the third, with room to spare for A's two.
    b.start(&world.ledger, RunOptions::capacity(3));
    world.reconciled(&b, 0).await;
    world.owner.start(three).await;
    world.online(&b, three, 0).await;

    a.stop().await;
    world.entries(&AuditAction::AGENT_DISCONNECT, 1).await;
    a.start(&world.ledger, RunOptions::capacity(2));
    world.online(&a, one, first).await;
    world.online(&a, two, first).await;

    // Each of A's bots ran once per run of A, never on B, and the server
    // never moved them.
    for bot in [one, two] {
        assert_eq!(world.ledger.sessions_on(bot, "agent-a"), 2);
        assert_eq!(world.ledger.sessions_on(bot, "agent-b"), 0);
        assert_eq!(world.server.bot(bot).await.agent, Some(a.id));
    }
    assert_eq!(
        world.server.entries(&AuditAction::BOT_ASSIGN).await.len(),
        3
    );
    assert_eq!(
        world.server.entries(&AuditAction::BOT_UNASSIGN).await.len(),
        0
    );
    assert_eq!(
        world.server.entries(&AuditAction::BOT_DETACH).await.len(),
        0
    );
    assert_eq!(world.ledger.sessions(three), 1, "B's bot ran on");
    world.end(vec![a, b]).await;
}

#[tokio::test]
async fn a_restart_while_the_old_connection_lives_is_refused_until_it_ends_and_records_no_conflict()
{
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(2));
    let one = world.owner.bot("AfkBot1").await;
    let two = world.owner.bot("AfkBot2").await;
    world.owner.start(one).await;
    world.owner.start(two).await;
    let first = world.online(&agent, one, 0).await;
    world.online(&agent, two, 0).await;

    // The host dies; to the server, its connection stays open and silent.
    agent.crash(&world.ledger).await;
    agent.start(&world.ledger, RunOptions::capacity(2));
    let mut refused_while_live = false;
    world
        .logs
        .until("the new process refused AlreadyConnected", async || {
            let refused = world
                .logs
                .buffer()
                .lines_with(RECONNECTS)
                .unwrap()
                .iter()
                .any(|line| line["cause"] == "reject_already_connected");
            if refused {
                refused_while_live = world
                    .held(&agent, one)
                    .is_some_and(|(generation, _)| generation == first);
            }
            refused
        })
        .await;
    let again = world.online(&agent, one, first).await;
    world.online(&agent, two, first).await;

    assert!(refused_while_live, "the old stream still held the bots");
    // The slot frees before the end is recorded, so the new stream may come
    // first.
    world.entries(&AuditAction::AGENT_DISCONNECT, 1).await;
    // The old stream ended of silence, and only then was the new one
    // welcomed.
    let ended = world.server.entries(&AuditAction::AGENT_DISCONNECT).await;
    assert_eq!(ended.len(), 1);
    assert_eq!(
        crate::loopback::reason(&ended[0]),
        Some("heartbeat_timeout".to_owned())
    );
    assert!(again > first);
    assert_eq!(
        world
            .server
            .entries(&AuditAction::AGENT_CONNECT)
            .await
            .len(),
        2
    );
    assert_eq!(
        world
            .server
            .entries(&AuditAction::SECURITY_AGENT_IDENTITY_CONFLICT)
            .await
            .len(),
        0,
        "the old stream never spoke after the refusal"
    );
    // The crashed run's bots died with it; the new run runs each once.
    for bot in [one, two] {
        assert_eq!(world.ledger.sessions(bot), 2);
        assert!(world.ledger.live(bot));
    }
    world.end(vec![agent]).await;
}
