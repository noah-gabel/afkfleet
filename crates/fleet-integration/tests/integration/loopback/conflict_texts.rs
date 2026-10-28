//! A server's conflict texts reaching a bot that's running there (Plan.md
//! P11.9; ADR-0020, S74, S76, S78): `POST /server-settings` through
//! fleet-client and serve's own wiring hints the reconciler, whose next
//! pass sends the agent that holds the bot an `Update` with the texts, in
//! the session it has. A kick with another text is transient and the bot
//! comes back; a kick with one of them pauses it as someone logging in with
//! its account, so it never kicks them back.

use fleet_core::bot::{BotState, PauseReason};
use fleet_core::disconnect::{ConflictKind, ConflictTexts, DisconnectReason};
use fleet_server::grpc::registry::ViewPhase;

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

#[tokio::test]
async fn a_servers_conflict_texts_reach_its_running_bot_and_a_listed_kick_pauses_it() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    // Pointed at `mc.example.com:25565`; the settings name it without the
    // default port, the same server (S74).
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    let generation = world.online(&agent, bot, 0).await;
    let sent_texts = || {
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
                    .and_then(|held| held.entry.sent.as_ref())
                    .map(|spec| spec.conflict_texts.clone()),
                ViewPhase::Listed(_) => None,
            })
    };
    let before = sent_texts();
    let expected = ConflictTexts::try_new(&["You are already connected to this proxy!"]).unwrap();

    world
        .owner
        .server_settings(
            "mc.example.com",
            &["You are already connected to this proxy!"],
        )
        .await;
    world
        .logs
        .until("the texts are sent", async || {
            sent_texts().as_ref() == Some(&expected)
        })
        .await;
    world
        .ledger
        .end(bot, DisconnectReason::kicked(None, "Server closed"));
    world
        .logs
        .until("the bot is back after an unlisted kick", async || {
            world.ledger.sessions(bot) == 2
                && world.held(&agent, bot).is_some_and(|(_, report)| {
                    report.is_some_and(|report| matches!(report.state, BotState::Online { .. }))
                })
        })
        .await;
    world.ledger.end(
        bot,
        DisconnectReason::kicked(None, "You are already connected to this proxy!"),
    );
    world
        .logs
        .until("the bot is paused by a listed kick", async || {
            world.held(&agent, bot).is_some_and(|(_, report)| {
                report.is_some_and(|report| {
                    report.state
                        == BotState::Paused {
                            reason: PauseReason::Conflict {
                                kind: ConflictKind::DuplicateLogin,
                            },
                        }
                })
            })
        })
        .await;

    assert_eq!(before, Some(ConflictTexts::default()), "none before");
    assert_eq!(
        world.held(&agent, bot).map(|(seen, _)| seen),
        Some(generation),
        "the same stream carried the Update"
    );
    assert_eq!(
        world.ledger.sessions(bot),
        2,
        "the listed kick ended the second session"
    );
    world.end(vec![agent]).await;
}
