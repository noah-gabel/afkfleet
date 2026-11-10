//! A bot riding a minecart right-clicks as a player sitting there would, its
//! other actions work from the seat, and it's where the server has it during
//! the ride and after it, checked on a real server through RCON (ADR-0026).
//!
//! azalea 0.16.0 has no riding on the client side, so the scenario also reads
//! the bot's own picture: where it thinks it and its minecart are, what it
//! looks at, and the packets it sends and receives. A test-only system and
//! observer, added through the `fault-injection` hook, record them, and a
//! click that does nothing fails with that picture.
//!
//! The scene is a stone platform 20 blocks above the flat world's ground:
//! the world's own animals wander on the ground, and one that walks into the
//! minecart pushes it off its rail (found while writing this). On the
//! platform, the bot looking south:
//!
//! ```text
//!   z = -1.5   a villager
//!   z =  0     a rail with the minecart; a pig two blocks east of it
//!   z =  1     a chest sunk into the floor: seen from the seat, it's
//!              behind the minecart's own box
//!   z =  2     a chest on the floor
//! ```

use core::fmt::Write as _;
use core::ops::RangeInclusive;
use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use azalea::BlockPos;
use azalea::app::{App, Update};
use azalea::core::hit_result::HitResult;
use azalea::core::position::Vec3;
use azalea::ecs::message::MessageReader;
use azalea::ecs::observer::On;
use azalea::ecs::query::With;
use azalea::ecs::system::Query;
use azalea::entity::dimensions::EntityDimensions;
use azalea::entity::metadata::AbstractMinecart;
use azalea::entity::{LocalEntity, Position};
use azalea::interact::pick::HitResultComponent;
use azalea::packet::game::{ReceiveGamePacketEvent, SendGamePacketEvent};
use azalea::protocol::packets::game::{ClientboundGamePacket, ServerboundGamePacket};
use fleet_core::id::BotId;
use fleet_core::mc::{SessionEvent, SessionHandle};
use fleet_core::mode::{GameAction, HotbarSlot};
use fleet_core::net::TargetPolicy;
use fleet_mc::{AzaleaConnector, McConfig, McSession};

use crate::checks::{data_floats, score, ticks_pass};
use crate::harness::{Mode, Server, WITHIN, bot, connect, offline, wait_for, within};

const NAME: &str = "AfkBot1";
/// The height of the platform's top, where the bot's feet are on foot.
const FLOOR: i32 = -40;
/// The one minecart of the scene.
const CART: &str = "@e[type=minecraft:minecart,tag=cart,limit=1]";
/// How far a seated player's feet are above its minecart's position.
const SEAT: f64 = -0.4125;
/// How long a click's effect may take to show on the server. Shorter than
/// the harness's bound: a click that does nothing is this scenario's
/// subject, and it fails with the bot's picture.
const CLICK: Duration = Duration::from_secs(15);

/// The bot's own picture of the scene, as of its last frame, and the packets
/// it sent and received.
#[derive(Debug, Default)]
struct Picture {
    /// Where the bot thinks its feet are.
    bot: Option<Vec3>,
    /// How far above its feet azalea thinks its eyes are.
    eye_height: Option<f32>,
    /// Where the bot thinks the minecart is.
    cart: Option<Vec3>,
    /// What the bot thinks it looks at.
    hit: Option<HitResult>,
    sent: Vec<ServerboundGamePacket>,
    /// Only the packets that mount, move or dismount the bot.
    received: Vec<Arc<ClientboundGamePacket>>,
}

/// A place in a [`Picture`]'s packet lists.
#[derive(Debug, Clone, Copy)]
struct Mark {
    sent: usize,
    received: usize,
}

/// An App hook that records `tested`'s picture in `picture`.
fn watch(
    tested: BotId,
    picture: &Arc<Mutex<Picture>>,
) -> impl Fn(BotId, &mut App) + Send + Sync + 'static {
    let picture = Arc::clone(picture);
    move |bot_id, app| {
        if bot_id != tested {
            return;
        }
        let seen = Arc::clone(&picture);
        app.add_systems(
            Update,
            move |bots: Query<
                (&Position, &EntityDimensions, Option<&HitResultComponent>),
                With<LocalEntity>,
            >,
                  carts: Query<&Position, With<AbstractMinecart>>,
                  mut packets: MessageReader<ReceiveGamePacketEvent>| {
                let mut picture = seen.lock().unwrap();
                if let Some((position, dimensions, hit)) = bots.iter().next() {
                    picture.bot = Some(**position);
                    picture.eye_height = Some(dimensions.eye_height);
                    picture.hit = hit.map(|hit| (**hit).clone());
                }
                picture.cart = carts.iter().next().map(|position| **position);
                for received in packets.read() {
                    if matches!(
                        &*received.packet,
                        ClientboundGamePacket::SetPassengers(_)
                            | ClientboundGamePacket::PlayerPosition(_)
                            | ClientboundGamePacket::RemoveEntities(_)
                    ) {
                        picture.received.push(Arc::clone(&received.packet));
                    }
                }
            },
        );
        let seen = Arc::clone(&picture);
        app.add_observer(move |sent: On<SendGamePacketEvent>| {
            seen.lock().unwrap().sent.push(sent.event().packet.clone());
        });
    }
}

