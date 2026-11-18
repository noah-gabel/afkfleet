//! A scripted mode's inputs on a real server (Plan.md P15.15; ADR-0036,
//! D9 to D12, D18): walking to the leash's end and back, each way to let
//! go, sneaking at an edge, a dimension change as the world change, the
//! mouse with both guards at a cobblestone generator, the right button's
//! cadence counted in ticks, and mounting past an occupied minecart.
//!
//! The server is asked through RCON where the bot is and what it did; the
//! bot's own packets are recorded by a test-only observer, added through
//! the `fault-injection` hook, and printed when a scenario fails.
//!
//! Like the riding scenarios, everything stands on stone platforms 20
//! blocks above the flat world's ground, away from the world's animals.

use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use azalea::app::App;
use azalea::core::position::Vec3;
use azalea::ecs::observer::On;
use azalea::packet::game::SendGamePacketEvent;
use azalea::protocol::packets::game::ServerboundGamePacket;
use azalea::protocol::packets::game::s_player_action::Action as PlayerAction;
use fleet_core::id::BotId;
use fleet_core::mc::{
    Guards, LetGo, Observation, SessionEvent, SessionEvents as _, SessionHandle as _,
};
use fleet_core::mode::GameAction;
use fleet_core::net::TargetPolicy;
use fleet_core::script::{Input, Key, LeashRadius, MouseButton, ToolGuard};
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession};

use crate::checks::{data_floats, score, ticks_pass};
use crate::harness::{Mode, Server, WITHIN, bot, connect, offline, wait_for, within};

pub(crate) const NAME: &str = "AfkBot1";
/// The height of a platform's top, where the bot's feet are.
pub(crate) const FLOOR: i32 = -40;
/// The same height as a coordinate.
const FEET: f64 = -40.0;
/// The layer a platform is made of.
const BELOW: i32 = FLOOR - 1;
/// No event at all.
const NOTHING: [SessionEvent; 0] = [];

/// The packets the bot sent, in order.
#[derive(Debug, Default)]
pub(crate) struct Picture {
    sent: Vec<ServerboundGamePacket>,
}

/// An App hook that records the packets `tested` sends in `picture`.
pub(crate) fn watch(
    tested: BotId,
    picture: &Arc<Mutex<Picture>>,
) -> impl Fn(BotId, &mut App) + Send + Sync + 'static {
    let picture = Arc::clone(picture);
    move |bot_id, app| {
        if bot_id != tested {
            return;
        }
        let seen = Arc::clone(&picture);
        app.add_observer(move |sent: On<SendGamePacketEvent>| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sent
                .push(sent.event().packet.clone());
        });
    }
}

/// Prints the bot's last packets when the scenario fails, whichever step
/// and check it fails in.
struct Evidence(Arc<Mutex<Picture>>);

impl Drop for Evidence {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let picture = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        eprintln!("the last packets the bot sent:");
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
        for packet in sent.iter().skip(sent.len().saturating_sub(40)) {
            eprintln!("  {packet:?}");
        }
    }
}

pub(crate) const fn key(key: Key) -> Input {
    Input::Key(key)
}

pub(crate) const LEFT: Input = Input::Mouse(MouseButton::Left);
pub(crate) const RIGHT: Input = Input::Mouse(MouseButton::Right);

/// The bot under test, its server, its events and its packets.
pub(crate) struct Scene {
    pub(crate) server: Server,
    pub(crate) session: McSession,
    pub(crate) events: McEvents,
    pub(crate) picture: Arc<Mutex<Picture>>,
    _evidence: Evidence,
}

impl Scene {
    /// Starts a server with `spawn_protection` blocks of it (which a
    /// server applies only once it has an operator), and joins the bot.
    pub(crate) async fn start(spawn_protection: u8) -> Self {
        let picture = Arc::new(Mutex::new(Picture::default()));
        // The container is published on 127.0.0.1, which only the offline
        // scenario checks (ADR-0019, S69).
        let connector = AzaleaConnector::new(&McConfig::default())
            .with_targets(TargetPolicy::Unchecked)
            .with_app_hook(watch(bot(1), &picture));
        let server = Server::start_with(Mode::Offline, spawn_protection).await;
        let (session, mut events) = connect(&connector, &server, bot(1), offline(NAME)).await;
        wait_for(&mut events, "Joined", |event| {
            *event == SessionEvent::Joined
        })
        .await;
        Self {
            server,
            session,
            events,
            _evidence: Evidence(Arc::clone(&picture)),
            picture,
        }
    }

    pub(crate) async fn rcon(&self, commands: &[String]) {
        for command in commands {
            self.server.rcon(command).await;
        }
    }

    pub(crate) async fn perform(&self, action: GameAction) {
        assert_eq!(
            within("an action", self.session.perform(action)).await,
            Ok(()),
            "{action:?}"
        );
    }

    pub(crate) async fn press(&self, input: Input) {
        self.perform(GameAction::Press(input)).await;
    }

    pub(crate) async fn release(&self, input: Input) {
        self.perform(GameAction::Release(input)).await;
    }

    pub(crate) async fn guards(&self, leash: u8, tool_guard: ToolGuard) {
        let guards = Guards {
            leash: LeashRadius::try_from(leash).unwrap(),
            tool_guard,
        };
        assert_eq!(
            within("the guards", self.session.set_guards(guards)).await,
            Ok(())
        );
    }

