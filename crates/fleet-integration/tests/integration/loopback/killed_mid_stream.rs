//! A connection killed mid-stream (P10.9): the agent's stream ends while
//! its bots run, the Owner stops one bot and starts another meanwhile, and
//! the agent comes back. The state reconciles: the running bot runs on
//! untouched, the stopped one stops and leaves the agent, the started one
//! joins it, and no account ever runs twice.

use fleet_core::audit::{AuditAction, AuditTarget, RecordedTarget};

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

#[tokio::test]
async fn a_connection_cut_mid_stream_reconciles_what_changed_meanwhile() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(4));
    let kept = world.owner.bot("AfkBot1").await;
    let stopped = world.owner.bot("AfkBot2").await;
    let started = world.owner.bot("AfkBot3").await;
    world.owner.start(kept).await;
    world.owner.start(stopped).await;
    let first = world.online(&agent, kept, 0).await;
    world.online(&agent, stopped, 0).await;

    // The connection dies, and stays down while the Owner changes things.
    agent.proxy.close_gate();
    agent.proxy.cut_open();
    world.entries(&AuditAction::AGENT_DISCONNECT, 1).await;
    world.owner.stop(stopped).await;
    world.owner.start(started).await;
    agent.proxy.open_gate();

    let again = world.online(&agent, started, first).await;
    world.entries(&AuditAction::BOT_UNASSIGN, 1).await;
    // Its Removed detaches it, after the unassignment.
    world.entries(&AuditAction::BOT_DETACH, 1).await;
    world
        .logs
        .until("the stopped bot left the agent", async || {
            !world.ledger.live(stopped) && world.held(&agent, stopped).is_none()
        })
        .await;

    // The kept bot ran on through it all: one session, on the new stream,
    // whose hello listed it, so the server never took it off the agent.
    assert!(again > first);
    assert_eq!(world.ledger.sessions(kept), 1);
    // Only the stopped bot left the agent, once its Removed came.
    let detached = world.server.entries(&AuditAction::BOT_DETACH).await;
    assert_eq!(detached.len(), 1, "{detached:#?}");
    assert_eq!(
        detached[0].target,
        Some(RecordedTarget::Known(AuditTarget::Bot(stopped)))
    );
    assert_eq!(
        crate::loopback::reason(&detached[0]),
        Some("removed".to_owned())
    );
    assert_eq!(
        world.server.entries(&AuditAction::BOT_ASSIGN).await.len(),
        3
    );
    assert!(world.ledger.live(kept));
    assert_eq!(
        world.held(&agent, kept).map(|(generation, _)| generation),
        Some(again)
    );
    // The stopped bot stopped, and the server took it off its agent.
    assert_eq!(world.ledger.sessions(stopped), 1);
    assert_eq!(world.server.bot(stopped).await.agent, None);
    let unassigned = world.server.entries(&AuditAction::BOT_UNASSIGN).await;
    assert_eq!(unassigned.len(), 1);
    // The started one joined it, once.
    assert_eq!(world.ledger.sessions(started), 1);
    assert_eq!(world.server.bot(started).await.agent, Some(agent.id));
    assert_eq!(
        world
            .server
            .entries(&AuditAction::AGENT_CONNECT)
            .await
            .len(),
        2
    );
    world.end(vec![agent]).await;
}
