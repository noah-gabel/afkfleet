//! Every game action, the respawn and a chat command have their effect on a
//! real server, checked through RCON (Plan.md P3.6, P3.9; ADR-0008 §8,
//! ADR-0011).
//!
//! The server keeps no state for an arm swing, so a second bot, the watcher,
//! counts the swing animations it receives, through a test-only system that
//! the `fault-injection` hook adds to its App. The same hook counts when the
//! bot tells the server it has loaded, which the respawn step waits for.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use azalea::app::Update;
use azalea::ecs::lifecycle::Add;
use azalea::ecs::message::MessageReader;
use azalea::ecs::observer::On;
use azalea::entity::HasClientLoaded;
use azalea::packet::game::ReceiveGamePacketEvent;
use azalea::protocol::packets::game::ClientboundGamePacket;
use azalea::protocol::packets::game::c_animate::AnimationAction;
use fleet_core::chat::ChatKind;
use fleet_core::id::BotId;
use fleet_core::mc::{SessionEvent, SessionHandle};
use fleet_core::mode::{GameAction, HotbarSlot};
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession};

use crate::checks::{data_floats, score, ticks_pass};
use crate::harness::{Mode, Server, bot, connect, is_chat, offline, wait_for, within};

const NAME: &str = "AfkBot1";
const WATCHER: &str = "AfkBot2";

/// The bot under test, its server, the swings its watcher has seen, and how
/// often the bot told the server it has loaded.
struct Scene {
    server: Server,
    session: McSession,
    events: McEvents,
    swings: Arc<AtomicUsize>,
    loads: Arc<AtomicUsize>,
}

impl Scene {
    async fn perform(&self, action: GameAction) {
        assert_eq!(
            within("an action", self.session.perform(action)).await,
            Ok(()),
            "{action:?}"
        );
    }

    async fn look(&self, yaw: f32, pitch: f32) {
        self.perform(GameAction::Look { yaw, pitch }).await;
    }

    async fn select(&self, slot: u8) {
        let slot = HotbarSlot::try_from(slot).unwrap();
        self.perform(GameAction::SelectHotbarSlot { slot }).await;
    }

    /// Waits until the bot's score for `objective` is at least 1.
    async fn scored(&self, what: &str, objective: &str) {
        self.server
            .eventually(
                what,
                &format!("scoreboard players get {NAME} {objective}"),
                |output| score(output) >= Some(1),
            )
            .await;
    }

    /// The health of the one entity `selector` names.
    async fn health(&self, selector: &str) -> Vec<f32> {
        data_floats(
            &self
                .server
                .rcon(&format!("data get entity {selector} Health"))
                .await,
        )
    }

    async fn died(&mut self, what: &str) {
        wait_for(&mut self.events, what, |event| *event == SessionEvent::Died).await;
    }
}

/// An App hook that counts in `loads` each time `tested` tells the server
/// it has loaded, and makes `watcher` count the main-hand swings it sees.
fn watch_the_scene(
    tested: BotId,
    loads: &Arc<AtomicUsize>,
    watcher: BotId,
    swings: &Arc<AtomicUsize>,
) -> impl Fn(BotId, &mut azalea::app::App) + Send + Sync + 'static {
    let loads = Arc::clone(loads);
    let swings = Arc::clone(swings);
    move |bot_id, app| {
        if bot_id == tested {
            // azalea adds `HasClientLoaded` when it sends `PlayerLoaded`, and
            // removes it on a respawn.
            let loads = Arc::clone(&loads);
            app.add_observer(move |_: On<Add, HasClientLoaded>| {
                loads.fetch_add(1, Ordering::SeqCst);
            });
        }
        if bot_id != watcher {
            return;
        }
        let swings = Arc::clone(&swings);
        app.add_systems(
            Update,
            move |mut packets: MessageReader<ReceiveGamePacketEvent>| {
                for received in packets.read() {
                    if let ClientboundGamePacket::Animate(animate) = &*received.packet
                        && animate.action == AnimationAction::SwingMainHand
                    {
                        swings.fetch_add(1, Ordering::SeqCst);
                    }
                }
            },
        );
    }
}

/// Look, then turn: the yaw wraps past 180, and the pitch stops at 90.
async fn look_and_turn(scene: &Scene) {
    scene.look(90.0, 30.0).await;
    scene.server.sees_rotation(NAME, 90.0, 30.0).await;
    scene
        .perform(GameAction::Turn {
            yaw: 100.0,
            pitch: 80.0,
        })
        .await;
    scene.server.sees_rotation(NAME, -170.0, 90.0).await;
}

async fn jump(scene: &Scene) {
    scene.perform(GameAction::Jump).await;
    scene.scored("a jump", "jumps").await;
}