    pub(crate) async fn observe(&self) -> Observation {
        within("a snapshot", self.session.observe())
            .await
            .expect("the bot is placed in its world")
    }

    /// Where the server has the bot.
    pub(crate) async fn position(&self) -> Vec3 {
        self.position_of(NAME).await
    }

    /// Where the server has the one entity `selector` names.
    pub(crate) async fn position_of(&self, selector: &str) -> Vec3 {
        let output = self
            .server
            .eventually(
                &format!("the position of {selector}"),
                &format!("data get entity {selector} Pos"),
                |output| data_floats_wide(output).len() == 3,
            )
            .await;
        let at = data_floats_wide(&output);
        Vec3::new(at[0], at[1], at[2])
    }

    /// Teleports the bot and waits until the bot itself is there and its
    /// leash is anchored there: a teleport of more than 8 blocks.
    pub(crate) async fn put_at(&self, at: Vec3, yaw: f32) {
        self.server
            .rcon(&format!("tp {NAME} {} {} {} {yaw} 0", at.x, at.y, at.z))
            .await;
        let deadline = Instant::now() + WITHIN;
        loop {
            if let Ok(seen) = within("a snapshot", self.session.observe()).await
                && seen
                    .anchor
                    .is_some_and(|anchor| near(place(anchor), at, 0.01))
                && near(place(seen.facts.bot().position), at, 0.01)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the bot wasn't put and anchored at {at:?}"
            );
            ticks_pass(&self.session, 1).await;
        }
    }

    /// Waits until the server has the bot at one place for a second of
    /// game ticks, and returns that place.
    pub(crate) async fn stands_still(&self, what: &str) -> Vec3 {
        let deadline = Instant::now() + WITHIN;
        let mut before = self.position().await;
        loop {
            ticks_pass(&self.session, 20).await;
            let now = self.position().await;
            if near(before, now, 0.001) {
                return now;
            }
            assert!(Instant::now() < deadline, "{what}: still moving at {now:?}");
            before = now;
        }
    }

    /// Waits until the server has the bot where `there` says.
    pub(crate) async fn gets(&self, what: &str, there: impl Fn(Vec3) -> bool) -> Vec3 {
        let deadline = Instant::now() + WITHIN;
        loop {
            let now = self.position().await;
            if there(now) {
                return now;
            }
            assert!(Instant::now() < deadline, "{what}: the bot is at {now:?}");
        }
    }

    /// Waits for the session to say it let go for `reason`.
    pub(crate) async fn lets_go(&mut self, reason: LetGo) {
        wait_for(&mut self.events, &format!("{reason:?}"), |event| {
            *event == SessionEvent::LetGo(reason)
        })
        .await;
    }

    /// The events the session has queued by now, chat left out. It waits a
    /// moment for each, so one on its way isn't missed.
    pub(crate) async fn reported(&mut self) -> Vec<SessionEvent> {
        let mut reported = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(200), self.events.next()).await
        {
            if !matches!(event, SessionEvent::Chat(_)) {
                reported.push(event);
            }
        }
        reported
    }

    /// The bot's score for `objective`; an unset score is 0.
    pub(crate) async fn score(&self, objective: &str) -> i64 {
        let output = self
            .server
            .rcon(&format!("scoreboard players get {NAME} {objective}"))
            .await;
        score(&output).unwrap_or(0)
    }

    /// How many packets the bot has sent so far: a mark for
    /// [`sent_since`](Self::sent_since).
    pub(crate) fn mark(&self) -> usize {
        self.picture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sent
            .len()
    }

    pub(crate) fn sent_since(&self, mark: usize) -> Vec<ServerboundGamePacket> {
        let picture = self.picture.lock().unwrap_or_else(PoisonError::into_inner);
        picture.sent.iter().skip(mark).cloned().collect()
    }

    /// The keys the bot last told the server it holds: forward, back, left,
    /// right, jump, sneak and sprint.
    pub(crate) fn keys_on_the_wire(&self) -> Option<[bool; 7]> {
        let picture = self.picture.lock().unwrap_or_else(PoisonError::into_inner);
        picture.sent.iter().rev().find_map(|packet| match packet {
            ServerboundGamePacket::PlayerInput(input) => Some([
                input.forward,
                input.backward,
                input.left,
                input.right,
                input.jump,
                input.shift,
                input.sprint,
            ]),
            _ => None,
        })
    }
}

/// A position of fleet-core's as azalea's.
pub(crate) fn place(at: fleet_core::script::Position) -> Vec3 {
    Vec3::new(at.x(), at.y(), at.z())
}

/// Whether two places are within `margin` blocks of each other.
pub(crate) fn near(a: Vec3, b: Vec3, margin: f64) -> bool {
    a.distance_squared_to(b) <= margin * margin
}

/// How far apart two places are on the ground: what the leash measures.
fn on_the_ground(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z)
}

/// The numbers of a `data get` answer at full width: a position's are
/// doubles, such as `… data: [0.5d, -40.0d, 8.76d]`.
pub(crate) fn data_floats_wide(output: &str) -> Vec<f64> {
    let Some((_, data)) = output.rsplit_once(": ") else {
        return Vec::new();
    };
    data.trim_matches(['[', ']'])
        .split(',')
        .filter_map(|number| number.trim().trim_end_matches(['f', 'd']).parse().ok())
        .collect()
}