/// Prints the bot's picture and its last packets when the scenario fails,
/// whichever step and check it fails in.
struct Evidence(Arc<Mutex<Picture>>);

impl Drop for Evidence {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let picture = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        eprintln!("the bot's own position: {:?}", picture.bot);
        eprintln!("azalea's eye height:    {:?}", picture.eye_height);
        eprintln!("the bot's cart:         {:?}", picture.cart);
        eprintln!("the bot's target:       {:?}", picture.hit);
        eprintln!("the last packets that mounted, moved or dismounted it:");
        let skipped = picture.received.len().saturating_sub(8);
        for packet in picture.received.iter().skip(skipped) {
            eprintln!("  {packet:?}");
        }
        eprintln!("the last packets it sent:");
        let sent: Vec<_> = picture
            .sent
            .iter()
            .filter(|packet| {
                !matches!(
                    packet,
                    ServerboundGamePacket::KeepAlive(_)
                        | ServerboundGamePacket::ClientTickEnd(_)
                        | ServerboundGamePacket::ChunkBatchReceived(_)
                )
            })
            .collect();
        for packet in sent.iter().skip(sent.len().saturating_sub(24)) {
            eprintln!("  {packet:?}");
        }
    }
}

/// Whether two positions are within a hundredth of a block: the server's
/// come through RCON as rounded text.
fn close(a: Vec3, b: Vec3) -> bool {
    a.distance_squared_to(b) < 0.01 * 0.01
}

/// The bot under test, its server and its picture.
struct Scene {
    server: Server,
    session: McSession,
    picture: Arc<Mutex<Picture>>,
}

impl Scene {
    async fn perform(&self, action: GameAction) {
        assert_eq!(
            within("an action", self.session.perform(action)).await,
            Ok(()),
            "{action:?}"
        );
    }

    /// Looks at `yaw` and `pitch` and lets the bot's target catch up.
    async fn look(&self, yaw: f32, pitch: f32) {
        self.perform(GameAction::Look { yaw, pitch }).await;
        self.server.sees_rotation(NAME, yaw, pitch).await;
        ticks_pass(&self.session, 3).await;
    }

    async fn select(&self, slot: u8) {
        let slot = HotbarSlot::try_from(slot).unwrap();
        self.perform(GameAction::SelectHotbarSlot { slot }).await;
        self.server
            .eventually(
                "the slot selected",
                &format!("data get entity {NAME} SelectedItemSlot"),
                |output| data_floats(output) == [f32::from(slot.get())],
            )
            .await;
    }

    fn mark(&self) -> Mark {
        let picture = self.picture.lock().unwrap();
        Mark {
            sent: picture.sent.len(),
            received: picture.received.len(),
        }
    }

    /// The packets the bot sent since `since`.
    fn sent_since(&self, since: Mark) -> Vec<ServerboundGamePacket> {
        let picture = self.picture.lock().unwrap();
        picture.sent.iter().skip(since.sent).cloned().collect()
    }

    /// How many teleports the server sent the bot since `since`.
    fn teleports_since(&self, since: Mark) -> usize {
        let picture = self.picture.lock().unwrap();
        picture
            .received
            .iter()
            .skip(since.received)
            .filter(|packet| matches!(&***packet, ClientboundGamePacket::PlayerPosition(_)))
            .count()
    }

    /// Where the bot thinks its feet are.
    fn own_position(&self) -> Vec3 {
        self.picture
            .lock()
            .unwrap()
            .bot
            .expect("the bot has no position yet")
    }

