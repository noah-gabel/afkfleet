//! A server restart (P10.9; D's note): every agent resyncs, whichever comes
//! back first. Right after the start, nothing new is placed until every
//! enabled agent is back (S45), though the first one back has room; its
//! hello's bots are reconciled at once, never moved and never restarted.
//! Once both are back, the new bot goes to the first agent with room, in
//! enrollment order (S42).

use fleet_core::audit::AuditAction;
use rstest::rstest;

use crate::harness::agent::{Agent, RunOptions};
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// Which agent comes back first.
#[derive(Debug, Clone, Copy)]
enum First {
    /// The one enrolled first, by the server's order: the agent IDs'.
    Low,
    /// The other.
    High,
}

#[rstest]
#[case::the_first_enrolled_first(First::Low)]
#[case::the_other_first(First::High)]
#[tokio::test]
async fn agents_resync_after_a_server_restart_in_either_order(#[case] first: First) {
    let mut world = World::new(ServerOptions::default()).await;
    let x = world.agent("agent-x").await;
    let y = world.agent("agent-y").await;
    let (mut low, mut high) = if x.id < y.id { (x, y) } else { (y, x) };
    let one = world.owner.bot("AfkBot1").await;
    let two = world.owner.bot("AfkBot2").await;
    let three = world.owner.bot("AfkBot3").await;
    let four = world.owner.bot("AfkBot4").await;
    // The high agent alone takes the third bot; then the low one, placed
    // first, takes the other two. Each keeps room for one more.
    high.start(&world.ledger, RunOptions::capacity(2));
    world.reconciled(&high, 0).await;
    world.owner.start(three).await;
    world.online(&high, three, 0).await;
    low.start(&world.ledger, RunOptions::capacity(3));
    world.reconciled(&low, 0).await;
    world.owner.start(one).await;
    world.owner.start(two).await;
    world.online(&low, one, 0).await;
    world.online(&low, two, 0).await;

    low.proxy.close_gate();
    high.proxy.close_gate();
    world.server.restart().await;
    world.owner.start(four).await;
    let (back, later): (&Agent, &Agent) = match first {
        First::Low => (&low, &high),
        First::High => (&high, &low),
    };
    back.proxy.open_gate();
    world.reconciled(back, 0).await;
    // The first agent back is reconciled with exactly its own bots, and the
    // new bot waits for the other, though this one has room.
    let placed_early = world.server.bot(four).await.agent;
    later.proxy.open_gate();
    world.online(&low, four, 0).await;
    world.online(&low, one, 0).await;
    world.online(&low, two, 0).await;
    world.online(&high, three, 0).await;

    assert_eq!(
        placed_early, None,
        "nothing new placed before every agent was back"
    );
    assert_eq!(world.server.bot(four).await.agent, Some(low.id));
    for bot in [one, two, three] {
        assert_eq!(world.ledger.sessions(bot), 1, "never restarted");
    }
    assert_eq!(world.ledger.sessions(four), 1);
    // Bots 1–3 were assigned once, before the restart; only the new one
    // since.
    assert_eq!(
        world.server.entries(&AuditAction::BOT_ASSIGN).await.len(),
        4
    );
    assert_eq!(
        world.server.entries(&AuditAction::BOT_UNASSIGN).await.len(),
        0
    );
    assert_eq!(
        world.server.entries(&AuditAction::BOT_DETACH).await.len(),
        0
    );
    world.end(vec![low, high]).await;
}