/// Checks `data_floats` stays usable for the short numbers of a score.
#[test]
fn a_position_reads_at_full_width() {
    assert_eq!(
        data_floats_wide("AfkBot1 has the following entity data: [0.5d, -40.0d, 8.7654321d]"),
        [0.5, -40.0, 8.765_432_1]
    );
    assert_eq!(data_floats("x has the following entity data: 3"), [3.0]);
    assert!((FEET - f64::from(FLOOR)).abs() < f64::EPSILON);
}

/// The start of the walking strip: a platform 3 wide from here south.
const START: Vec3 = Vec3::new(0.5, FEET, 0.5);
/// Far from every platform, to make the next teleport a far one.
const AWAY: Vec3 = Vec3::new(40.5, FEET, -40.5);

/// The bot walks to the end of its leash and no further, walks back and
/// along it freely, lets go in each way, stops at an edge while it sneaks,
/// and comes out of a dimension change with nothing held and a new anchor.
#[tokio::test(flavor = "multi_thread")]
async fn slow_walking_scenario() {
    let mut scene = Scene::start(0).await;
    scene
        .rcon(&[
            // The strip, a pad to stand on far away, and a small pad with
            // an edge well inside a leash.
            format!("fill -1 {BELOW} -2 1 {BELOW} 24 minecraft:stone"),
            format!("fill 39 {BELOW} -42 41 {BELOW} -39 minecraft:stone"),
            format!("fill 9 {BELOW} 0 11 {BELOW} 3 minecraft:stone"),
            "scoreboard objectives add jumps minecraft.custom:minecraft.jump".to_owned(),
            "scoreboard objectives add sneaking minecraft.custom:minecraft.sneak_time".to_owned(),
        ])
        .await;

    the_leash_ends_a_walk_and_lets_the_bot_back(&mut scene).await;
    each_release_ends_a_walk(&mut scene).await;
    sneaking_stops_at_an_edge(&mut scene).await;
    a_dimension_change_is_a_world_change(&mut scene).await;

    within("disconnect", scene.session.disconnect()).await;
}

/// Puts the bot at the strip's start, looking south along it, anchored
/// there.
async fn at_the_start(scene: &Scene) {
    scene.put_at(AWAY, 0.0).await;
    scene.put_at(START, 0.0).await;
}

async fn the_leash_ends_a_walk_and_lets_the_bot_back(scene: &mut Scene) {
    at_the_start(scene).await;
    let seen = scene.observe().await;
    assert_eq!(seen.facts.bot().dimension.as_str(), "minecraft:overworld");
    assert_eq!(seen.facts.bot().riding, None);
    assert_eq!(scene.reported().await, NOTHING);

    // Walking: 8 blocks and the tick that passes them.
    scene.press(key(Key::Forward)).await;
    scene.lets_go(LetGo::Leash).await;
    let end = scene.stands_still("at the leash's end").await;
    let out = on_the_ground(START, end);
    assert!(out > 8.0 && out < 9.0, "{end:?}, {out} blocks out");
    assert!(
        near(Vec3::new(START.x, START.y, end.z), end, 0.05),
        "{end:?}"
    );
    assert_eq!(scene.keys_on_the_wire(), Some([false; 7]));
    // The bot's own picture is the server's.
    let seen = scene.observe().await;
    assert!(near(place(seen.facts.bot().position), end, 0.05));
    assert!(near(place(seen.anchor.unwrap()), START, 0.01));

    // Back, and along the edge: nothing is let go of.
    scene.press(key(Key::Back)).await;
    scene.gets("walking back", |at| at.z < 5.0).await;
    scene.release(key(Key::Back)).await;
    let back = scene.stands_still("after walking back").await;
    assert!(back.z > 3.0 && back.z < 5.0, "{back:?}");
    assert_eq!(scene.reported().await, NOTHING);

    // Out again: the same end.
    scene.press(key(Key::Forward)).await;
    scene.lets_go(LetGo::Leash).await;
    let again = scene.stands_still("at the leash's end again").await;
    assert!(
        on_the_ground(START, again) > 8.0 && on_the_ground(START, again) < 9.0,
        "{again:?}"
    );

    // Sprinting gets no further than a tick's worth more.
    at_the_start(scene).await;
    scene.press(key(Key::Sprint)).await;
    scene.press(key(Key::Forward)).await;
    scene.lets_go(LetGo::Leash).await;
    let sprinted = scene.stands_still("at the leash's end, sprinting").await;
    let out = on_the_ground(START, sprinted);
    assert!(out > 8.0 && out < 9.5, "{sprinted:?}, {out} blocks out");
    scene.perform(GameAction::ReleaseAll).await;

    // A mode's own radius.
    scene.guards(3, ToolGuard::DEFAULT).await;
    at_the_start(scene).await;
    scene.press(key(Key::Forward)).await;
    scene.lets_go(LetGo::Leash).await;
    let short = scene.stands_still("at the end of a leash of 3").await;
    let out = on_the_ground(START, short);
    assert!(out > 3.0 && out < 4.0, "{short:?}, {out} blocks out");
    scene.guards(8, ToolGuard::DEFAULT).await;
    let _ = scene.reported().await;
}