    /// Where the server has the one entity `selector` names.
    async fn server_position(&self, selector: &str) -> Vec3 {
        let output = self
            .server
            .rcon(&format!("data get entity {selector} Pos"))
            .await;
        match data_floats(&output).as_slice() {
            [x, y, z] => Vec3::new(f64::from(*x), f64::from(*y), f64::from(*z)),
            _ => panic!("no position of {selector}: {output}"),
        }
    }

    /// Teleports the bot onto the platform at `x` and `z`, looking south,
    /// and waits until the teleport has reached it: it may stand there
    /// already, and a teleport that arrives later would undo a look.
    async fn is_moved_to(&self, what: &str, x: f64, z: f64) {
        let before = self.mark();
        self.server
            .rcon(&format!("tp {NAME} {x} {FLOOR} {z} 0 0"))
            .await;
        let wanted = Vec3::new(x, f64::from(FLOOR), z);
        within(what, async {
            while self.teleports_since(before) == 0 || !close(self.own_position(), wanted) {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    /// Waits until the bot's own position is the server's, reading both
    /// again each round: after a dismount, both still settle for a moment.
    async fn is_where_the_server_has_it(&self, what: &str) {
        let deadline = Instant::now() + WITHIN;
        loop {
            let server = self.server_position(NAME).await;
            let own = self.own_position();
            if close(own, server) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: the bot's own {own:?}, the server's {server:?}"
            );
        }
    }

    /// Waits until the server has the bot riding, or not.
    async fn server_has_it_riding(&self, what: &str, riding: bool) {
        self.server
            .eventually(
                what,
                &format!("data get entity {NAME} RootVehicle"),
                // On foot there's no such data, and the answer says so.
                |output| output.contains("has the following entity data") == riding,
            )
            .await;
    }

    /// The bot rides, and both the server and the bot itself have it at a
    /// seated player's place in the minecart.
    async fn is_seated(&self, what: &str) {
        self.server_has_it_riding(what, true).await;
        // Read again each round: a minecart that was just moved settles
        // over a tick or two, and its passenger with it.
        let deadline = Instant::now() + WITHIN;
        loop {
            let seat = self.server_position(CART).await.up(SEAT);
            let server = self.server_position(NAME).await;
            let own = self.own_position();
            if close(server, seat) && close(own, seat) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: the seat {seat:?}, the server's {server:?}, the bot's own {own:?}"
            );
        }
    }

    /// How many packets with its position the bot sent since `since`. A
    /// passenger sends none: a server ignores them.
    fn positions_sent_since(&self, since: Mark) -> usize {
        self.sent_since(since)
            .iter()
            .filter(|packet| {
                matches!(
                    packet,
                    ServerboundGamePacket::MovePlayerPos(_)
                        | ServerboundGamePacket::MovePlayerPosRot(_)
                )
            })
            .count()
    }

    /// The bot's score for `objective`; an unset score is 0.
    async fn score(&self, objective: &str) -> i64 {
        let output = self
            .server
            .rcon(&format!("scoreboard players get {NAME} {objective}"))
            .await;
        score(&output).unwrap_or(0)
    }

    /// Waits until the bot's score for `objective` is `wanted`. Fails with
    /// the bot's picture and its packets since `since` after [`CLICK`].
    async fn scores(&self, what: &str, objective: &str, wanted: i64, since: Mark) {
        self.scores_within(what, objective, wanted..=wanted, since)
            .await;
    }

    /// As [`Self::scores`], for a score within `wanted`.
    async fn scores_within(
        &self,
        what: &str,
        objective: &str,
        wanted: RangeInclusive<i64>,
        since: Mark,
    ) {
        let deadline = Instant::now() + CLICK;
        loop {
            let scored = self.score(objective).await;
            if wanted.contains(&scored) {
                return;
            }
            if Instant::now() >= deadline {
                let report = self.report(since).await;
                panic!("{what}: the bot's `{objective}` is {scored}, not {wanted:?}\n{report}");
            }
        }
    }

    /// The server's and the bot's picture, and the bot's packets since
    /// `since`: movement packets counted by kind, the others in full.
    async fn report(&self, since: Mark) -> String {
        let server_bot = self
            .server
            .rcon(&format!("data get entity {NAME} Pos"))
            .await;
        let server_cart = self
            .server
            .rcon(&format!("data get entity {CART} Pos"))
            .await;
        let picture = self.picture.lock().unwrap();
        let mut report = String::new();
        let _ = writeln!(report, "the server's bot:  {server_bot}");
        let _ = writeln!(report, "the server's cart: {server_cart}");
        let _ = writeln!(report, "the bot's own position: {:?}", picture.bot);
        let _ = writeln!(report, "azalea's eye height:    {:?}", picture.eye_height);
        let _ = writeln!(report, "the bot's cart:         {:?}", picture.cart);
        let _ = writeln!(report, "the bot's target:       {:?}", picture.hit);
        let _ = writeln!(report, "received since:");
        for packet in picture.received.iter().skip(since.received) {
            let _ = writeln!(report, "  {packet:?}");
        }
        let _ = writeln!(report, "sent since:");
        let mut moves = [0_usize; 4];
        let mut last_move = None;
        for packet in picture.sent.iter().skip(since.sent) {
            let kind = match packet {
                ServerboundGamePacket::MovePlayerPos(_) => 0,
                ServerboundGamePacket::MovePlayerPosRot(_) => 1,
                ServerboundGamePacket::MovePlayerRot(_) => 2,
                ServerboundGamePacket::MovePlayerStatusOnly(_) => 3,
                ServerboundGamePacket::KeepAlive(_)
                | ServerboundGamePacket::ClientTickEnd(_)
                | ServerboundGamePacket::ChunkBatchReceived(_) => continue,
                other => {
                    let _ = writeln!(report, "  {other:?}");
                    continue;
                }
            };
            moves[kind] += 1;
            last_move = Some(packet);
        }
        let _ = writeln!(
            report,
            "  movement packets: {} Pos, {} PosRot, {} Rot, {} StatusOnly; the last: {last_move:?}",
            moves[0], moves[1], moves[2], moves[3]
        );
        report
    }

    /// The click since `since` was on the block at `wanted`, and on no
    /// entity.
    fn clicked_the_block(&self, what: &str, wanted: BlockPos, since: Mark) {
        let sent = self.sent_since(since);
        assert!(
            sent.iter().any(|packet| matches!(
                packet,
                ServerboundGamePacket::UseItemOn(click) if click.block_hit.block_pos == wanted
            )),
            "{what}: no click on {wanted:?} in {sent:?}"
        );
        assert!(
            !sent
                .iter()
                .any(|packet| matches!(packet, ServerboundGamePacket::Interact(_))),
            "{what}: a click on an entity in {sent:?}"
        );
    }
}

/// On foot, the bot opens the chest two blocks south of it: the scenario's
/// way of checking a click, before any riding and after it.
async fn opens_the_chest_on_foot(scene: &Scene, opened: i64) {
    scene
        .is_moved_to("the bot's arrival at the chest", 0.5, 0.5)
        .await;
    // From eyes 1.62 above the floor to the chest's middle, 2 blocks south.
    scene.look(0.0, 29.0).await;
    let before = scene.mark();

    scene.perform(GameAction::UseItem).await;

    scene
        .scores("a click on foot", "chests", opened, before)
        .await;
    scene.clicked_the_block("a click on foot", BlockPos::new(0, FLOOR, 2), before);
}

/// Puts the bot into the minecart.
async fn mounts(scene: &Scene) {
    let output = scene
        .server
        .rcon(&format!("ride {NAME} mount {CART}"))
        .await;
    assert!(output.contains("started riding"), "{output}");
    scene.is_seated("the bot's mount").await;
}

/// Sitting in the minecart, the bot opens the chest on the floor, as a
/// player does: its line of sight passes above the minecart's box.
async fn opens_the_chest_from_the_minecart(scene: &Scene) {
    // From a seated player's eyes, 1.27 above the floor, to the chest's
    // middle.
    scene.look(0.0, 21.0).await;
    let before = scene.mark();

    scene.perform(GameAction::UseItem).await;

    scene
        .scores("a click from the minecart", "chests", 2, before)
        .await;
    scene.clicked_the_block(
        "a click from the minecart",
        BlockPos::new(0, FLOOR, 2),
        before,
    );
}

/// The bot opens the chest sunk into the floor in front of the minecart:
/// its line of sight passes through the minecart's own box, which a player
/// clicks through.
async fn opens_the_chest_behind_its_own_minecart(scene: &Scene) {
    // Steeply down: through the minecart's top and front onto the chest's lid.
    scene.look(0.0, 54.0).await;
    let before = scene.mark();

    scene.perform(GameAction::UseItem).await;

    scene
        .scores("a click through the own minecart", "chests", 3, before)
        .await;
    scene.clicked_the_block(
        "a click through the own minecart",
        BlockPos::new(0, FLOOR - 1, 1),
        before,
    );
}

/// The bot right-clicks the villager north of it: an entity it faces.
///
/// azalea sends an entity click as two identical packets, on foot too
/// ("most of the time vanilla really does send two identical interact
/// packets like this"), and the server counts a talk for each.
async fn talks_to_the_villager(scene: &Scene) {
    scene.look(180.0, 9.0).await;
    let before = scene.mark();

    scene.perform(GameAction::UseItem).await;

    scene
        .scores_within("a click on the villager", "talks", 1..=2, before)
        .await;
}

/// `HoldUse` draws a bow from the seat: nothing is shot while the bot
/// holds, one arrow when it lets go.
async fn draws_and_shoots_a_bow(scene: &Scene) {
    for command in [
        format!("item replace entity {NAME} hotbar.1 with minecraft:bow"),
        format!("item replace entity {NAME} inventory.0 with minecraft:arrow 16"),
    ] {
        scene.server.rcon(&command).await;
    }
    scene.select(1).await;
    scene.look(0.0, -60.0).await;
    let before = scene.mark();

    scene.perform(GameAction::HoldUse { on: true }).await;
    // A bow at full draw takes 20 ticks.
    ticks_pass(&scene.session, 25).await;
    let while_held = scene.score("bows").await;
    scene.perform(GameAction::HoldUse { on: false }).await;

    assert_eq!(while_held, 0, "an arrow was shot while held");
    scene
        .scores("the arrow shot on the release", "bows", 1, before)
        .await;
    // The bot's own picture shows a projectile right in front of it for a
    // while (found in P3.9), where the next click would land.
    scene.server.rcon("kill @e[type=minecraft:arrow]").await;
    ticks_pass(&scene.session, 3).await;
}

/// The bot's other actions from the seat: a turn, a swing, a thrown
/// snowball from another slot, an attack and a jump. None of them moves it
/// out of its seat, and it sends no position while it sits: a server
/// ignores a passenger's.
async fn acts_from_the_seat(scene: &Scene) {
    let seated = scene.mark();

    scene.look(0.0, -60.0).await;
    scene
        .perform(GameAction::Turn {
            yaw: 90.0,
            pitch: 10.0,
        })
        .await;
    scene.server.sees_rotation(NAME, 90.0, -50.0).await;

    let before = scene.mark();
    scene.perform(GameAction::SwingArm).await;
    within("the swing sent", async {
        while !scene
            .sent_since(before)
            .iter()
            .any(|packet| matches!(packet, ServerboundGamePacket::Swing(_)))
        {
            tokio::task::yield_now().await;
        }
    })
    .await;

    scene
        .server
        .rcon(&format!(
            "item replace entity {NAME} hotbar.2 with minecraft:snowball 16"
        ))
        .await;
    scene.select(2).await;
    scene.look(0.0, -60.0).await;
    let before = scene.mark();
    scene.perform(GameAction::UseItem).await;
    scene
        .scores("a snowball thrown from the seat", "snowballs", 1, before)
        .await;
    scene.server.rcon("kill @e[type=minecraft:snowball]").await;

    // The pig two blocks east, with an empty hand.
    scene.select(0).await;
    scene.look(-90.0, 27.0).await;
    scene.perform(GameAction::AttackFacingEntity).await;
    scene
        .server
        .eventually(
            "the pig hurt from the seat",
            "data get entity @e[type=minecraft:pig,tag=neighbour,limit=1] Health",
            |output| matches!(data_floats(output).as_slice(), [health] if *health < 10.0),
        )
        .await;

    scene.perform(GameAction::Jump).await;
    ticks_pass(&scene.session, 10).await;

    scene.is_seated("the bot after its actions").await;
    assert_eq!(scene.positions_sent_since(seated), 0);
}

/// The minecart moves with the bot in it, and back onto its rail: the bot's
/// own position follows its seat both times, and it sends its position only
/// in answer to the server's teleports.
async fn follows_its_minecart(scene: &Scene) {
    let before = scene.mark();

    scene
        .server
        .rcon(&format!("tp {CART} -3.5 {FLOOR} 0.5"))
        .await;
    scene.is_seated("the bot in its moved minecart").await;
    let moved = scene.own_position();
    scene
        .server
        .rcon(&format!("tp {CART} 0.5 {FLOOR} 0.5"))
        .await;
    scene.is_seated("the bot back on the rail").await;

    assert!((moved.x - -3.5).abs() < 0.01, "{moved:?}");
    // The server teleports a moved minecart's passenger with it, and the
    // answer to a teleport carries the position, as a vanilla client's does.
    assert_eq!(
        scene.positions_sent_since(before),
        scene.teleports_since(before),
        "{}",
        scene.report(before).await
    );
}

/// After a ride that ended since `since`, the bot is where the server has
/// it, on foot, and stays there: the server sent the dismount's one
/// teleport and never pulls it back.
async fn has_left(scene: &Scene, what: &str, since: Mark) {
    scene.server_has_it_riding(what, false).await;
    scene.is_where_the_server_has_it(what).await;

    ticks_pass(&scene.session, 40).await;

    let server = scene.server_position(NAME).await;
    let own = scene.own_position();
    assert!(
        close(own, server),
        "{what}: the bot's {own:?}, the server's {server:?}"
    );
    assert_eq!(scene.teleports_since(since), 1, "{what}: teleports");
}

/// The bot leaves the minecart as a player does, by sneaking.
async fn dismounts_by_sneaking(scene: &Scene) {
    let before = scene.mark();

    scene.perform(GameAction::Sneak { on: true }).await;

    has_left(scene, "the dismount", before).await;
    // A sneaking player gets out of any vehicle at once.
    scene.perform(GameAction::Sneak { on: false }).await;
    ticks_pass(&scene.session, 5).await;
}

/// The minecart breaks under the bot.
async fn loses_its_minecart(scene: &Scene) {
    let before = scene.mark();

    scene.server.rcon(&format!("kill {CART}")).await;

    has_left(scene, "the minecart's end", before).await;
}

#[tokio::test]
async fn slow_riding_scenario() {
    let server = Server::start(Mode::Offline).await;
    let picture = Arc::new(Mutex::new(Picture::default()));
    let _evidence = Evidence(Arc::clone(&picture));
    // The container is published on 127.0.0.1, which only the offline
    // scenario checks (ADR-0019, S69).
    let connector = AzaleaConnector::new(&McConfig::default())
        .with_targets(TargetPolicy::Unchecked)
        .with_app_hook(watch(bot(1), &picture));
    let (session, mut events) = connect(&connector, &server, bot(1), offline(NAME)).await;
    wait_for(&mut events, "the bot's join", |event| {
        *event == SessionEvent::Joined
    })
    .await;
    let below = FLOOR - 1;
    for command in [
        format!("fill -6 {below} -5 6 {below} 6 minecraft:stone"),
        format!("setblock 0 {FLOOR} 2 minecraft:chest"),
        format!("setblock 0 {below} 1 minecraft:chest"),
        "scoreboard objectives add chests minecraft.custom:minecraft.open_chest".to_owned(),
        "scoreboard objectives add talks minecraft.custom:minecraft.talked_to_villager".to_owned(),
        "scoreboard objectives add bows minecraft.used:minecraft.bow".to_owned(),
        "scoreboard objectives add snowballs minecraft.used:minecraft.snowball".to_owned(),
    ] {
        server.rcon(&command).await;
    }
    let scene = Scene {
        server,
        session,
        picture,
    };

    opens_the_chest_on_foot(&scene, 1).await;

    // The bot steps aside for the rail, the minecart and its neighbours.
    scene.is_moved_to("the bot's step aside", 3.5, 3.5).await;
    for command in [
        format!("setblock 0 {FLOOR} 0 minecraft:rail"),
        format!("summon minecraft:minecart 0.5 {FLOOR} 0.5 {{Tags:[\"cart\"]}}"),
        format!("summon minecraft:villager 0.5 {FLOOR} -1.5 {{NoAI:1b,Silent:1b}}"),
        format!("summon minecraft:pig 2.0 {FLOOR} 0.5 {{NoAI:1b,Silent:1b,Tags:[\"neighbour\"]}}"),
    ] {
        scene.server.rcon(&command).await;
    }

    mounts(&scene).await;
    opens_the_chest_from_the_minecart(&scene).await;
    opens_the_chest_behind_its_own_minecart(&scene).await;
    talks_to_the_villager(&scene).await;
    draws_and_shoots_a_bow(&scene).await;
    acts_from_the_seat(&scene).await;
    follows_its_minecart(&scene).await;
    dismounts_by_sneaking(&scene).await;
    mounts(&scene).await;
    loses_its_minecart(&scene).await;
    // The broken minecart's item would be picked up into the bot's hand.
    scene.server.rcon("kill @e[type=minecraft:item]").await;
    opens_the_chest_on_foot(&scene, 4).await;

    within("the teardown", scene.session.disconnect()).await;
    assert_eq!(connector.pool().live_threads(), 0);
}
