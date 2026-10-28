//! An edited mode reaching a bot that's running it (Plan.md P11.4; ADR-0020,
//! S17): `PUT /modes/{id}` through fleet-client and serve's own wiring
//! hints the reconciler, whose next pass sends the agent that holds the bot
//! an `Update` with the edited steps. The bot runs them in the session it
//! has: the same session, no new `Assign` (P10.5), so the hold keeps its
//! `since` (PR #64's review). The steps must come within [`HINTED`], before
//! the periodic pass could bring them, so the case proves the hint's wiring
//! (PR #79's review).

use core::time::Duration;

use fleet_api_types::modes::ModeActionKind;
use fleet_core::mode::GameAction;
use fleet_server::grpc::registry::{HoldEntry, ViewPhase};
use fleet_testkit::mc::Performed;

use crate::harness::agent::RunOptions;
use crate::harness::logs::HINTED;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

#[tokio::test]
async fn an_edited_mode_reaches_its_held_bot_as_an_update_without_a_new_session() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    let bot = world.owner.bot("AfkBot1").await;
    let mode = world.owner.mode("Hopper", ModeActionKind::Jump).await;
    world.owner.set_mode(bot, mode.id).await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    let session = agent.fake().try_session(0).unwrap();
    let swung = |session: &fleet_testkit::mc::SessionController| {
        session
            .log()
            .iter()
            .filter(|performed| **performed == Performed::Action(GameAction::SwingArm))
            .count()
    };
    world
        .logs
        .until("the bot jumps", async || {
            session.log().contains(&Performed::Action(GameAction::Jump))
        })
        .await;
    assert_eq!(swung(&session), 0, "the first steps only jump");
    let held = || {
        world
            .server
            .registry()
            .placement_view()
            .iter()
            .find(|live| live.agent == agent.id)
            .and_then(|live| match &live.phase {
                ViewPhase::Reconciled(held) => held
                    .iter()
                    .find(|held| held.bot == bot)
                    .map(|held| held.entry.clone()),
                ViewPhase::Listed(_) => None,
            })
    };
    let since = held().map(|entry| entry.since);
    // A hold that started again after the edit would show a later `since`.
    world.server.clock().advance(Duration::from_secs(1));

    let edited = world.owner.edit_mode(&mode, ModeActionKind::SwingArm).await;
    let edited_steps_sent = |entry: &HoldEntry| {
        entry.sent.as_ref().is_some_and(|spec| {
            spec.mode
                .steps()
                .iter()
                .all(|step| step.action == fleet_core::mode::Action::SwingArm)
        })
    };
    let hinted = world
        .logs
        .within(HINTED, async || {
            held().is_some_and(|entry| edited_steps_sent(&entry))
        })
        .await;
    assert!(
        hinted,
        "the edited steps are sent: not within {HINTED:?}, so no hint started a pass\n{}",
        world.logs.buffer().text()
    );
    world
        .logs
        .until("the bot swings", async || swung(&session) > 0)
        .await;

    assert_eq!(edited.version, 2);
    assert!(since.is_some(), "the bot was held before the edit");
    assert_eq!(
        held().map(|entry| entry.since),
        since,
        "an Update keeps the hold; a new Assign would start it again"
    );
    assert_eq!(world.ledger.sessions(bot), 1, "the same session ran it");
    assert!(agent.fake().try_session(1).is_none(), "no second session");
    world.end(vec![agent]).await;
}