async fn each_release_ends_a_walk(scene: &mut Scene) {
    for (name, release) in [
        (
            "the key's own release",
            GameAction::Release(key(Key::Forward)),
        ),
        ("releasing the keys", GameAction::ReleaseKeys),
        ("releasing everything", GameAction::ReleaseAll),
    ] {
        at_the_start(scene).await;
        scene.press(key(Key::Forward)).await;
        scene.gets(name, |at| at.z > 2.0).await;
        scene.perform(release).await;
        let stopped = scene.stands_still(name).await;
        assert!(stopped.z > 2.0 && stopped.z < 6.0, "{name}: {stopped:?}");
        assert_eq!(scene.keys_on_the_wire(), Some([false; 7]), "{name}");
    }

    // A tap is one tick of the key: a step's worth, and no more.
    at_the_start(scene).await;
    scene.perform(GameAction::Tap(key(Key::Forward))).await;
    ticks_pass(&scene.session, 5).await;
    let stepped = scene.stands_still("after a tap").await;
    assert!(
        stepped.z > START.z && stepped.z < START.z + 0.5,
        "{stepped:?}"
    );

    // A tapped jump is one jump; a held one jumps on.
    let before = scene.score("jumps").await;
    scene.perform(GameAction::Tap(key(Key::Jump))).await;
    scene
        .server
        .eventually(
            "one jump",
            &format!("scoreboard players get {NAME} jumps"),
            |output| score(output) == Some(before + 1),
        )
        .await;
    ticks_pass(&scene.session, 30).await;
    assert_eq!(scene.score("jumps").await, before + 1);
    scene.press(key(Key::Jump)).await;
    scene
        .server
        .eventually(
            "jumping on",
            &format!("scoreboard players get {NAME} jumps"),
            |output| score(output).is_some_and(|jumps| jumps >= before + 3),
        )
        .await;
    scene.release(key(Key::Jump)).await;
    let landed = scene.stands_still("after jumping").await;
    assert!(near(Vec3::new(landed.x, START.y, landed.z), landed, 0.001));

    // A held sneak is the server's sneak, until its release.
    let before = scene.score("sneaking").await;
    scene.press(key(Key::Sneak)).await;
    scene
        .server
        .eventually(
            "sneaking",
            &format!("scoreboard players get {NAME} sneaking"),
            |output| score(output).is_some_and(|ticks| ticks > before + 10),
        )
        .await;
    scene.release(key(Key::Sneak)).await;
    ticks_pass(&scene.session, 10).await;
    let after = scene.score("sneaking").await;
    ticks_pass(&scene.session, 20).await;
    assert_eq!(scene.score("sneaking").await, after);
    assert_eq!(scene.reported().await, NOTHING);
}

async fn sneaking_stops_at_an_edge(scene: &mut Scene) {
    // The pad's blocks end at z = 4; a sneaking player overhangs it by
    // less than its own half width, 0.3.
    let pad = Vec3::new(10.5, FEET, 1.5);
    scene.put_at(AWAY, 0.0).await;
    scene.put_at(pad, 0.0).await;

    scene.press(key(Key::Sneak)).await;
    scene.press(key(Key::Forward)).await;
    scene.gets("the edge", |at| at.z > 3.9).await;
    let edge = scene.stands_still("sneaking at the edge").await;
    assert!(
        near(Vec3::new(edge.x, pad.y, edge.z), edge, 0.001),
        "{edge:?}"
    );
    assert!(edge.z > 3.9 && edge.z < 4.3, "{edge:?}");
    assert_eq!(scene.reported().await, NOTHING);

    // Without the sneak key the same walk goes over the edge.
    scene.release(key(Key::Sneak)).await;
    let fell = scene.gets("the fall", |at| at.y < pad.y - 1.0).await;
    assert!(fell.z > 4.0, "{fell:?}");
    scene.perform(GameAction::ReleaseAll).await;
    scene.stands_still("on the ground").await;
    let _ = scene.reported().await;
}

async fn a_dimension_change_is_a_world_change(scene: &mut Scene) {
    let nether = Vec3::new(0.5, 100.0, 0.5);
    scene
        .rcon(&[
            "execute in minecraft:the_nether run forceload add 0 0".to_owned(),
            "execute in minecraft:the_nether run fill -2 99 -2 2 99 2 minecraft:stone".to_owned(),
            "execute in minecraft:the_nether run fill -2 100 -2 2 103 2 minecraft:air".to_owned(),
        ])
        .await;
    at_the_start(scene).await;
    scene.press(key(Key::Sneak)).await;
    scene.press(LEFT).await;
    scene.press(RIGHT).await;
    ticks_pass(&scene.session, 5).await;
    assert_eq!(
        scene.keys_on_the_wire().map(|keys| keys[5]),
        Some(true),
        "the sneak key is down"
    );
    let _ = scene.reported().await;

    scene
        .server
        .rcon(&format!(
            "execute in minecraft:the_nether run tp {NAME} {} {} {}",
            nether.x, nether.y, nether.z
        ))
        .await;
    wait_for(&mut scene.events, "the world change", |event| {
        *event == SessionEvent::WorldChanged
    })
    .await;

    // Placed in the new world, with nothing held and a new anchor.
    let seen = scene.observe().await;
    assert_eq!(seen.facts.bot().dimension.as_str(), "minecraft:the_nether");
    assert!(near(place(seen.anchor.unwrap()), nether, 0.01), "{seen:?}");
    assert!(near(place(seen.facts.bot().position), nether, 0.01));
    ticks_pass(&scene.session, 5).await;
    assert_eq!(scene.keys_on_the_wire(), Some([false; 7]));
    let mark = scene.mark();
    ticks_pass(&scene.session, 12).await;
    assert!(
        !scene.sent_since(mark).iter().any(|packet| matches!(
            packet,
            ServerboundGamePacket::UseItem(_)
                | ServerboundGamePacket::UseItemOn(_)
                | ServerboundGamePacket::PlayerAction(_)
        )),
        "a button is still down: {:?}",
        scene.sent_since(mark)
    );

    // And back: a world change again, once.
    scene
        .server
        .rcon(&format!(
            "execute in minecraft:overworld run tp {NAME} {} {} {}",
            START.x, START.y, START.z
        ))
        .await;
    wait_for(&mut scene.events, "the world change back", |event| {
        *event == SessionEvent::WorldChanged
    })
    .await;
    let seen = scene.observe().await;
    assert_eq!(seen.facts.bot().dimension.as_str(), "minecraft:overworld");
    assert!(near(place(seen.anchor.unwrap()), START, 0.01));
    assert_eq!(scene.reported().await, NOTHING);
}

