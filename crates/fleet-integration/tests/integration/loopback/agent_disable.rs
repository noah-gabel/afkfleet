//! An agent disabled through the API (ADR-0020, R180): the real connected
//! agent's stream ends, the refusal stops its run and its bots with it, and
//! each bot waits on it, read Unknown, however many passes place other bots
//! (ADR-0019, S32). Only the release frees it, and it runs once more, on
//! another agent, never on two at once.

use fleet_api_types::bots::BotStatusKind;
use fleet_core::audit::AuditAction;

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

#[tokio::test]
async fn a_disabled_agents_bot_stops_and_waits_until_a_release_frees_it() {
    let world = World::new(ServerOptions::default()).await;
    let mut a = world.agent("agent-a").await;
    let mut b = world.agent("agent-b").await;
    a.start(&world.ledger, RunOptions::capacity(1));
    let held = world.owner.bot("AfkBot1").await;
    let other = world.owner.bot("AfkBot2").await;
    world.owner.start(held).await;
    world.online(&a, held, 0).await;
    b.start(&world.ledger, RunOptions::capacity(2));
    world.reconciled(&b, 0).await;

    let disabled = world.owner.disable_agent(a.id).await;
    a.refused().await;
    world.entries(&AuditAction::AGENT_DISCONNECT, 1).await;

    assert!(!disabled.enabled);
    assert_eq!(disabled.bots, 1);
    let ended = world.server.entries(&AuditAction::AGENT_DISCONNECT).await;
    assert_eq!(
        crate::loopback::reason(&ended[0]),
        Some("disabled".to_owned())
    );
    assert!(!world.ledger.live(held), "the refusal stopped the bot");

    // It stopped at the server's word that it's disabled, not at a retry its revoked
    // certificate lost.
    let refusals: Vec<_> = world
        .logs
        .buffer()
        .json_lines()
        .unwrap()
        .into_iter()
        .filter_map(|line| line.get("refusal").cloned())
        .collect();
    assert_ne!(refusals.len(), 0);
    assert!(
        refusals.iter().all(|refusal| refusal == "agent_disabled"),
        "{refusals:?}"
    );
    let connects = world.server.entries(&AuditAction::AGENT_CONNECT).await;
    assert!(
        connects
            .iter()
            .all(|entry| crate::loopback::reason(entry).as_deref() != Some("revoked")),
        "the agent tried again after it was disabled"
    );

    // The pass that places another bot on the other agent, which has room,
    // leaves the first where it is.
    world.owner.start(other).await;
    world.online(&b, other, 0).await;
    assert_eq!(world.server.bot(held).await.agent, Some(a.id));
    assert_eq!(world.ledger.sessions_on(held, "agent-b"), 0);
    let waiting = world.owner.shown(held).await;
    assert_eq!(waiting.status.kind, BotStatusKind::Unknown.into());

    let released = world.owner.release_agent(a.id).await;
    assert_eq!(released.bots, 0);
    world.online(&b, held, 0).await;

    assert_eq!(world.server.bot(held).await.agent, Some(b.id));
    assert_eq!(world.ledger.sessions_on(held, "agent-a"), 1);
    assert_eq!(world.ledger.sessions_on(held, "agent-b"), 1);
    assert_eq!(
        world
            .server
            .entries(&AuditAction::AGENT_DISABLE)
            .await
            .len(),
        1
    );
    assert_eq!(
        world
            .server
            .entries(&AuditAction::AGENT_RELEASE)
            .await
            .len(),
        1
    );
    world.end(vec![a, b]).await;
}
