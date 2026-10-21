//! A mode's on-respawn steps through the whole path (Plan.md P11.4;
//! ADR-0020, S20–S24): created through fleet-client and serve's own wiring,
//! stored, sent to the agent in its bot's spec over the proto, and run by
//! the bot's runner after a respawn the (fake) server confirmed: never at
//! start, and after letting go.

use fleet_api_types::modes::ModeActionKind;
use fleet_core::mc::SessionEvent;
use fleet_core::mode::GameAction;
use fleet_testkit::mc::{EmitOutcome, Performed};

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

#[tokio::test]
async fn a_mode_from_the_api_runs_its_on_respawn_steps_after_a_confirmed_respawn() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    let bot = world.owner.bot("AfkBot1").await;
    let mode = world
        .owner
        .mode_on_respawn("Back again", ModeActionKind::Jump)
        .await;
    world.owner.set_mode(bot, mode.id).await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    let session = agent.fake().try_session(0).unwrap();
    let at_start = session.log();

    assert_eq!(session.emit(SessionEvent::Died), EmitOutcome::Queued);
    world
        .logs
        .until("the on-respawn jump", async || {
            session.log().contains(&Performed::Action(GameAction::Jump))
        })
        .await;

    assert_eq!(at_start, [], "nothing at start");
    assert_eq!(
        session.log(),
        [
            Performed::Respawn,
            Performed::Action(GameAction::HoldUse { on: false }),
            Performed::Action(GameAction::Sneak { on: false }),
            Performed::Action(GameAction::Jump),
        ]
    );
    world.end(vec![agent]).await;
}