// --- The mouse, mounting and a break the server refuses ---

/// The generator's platform: the bot stands at its west side and looks east
/// at the place where cobblestone forms.
const MINER: Vec3 = Vec3::new(20.5, FEET, 2.5);
/// The block where the generator's cobblestone forms, level with the
/// bot's feet.
const COBBLE: (i32, i32, i32) = (22, FLOOR, 2);
/// The block the miner stands on.
const UNDER_THE_MINER: (i32, i32, i32) = (20, BELOW, 2);

/// A place as text for a command: three whole numbers.
fn at((x, y, z): (i32, i32, i32)) -> String {
    format!("{x} {y} {z}")
}

/// The middle of a block.
fn middle((x, y, z): (i32, i32, i32)) -> fleet_core::script::Position {
    fleet_core::script::Position::try_new(
        f64::from(x) + 0.5,
        f64::from(y) + 0.5,
        f64::from(z) + 0.5,
    )
    .unwrap()
}

impl Scene {
    async fn look_at(&self, block: (i32, i32, i32)) {
        self.perform(GameAction::LookAtPosition(middle(block)))
            .await;
        ticks_pass(&self.session, 3).await;
    }

    /// Whether the server has `kind` at `block`.
    async fn block_is(&self, block: (i32, i32, i32), kind: &str) -> bool {
        self.server
            .rcon(&format!("execute if block {} {kind}", at(block)))
            .await
            .contains("Test passed")
    }

    /// Selects a hotbar slot and waits until the server has it selected.
    async fn select(&self, slot: u8) {
        let slot = fleet_core::mode::HotbarSlot::try_from(slot).unwrap();
        self.perform(GameAction::SelectHotbarSlot { slot }).await;
        self.server
            .eventually(
                "the slot selected",
                &format!("data get entity {NAME} SelectedItemSlot"),
                |output| data_floats(output) == [f32::from(slot.get())],
            )
            .await;
    }

    /// One whole number the server answers `data get entity … <path>`
    /// with; 0 when there is no such data.
    async fn number(&self, path: &str) -> i64 {
        let output = self
            .server
            .rcon(&format!("data get entity {NAME} {path}"))
            .await;
        data_floats_wide(&output)
            .first()
            .map_or(0, |number| format!("{number:.0}").parse().unwrap_or(0))
    }

    /// How many of the item in its hand the server has the bot holding; 0
    /// for an empty hand.
    async fn count_in_hand(&self) -> i64 {
        self.number("SelectedItem.count").await
    }

    /// How worn the tool in the bot's hand is, as the server has it.
    async fn damage_in_hand(&self) -> i64 {
        self.number("SelectedItem.components.\"minecraft:damage\"")
            .await
    }

    /// Whether the server has the bot in a vehicle.
    async fn rides(&self) -> bool {
        self.server
            .rcon(&format!("data get entity {NAME} RootVehicle"))
            .await
            .contains("has the following entity data")
    }
}

/// How many game ticks lay between each two uses the bot sent: the tick
/// ends between them, which azalea sends once a game tick.
fn ticks_between_uses(sent: &[ServerboundGamePacket]) -> Vec<usize> {
    let mut gaps = Vec::new();
    let mut since_use = None;
    for packet in sent {
        match packet {
            ServerboundGamePacket::UseItem(_) | ServerboundGamePacket::UseItemOn(_) => {
                if let Some(ticks) = since_use.replace(0) {
                    gaps.push(ticks);
                }
            }
            ServerboundGamePacket::ClientTickEnd(_) => {
                if let Some(ticks) = since_use.as_mut() {
                    *ticks += 1;
                }
            }
            _ => {}
        }
    }
    gaps
}

fn uses(sent: &[ServerboundGamePacket]) -> usize {
    sent.iter()
        .filter(|packet| {
            matches!(
                packet,
                ServerboundGamePacket::UseItem(_) | ServerboundGamePacket::UseItemOn(_)
            )
        })
        .count()
}

/// The player actions of `kind` the bot sent for `block`.
fn actions_on(sent: &[ServerboundGamePacket], kind: PlayerAction, block: (i32, i32, i32)) -> usize {
    sent.iter()
        .filter(|packet| match packet {
            ServerboundGamePacket::PlayerAction(action) => {
                action.action == kind && (action.pos.x, action.pos.y, action.pos.z) == block
            }
            _ => false,
        })
        .count()
}