/// Sneak, then stop. A test that fails answers nothing over RCON, so both
/// checks look for "Test passed": `if` the bot sneaks, then `unless` it does.
async fn sneak(scene: &Scene) {
    let sneaking = |condition: &str| {
        format!(
            "execute as {NAME} {condition} predicate {{condition:\"minecraft:entity_properties\",entity:\"this\",predicate:{{flags:{{is_sneaking:true}}}}}}"
        )
    };
    scene.perform(GameAction::Sneak { on: true }).await;
    scene
        .server
        .eventually("sneaking", &sneaking("if"), |output| {
            output == "Test passed"
        })
        .await;
    scene.perform(GameAction::Sneak { on: false }).await;
    scene
        .server
        .eventually("no longer sneaking", &sneaking("unless"), |output| {
            output == "Test passed"
        })
        .await;
}

/// Swing the arm: the watcher sees it.
async fn swing(scene: &Scene) {
    let before = scene.swings.load(Ordering::SeqCst);
    scene.perform(GameAction::SwingArm).await;
    within("the watcher seeing the swing", async {
        while scene.swings.load(Ordering::SeqCst) <= before {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// Select a slot, then use what it holds: a snowball, thrown at the sky
/// (looking at a block would make it a block click).
async fn select_and_use(scene: &Scene) {
    scene.select(3).await;
    scene
        .server
        .eventually(
            "slot 3 selected",
            &format!("data get entity {NAME} SelectedItemSlot"),
            |output| data_floats(output) == [3.0],
        )
        .await;
    scene
        .server
        .rcon(&format!(
            "item replace entity {NAME} hotbar.3 with minecraft:snowball 16"
        ))
        .await;
    scene.look(0.0, -60.0).await;
    ticks_pass(&scene.session, 3).await;
    scene.perform(GameAction::UseItem).await;
    scene.scored("a thrown snowball", "snowballs").await;
}

/// The bot's `bows` score: how many arrows it has shot. An unset score is 0.
async fn arrows_shot(scene: &Scene) -> i64 {
    let output = scene
        .server
        .rcon(&format!("scoreboard players get {NAME} bows"))
        .await;
    score(&output).unwrap_or(0)
}

/// Hold the use button with a bow, at the sky (Plan.md P3.9; ADR-0011).
///
/// First, hold and let go right away: the release cancels the use azalea
/// hasn't sent yet, so nothing is left drawn, and a later release shoots
/// nothing. Then hold for a full draw: nothing is shot while the bot holds,
/// one arrow when it lets go. Last, from another slot, select the bow's slot
/// and hold in the same tick: the hold still draws, and the release shoots
/// one more arrow. The bow is only the instrument.
///
/// azalea doesn't move projectiles between the server's position updates,
/// so for a while after a throw or a shot, the bot's own picture shows the
/// projectile right in front of it, and a use clicks that entity instead
/// (found in P3.9). So the step clears them first and runs the back-to-back
/// check before any arrow exists.
async fn hold_use(scene: &Scene) {
    for command in [
        "kill @e[type=minecraft:snowball]".to_owned(),
        format!("item replace entity {NAME} hotbar.4 with minecraft:bow"),
        format!("item replace entity {NAME} inventory.0 with minecraft:arrow 16"),
    ] {
        scene.server.rcon(&command).await;
    }
    scene.select(4).await;
    scene.look(0.0, -60.0).await;
    ticks_pass(&scene.session, 3).await;

    scene.perform(GameAction::HoldUse { on: true }).await;
    scene.perform(GameAction::HoldUse { on: false }).await;
    // A bow at full draw takes 20 ticks.
    ticks_pass(&scene.session, 25).await;
    // Had the hold survived, the bow would be drawn now, and this would
    // shoot it.
    scene.perform(GameAction::HoldUse { on: false }).await;
    ticks_pass(&scene.session, 10).await;
    assert_eq!(
        arrows_shot(scene).await,
        0,
        "on, then off right away, left the bow drawn"
    );

    scene.perform(GameAction::HoldUse { on: true }).await;
    ticks_pass(&scene.session, 25).await;
    assert_eq!(arrows_shot(scene).await, 0, "an arrow was shot while held");
    scene.perform(GameAction::HoldUse { on: false }).await;
    scene
        .server
        .eventually(
            "the arrow shot on the release",
            &format!("scoreboard players get {NAME} bows"),
            |output| score(output) == Some(1),
        )
        .await;
    scene
        .server
        .eventually(
            "the shot arrow",
            "execute if entity @e[type=minecraft:arrow]",
            // A selector's test also answers the count: "Test passed. Count: 1".
            |output| output.starts_with("Test passed"),
        )
        .await;
    scene.server.rcon("kill @e[type=minecraft:arrow]").await;

    // A slot change and a hold in the same tick, as an at-start "select a
    // slot, then hold" runs on every join. The server must see the new slot
    // before the use: a use with the old slot uses its item, and a slot change
    // after the use ends the hold.
    scene.select(0).await;
    scene
        .server
        .eventually(
            "slot 0 selected",
            &format!("data get entity {NAME} SelectedItemSlot"),
            |output| data_floats(output) == [0.0],
        )
        .await;
    // Also lets the arrow's removal reach the bot.
    ticks_pass(&scene.session, 3).await;
    scene.select(4).await;
    scene.perform(GameAction::HoldUse { on: true }).await;
    ticks_pass(&scene.session, 25).await;
    assert_eq!(
        arrows_shot(scene).await,
        1,
        "an arrow was shot while held after the slot change"
    );
    scene.perform(GameAction::HoldUse { on: false }).await;
    scene
        .server
        .eventually(
            "the arrow shot after a slot change and a hold in the same tick",
            &format!("scoreboard players get {NAME} bows"),
            |output| score(output) == Some(2),
        )
        .await;
    scene.server.rcon("kill @e[type=minecraft:arrow]").await;
}

/// Attack with an empty hand: a pig 6 blocks east is out of reach, one 2
/// blocks south isn't. The bot's target updates a tick after it looks.
async fn attack(scene: &Scene) {
    scene.select(0).await;
    for pig in [
        "0.5 -60 2.5 {NoAI:1b,Tags:[\"near\"]}",
        "6.5 -60 0.5 {NoAI:1b,Tags:[\"far\"]}",
    ] {
        scene
            .server
            .rcon(&format!("summon minecraft:pig {pig}"))
            .await;
    }
    for (yaw, pitch) in [(-90.0, 11.0), (0.0, 30.0)] {
        scene.look(yaw, pitch).await;
        ticks_pass(&scene.session, 3).await;
        scene.perform(GameAction::AttackFacingEntity).await;
    }
    scene
        .server
        .eventually(
            "the near pig hurt",
            "data get entity @e[type=minecraft:pig,tag=near,limit=1] Health",
            |output| matches!(data_floats(output).as_slice(), [health] if *health < 10.0),
        )
        .await;
    assert_eq!(
        scene.health("@e[type=minecraft:pig,tag=far,limit=1]").await,
        [10.0],
        "the pig out of reach was hit"
    );
    // The server accepted the hand-written attack packet: the bot is still
    // connected and acting.
    ticks_pass(&scene.session, 3).await;
    scene.perform(GameAction::Jump).await;
}

/// A chat command, echoed as the bot's emote.
async fn chat_command(scene: &mut Scene) {
    let sent = scene.session.send_chat("/me waves".parse().unwrap()).await;
    assert_eq!(sent, Ok(()));
    wait_for(&mut scene.events, "the emote", |event| {
        is_chat(event, ChatKind::Emote, Some(NAME), "waves")
    })
    .await;
}

/// Respawn: back to full health, and the next death is reported again.
async fn respawn(scene: &mut Scene) {
    scene.server.rcon(&format!("kill {NAME}")).await;
    scene.died("the first death").await;
    let loads = scene.loads.load(Ordering::SeqCst);
    assert_eq!(within("the respawn", scene.session.respawn()).await, Ok(()));
    // The server ignores damage to a respawned player until its client says
    // it has loaded, which azalea does only once the bot is in a loaded chunk
    // again. A kill before that is lost (found in group E: about 4 runs in
    // 10 failed here).
    within("the bot loading again after the respawn", async {
        while scene.loads.load(Ordering::SeqCst) <= loads {
            tokio::task::yield_now().await;
        }
    })
    .await;
    scene
        .server
        .eventually(
            "the respawn",
            &format!("data get entity {NAME} Health"),
            |output| data_floats(output) == [20.0],
        )
        .await;
    scene.server.rcon(&format!("kill {NAME}")).await;
    scene.died("the second death").await;
}

#[tokio::test]
async fn slow_actions_scenario() {
    let server = Server::start(Mode::Offline).await;
    let swings = Arc::new(AtomicUsize::new(0));
    let loads = Arc::new(AtomicUsize::new(0));
    let connector = AzaleaConnector::new(&McConfig::default()).with_app_hook(watch_the_scene(
        bot(1),
        &loads,
        bot(2),
        &swings,
    ));
    let (session, mut events) = connect(&connector, &server, bot(1), offline(NAME)).await;
    wait_for(&mut events, "the bot's join", |event| {
        *event == SessionEvent::Joined
    })
    .await;
    let (watcher, mut watcher_events) =
        connect(&connector, &server, bot(2), offline(WATCHER)).await;
    wait_for(&mut watcher_events, "the watcher's join", |event| {
        *event == SessionEvent::Joined
    })
    .await;
    // The bot faces south on open ground; the watcher stands behind it.
    server.rcon(&format!("tp {NAME} 0.5 -60 0.5 0 0")).await;
    server.rcon(&format!("tp {WATCHER} 0.5 -60 -2.5 0 0")).await;
    for objective in [
        "jumps minecraft.custom:minecraft.jump",
        "snowballs minecraft.used:minecraft.snowball",
        "bows minecraft.used:minecraft.bow",
    ] {
        server
            .rcon(&format!("scoreboard objectives add {objective}"))
            .await;
    }
    let mut scene = Scene {
        server,
        session,
        events,
        swings,
        loads,
    };

    look_and_turn(&scene).await;
    jump(&scene).await;
    sneak(&scene).await;
    swing(&scene).await;
    select_and_use(&scene).await;
    hold_use(&scene).await;
    attack(&scene).await;
    chat_command(&mut scene).await;
    respawn(&mut scene).await;

    within("the teardown", scene.session.disconnect()).await;
    within("the watcher's teardown", watcher.disconnect()).await;
    assert_eq!(connector.pool().live_threads(), 0);
}
