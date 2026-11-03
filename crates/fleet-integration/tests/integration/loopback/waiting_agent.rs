//! A bot whose agent goes away, as the Owner reads it (ADR-0020, S125,
//! U116): the live frame that follows the agent's end, and the GET it
//! equals, read the bot Unknown and name its agent with the agent's status,
//! which is what the app's "Agents" link shows on. Stopped by its operator,
//! the agent reads offline; disabled through the API, disabled.

use fleet_api_types::agents::AgentStatus;
use fleet_api_types::bots::{BotResponse, BotStatusKind};
use fleet_core::audit::AuditAction;

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// Whether `shown` waits on its agent, named in the answer with `status`.
fn waits_on(shown: &BotResponse, status: AgentStatus) -> bool {
    shown.status.kind == BotStatusKind::Unknown.into()
        && shown
            .agent
            .as_ref()
            .is_some_and(|agent| agent.status == status.into())
}

#[tokio::test]
async fn a_stopped_agents_bot_reads_unknown_and_names_its_agent_offline() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    let bot = world.owner.bot("AfkBot1").await;
    let live = world.owner.live();
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;

    agent.stop().await;
    world.entries(&AuditAction::AGENT_DISCONNECT, 1).await;

    let frame = world
        .live_settles(&live, bot, "the bot waiting on its agent", |shown| {
            shown.status.kind == BotStatusKind::Unknown.into()
        })
        .await;
    assert!(
        waits_on(&frame, AgentStatus::Offline),
        "the frame names its agent offline: {frame:#?}"
    );
    assert!(waits_on(
        &world.owner.shown(bot).await,
        AgentStatus::Offline
    ));
    live.close().await;
    world.end(vec![agent]).await;
}

#[tokio::test]
async fn a_disabled_agents_bot_reads_unknown_and_names_its_agent_disabled() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    let bot = world.owner.bot("AfkBot1").await;
    let live = world.owner.live();
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;

    world.owner.disable_agent(agent.id).await;
    agent.refused().await;
    world.entries(&AuditAction::AGENT_DISCONNECT, 1).await;

    let frame = world
        .live_settles(
            &live,
            bot,
            "the bot waiting on its disabled agent",
            |shown| shown.status.kind == BotStatusKind::Unknown.into(),
        )
        .await;
    assert!(
        waits_on(&frame, AgentStatus::Disabled),
        "the frame names its agent disabled: {frame:#?}"
    );
    assert!(waits_on(
        &world.owner.shown(bot).await,
        AgentStatus::Disabled
    ));
    live.close().await;
    world.end(vec![agent]).await;
}