/// The left button mines a cobblestone generator block after block behind
/// both guards; the right button uses again every 4 ticks, or once for an
/// item used over time; mounting passes over a minecart someone sits in;
/// and a break the server refuses is tried again and again.
#[tokio::test(flavor = "multi_thread")]
async fn slow_mouse_scenario() {
    // Blocks around the world's spawn are protected from the moment the
    // server has an operator, which the last step makes one.
    let mut scene = Scene::start(16).await;

    a_generator_is_mined_behind_both_guards(&mut scene).await;
    the_right_button_keeps_the_vanilla_clients_cadence(&mut scene).await;
    mounting_passes_over_an_occupied_minecart(&mut scene).await;
    a_break_the_server_refuses_is_tried_again(&mut scene).await;

    within("disconnect", scene.session.disconnect()).await;
}

/// Builds the cobblestone generator, waits for its first block, and puts
/// the bot before it with a pickaxe that has 6 of its 59 uses left.
async fn a_generator_and_a_worn_pickaxe(scene: &mut Scene) {
    let deeper = BELOW - 1;
    scene
        .rcon(&[
            // Two layers of floor, then the trench on top of it: water at
            // its north end, a hole the water falls into, the place where
            // cobblestone forms, and lava at its south end. The west side
            // of that place is open, towards the bot.
            format!("fill 18 {deeper} -2 24 {BELOW} 6 minecraft:stone"),
            format!("fill 21 {FLOOR} -1 23 {FLOOR} 4 minecraft:stone"),
            format!("setblock 22 {FLOOR} 0 minecraft:air"),
            format!("setblock 22 {FLOOR} 1 minecraft:air"),
            format!("setblock 22 {BELOW} 1 minecraft:air"),
            format!("setblock {} minecraft:air", at(COBBLE)),
            format!("setblock 21 {FLOOR} 2 minecraft:air"),
            format!("setblock 22 {FLOOR} 3 minecraft:air"),
            // Behind that place, bedrock: a held button goes on to whatever
            // the crosshair lands on once a block is gone, as in vanilla,
            // and a wall of stone there was mined too (found on the first
            // run: three cobblestone and one stone for the tool's 4 uses).
            format!("setblock 23 {FLOOR} 2 minecraft:bedrock"),
            format!("setblock 22 {FLOOR} 0 minecraft:water"),
            "scoreboard objectives add mined minecraft.mined:minecraft.cobblestone".to_owned(),
        ])
        .await;
    scene.put_at(AWAY, -90.0).await;
    scene.put_at(MINER, -90.0).await;
    // The water has reached its hole by now; then the lava flows to it.
    scene
        .server
        .eventually(
            "the water in its hole",
            &format!("execute if block 22 {BELOW} 1 minecraft:water"),
            |output| output.contains("Test passed"),
        )
        .await;
    scene
        .server
        .rcon(&format!("setblock 22 {FLOOR} 3 minecraft:lava"))
        .await;
    scene
        .server
        .eventually(
            "the first cobblestone",
            &format!("execute if block {} minecraft:cobblestone", at(COBBLE)),
            |output| output.contains("Test passed"),
        )
        .await;
    // The guard keeps 2 of the pickaxe's uses.
    scene
        .server
        .rcon(&format!(
            "item replace entity {NAME} weapon.mainhand with minecraft:wooden_pickaxe[minecraft:damage=53]"
        ))
        .await;
    scene
        .server
        .eventually(
            "the pickaxe in hand",
            &format!("data get entity {NAME} SelectedItem.id"),
            |output| output.contains("wooden_pickaxe"),
        )
        .await;
    scene.guards(8, ToolGuard::DEFAULT).await;
    let _ = scene.reported().await;
}

async fn a_generator_is_mined_behind_both_guards(scene: &mut Scene) {
    a_generator_and_a_worn_pickaxe(scene).await;

    // The mining guard: held on the block it stands on, the button mines
    // nothing, and sends nothing for it.
    scene.look_at(UNDER_THE_MINER).await;
    let seen = scene.observe().await;
    assert_eq!(
        seen.facts.bot().looking_at,
        Some(fleet_core::script::LookingAt::Block {
            id: "minecraft:stone".parse().unwrap(),
            at: fleet_core::script::BlockPosition {
                x: UNDER_THE_MINER.0,
                y: UNDER_THE_MINER.1,
                z: UNDER_THE_MINER.2,
            },
        })
    );
    assert_eq!(
        seen.facts.bot().held.as_ref().map(|held| held.durability),
        Some(Some(6))
    );
    let mark = scene.mark();
    scene.press(LEFT).await;
    ticks_pass(&scene.session, 60).await;
    assert!(scene.block_is(UNDER_THE_MINER, "minecraft:stone").await);
    assert_eq!(
        actions_on(
            &scene.sent_since(mark),
            PlayerAction::StartDestroyBlock,
            UNDER_THE_MINER
        ),
        0
    );
    assert_eq!(scene.reported().await, NOTHING);

    // Still held: looking at the generator, the hold goes on by itself,
    // block after block, until the tool guard ends it at 2 uses left.
    scene.look_at(COBBLE).await;
    scene.lets_go(LetGo::ToolGuard).await;
    // The server counts the fourth at its next tick.
    scene
        .server
        .eventually(
            "four blocks mined",
            &format!("scoreboard players get {NAME} mined"),
            |output| score(output) == Some(4),
        )
        .await;
    assert_eq!(scene.damage_in_hand().await, 57);
    let sent = scene.sent_since(mark);
    assert_eq!(
        actions_on(&sent, PlayerAction::StopDestroyBlock, COBBLE),
        4,
        "{sent:?}"
    );
    // Nothing but the generator's block was mined to its end.
    let finished = sent
        .iter()
        .filter(|packet| {
            matches!(
                packet,
                ServerboundGamePacket::PlayerAction(action)
                    if action.action == PlayerAction::StopDestroyBlock
            )
        })
        .count();
    assert_eq!(finished, 4, "{sent:?}");
    // The button is up: the generator's next block stays.
    scene
        .server
        .eventually(
            "the next cobblestone",
            &format!("execute if block {} minecraft:cobblestone", at(COBBLE)),
            |output| output.contains("Test passed"),
        )
        .await;
    ticks_pass(&scene.session, 60).await;
    assert_eq!(scene.score("mined").await, 4);
    assert!(scene.block_is(COBBLE, "minecraft:cobblestone").await);

    // A new hold with the worn tool is ended at once.
    scene.press(LEFT).await;
    scene.lets_go(LetGo::ToolGuard).await;
    ticks_pass(&scene.session, 40).await;
    assert_eq!(scene.score("mined").await, 4);
    assert_eq!(scene.damage_in_hand().await, 57);
    scene.perform(GameAction::ReleaseAll).await;
    let _ = scene.reported().await;
}

async fn the_right_button_keeps_the_vanilla_clients_cadence(scene: &mut Scene) {
    scene
        .rcon(&[
            format!("item replace entity {NAME} hotbar.1 with minecraft:stick"),
            format!("item replace entity {NAME} hotbar.2 with minecraft:golden_apple 3"),
        ])
        .await;
    // Up at the sky: a use of the item, not of a block.
    scene
        .perform(GameAction::Look {
            yaw: 0.0,
            pitch: -90.0,
        })
        .await;
    scene.server.sees_rotation(NAME, 0.0, -90.0).await;

    // An item that isn't used over time: again every 4 game ticks.
    scene.select(1).await;
    ticks_pass(&scene.session, 3).await;
    let mark = scene.mark();
    scene.press(RIGHT).await;
    ticks_pass(&scene.session, 30).await;
    scene.release(RIGHT).await;
    ticks_pass(&scene.session, 12).await;
    let sent = scene.sent_since(mark);
    let gaps = ticks_between_uses(&sent);
    eprintln!("MEASURED the game ticks between a held right button's uses: {gaps:?}");
    assert!(gaps.len() >= 5, "{gaps:?}");
    assert!(gaps.iter().all(|ticks| *ticks == 4), "{gaps:?}");
    // Nothing after the release, which the server was told of.
    let released = sent
        .iter()
        .rposition(|packet| {
            matches!(
                packet,
                ServerboundGamePacket::PlayerAction(action)
                    if action.action == PlayerAction::ReleaseUseItem
            )
        })
        .expect("the release went out");
    assert_eq!(uses(&sent[released..]), 0, "{:?}", &sent[released..]);

    // An item used over time, held until used: one use, one apple, and
    // the button goes up by itself.
    scene.select(2).await;
    ticks_pass(&scene.session, 3).await;
    let mark = scene.mark();
    scene.perform(GameAction::HoldUntilUsed).await;
    scene.lets_go(LetGo::ItemUsed).await;
    assert_eq!(scene.count_in_hand().await, 2);
    ticks_pass(&scene.session, 20).await;
    assert_eq!(uses(&scene.sent_since(mark)), 1);
    assert_eq!(scene.count_in_hand().await, 2);

    // Held on: one long use an apple, the next started when one is eaten.
    let mark = scene.mark();
    scene.press(RIGHT).await;
    scene
        .server
        .eventually(
            "the second apple eaten",
            &format!("data get entity {NAME} SelectedItem.count"),
            |output| data_floats(output) == [1.0],
        )
        .await;
    ticks_pass(&scene.session, 8).await;
    scene.release(RIGHT).await;
    assert_eq!(uses(&scene.sent_since(mark)), 2);
    ticks_pass(&scene.session, 40).await;
    assert_eq!(scene.count_in_hand().await, 1);
    assert_eq!(scene.reported().await, NOTHING);
}

async fn mounting_passes_over_an_occupied_minecart(scene: &mut Scene) {
    /// A seated player's feet, from its minecart's position (ADR-0026).
    const SEAT: f64 = -0.4125;
    let rider = Vec3::new(22.5, FEET, 10.5);
    scene
        .rcon(&[
            format!("fill 18 {BELOW} 8 24 {BELOW} 14 minecraft:stone"),
            format!("setblock 21 {FLOOR} 10 minecraft:rail"),
            format!("setblock 22 {FLOOR} 12 minecraft:rail"),
        ])
        .await;
    scene.put_at(AWAY, 0.0).await;
    scene.put_at(rider, 0.0).await;
    scene
        .rcon(&[
            // The nearer one, a block west, with a pig in it; the further
            // one, two blocks south, free.
            format!(
                "summon minecraft:minecart 21.5 {FLOOR} 10.5 {{Tags:[\"near\"],Passengers:[{{id:\"minecraft:pig\",NoAI:1b,Silent:1b}}]}}"
            ),
            format!("summon minecraft:minecart 22.5 {FLOOR} 12.5 {{Tags:[\"far\"]}}"),
        ])
        .await;
    let near_cart = scene.position_of("@e[tag=near,limit=1]").await;
    let far_cart = scene.position_of("@e[tag=far,limit=1]").await;

    // The bot sees both, and which of them is taken.
    let minecart: fleet_core::script::Identifier = "minecart".parse().unwrap();
    let deadline = Instant::now() + WITHIN;
    let seen = loop {
        let seen = scene.observe().await;
        let carts: Vec<_> = seen
            .facts
            .entities()
            .iter()
            .filter(|entity| entity.kind == minecart)
            .map(|entity| entity.occupied)
            .collect();
        if carts == [true, false] {
            break seen;
        }
        assert!(Instant::now() < deadline, "the bot sees {carts:?}");
        ticks_pass(&scene.session, 1).await;
    };
    let nearest = seen.facts.nearest(&minecart).unwrap();
    let free = seen.facts.nearest_free(&minecart).unwrap();
    // A minecart settles a sixteenth of a block onto its rail, which the
    // bot may hear of a tick later.
    assert!(near(place(nearest.position), near_cart, 0.1), "{nearest:?}");
    assert!(near(place(free.position), far_cart, 0.1), "{free:?}");

    // The stopgap action takes the nearest, where a pig sits: nothing.
    scene.perform(GameAction::EnterMinecart).await;
    ticks_pass(&scene.session, 40).await;
    assert!(!scene.rides().await);

    // Mounting takes the free one.
    scene.perform(GameAction::Mount).await;
    scene
        .server
        .eventually(
            "the bot in a minecart",
            &format!("data get entity {NAME} RootVehicle"),
            |output| output.contains("has the following entity data"),
        )
        .await;
    scene
        .gets("the far minecart's seat", |at| {
            near(at, far_cart.up(SEAT), 0.05)
        })
        .await;
    let deadline = Instant::now() + WITHIN;
    loop {
        let seen = scene.observe().await;
        if seen.facts.bot().riding.as_ref() == Some(&minecart) {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", seen.facts.bot());
        ticks_pass(&scene.session, 1).await;
    }

    // And out again, as sneaking does.
    scene.perform(GameAction::Dismount).await;
    scene
        .server
        .eventually(
            "the bot on foot",
            &format!("data get entity {NAME} RootVehicle"),
            |output| !output.contains("has the following entity data"),
        )
        .await;
    ticks_pass(&scene.session, 10).await;
    assert_eq!(scene.keys_on_the_wire(), Some([false; 7]));
    assert_eq!(scene.observe().await.facts.bot().riding, None);
    assert_eq!(scene.reported().await, NOTHING);
}

/// D11's known limit, shown live: the bot isn't an operator, the block is
/// inside the spawn protection, and the server refuses each break.
async fn a_break_the_server_refuses_is_tried_again(scene: &mut Scene) {
    let stand = Vec3::new(60.5, FEET, 60.5);
    let protected = (62, FLOOR, 60);
    scene
        .rcon(&[
            "forceload add 56 56 66 66".to_owned(),
            format!("fill 58 {BELOW} 58 64 {BELOW} 62 minecraft:stone"),
            format!("setblock {} minecraft:cobblestone", at(protected)),
            format!("item replace entity {NAME} hotbar.3 with minecraft:iron_pickaxe"),
            format!("setworldspawn 60 {FLOOR} 60"),
            // Any operator at all switches the protection on.
            "op AfkOwner".to_owned(),
        ])
        .await;
    scene.put_at(AWAY, -90.0).await;
    scene.put_at(stand, -90.0).await;
    scene.select(3).await;
    scene.look_at(protected).await;
    let before = scene.score("mined").await;
    let _ = scene.reported().await;

    let mark = scene.mark();
    scene.press(LEFT).await;
    ticks_pass(&scene.session, 100).await;
    scene.release(LEFT).await;
    ticks_pass(&scene.session, 10).await;

    // The server kept the block and counted nothing.
    assert!(scene.block_is(protected, "minecraft:cobblestone").await);
    assert_eq!(scene.score("mined").await, before);
    assert_eq!(scene.damage_in_hand().await, 0);
    // The bot's own picture is the server's again.
    let seen = scene.observe().await;
    assert_eq!(
        seen.facts
            .blocks()
            .get(fleet_core::script::BlockPosition {
                x: protected.0,
                y: protected.1,
                z: protected.2,
            })
            .map(fleet_core::script::Identifier::as_str),
        Some("minecraft:cobblestone")
    );
    // What it did meanwhile: it mined the block to its end, was put right,
    // and began again.
    let sent = scene.sent_since(mark);
    let started = actions_on(&sent, PlayerAction::StartDestroyBlock, protected);
    let finished = actions_on(&sent, PlayerAction::StopDestroyBlock, protected);
    eprintln!(
        "MEASURED a refused break, 100 ticks of holding: {started} starts, {finished} finished breaks"
    );
    assert!(
        started >= 2 && finished >= 1,
        "{started} starts, {finished} ends"
    );
    // Nothing else happened: no let-go, and the session is up.
    assert_eq!(scene.reported().await, NOTHING);
    assert_eq!(scene.session.set_guards(Guards::default()).await, Ok(()));
}
