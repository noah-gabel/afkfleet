//! The reference's examples as written, on a real server (Plan.md P15.29;
//! ADR-0036, D4): each set on a bot through the API, as the editor's
//! starter template sets it, and judged by what the server shows.
//!
//! - **The AFK farm** looks where it says at its join and at a respawn, and
//!   attacks what's in front every 1.2 s: an iron golem without AI two
//!   blocks before it, whose damage the bot's statistic counts.
//! - **The minecart farm** gets into the nearest free minecart, passing over
//!   a nearer one a pig sits in, and again after a death and after a trip
//!   to another world and back.
//! - **The cobblestone generator** mines a real generator with the nine
//!   pickaxes of its hotbar, one block each before the tool guard keeps the
//!   last two uses, never the wall behind it, and then says it has no
//!   pickaxe left.

use std::rc::Rc;
use std::time::Instant;

use crate::harness::agent::Agent;
use crate::harness::world::World;
use crate::managed_check::minecraft::Minecraft;

use super::{
    CONFIRMED, NAME, agent_on_minecraft, bot_on, example, facing, on_a_server, passes, position,
    rcon, rotation, score, started, until_logged,
};

/// Where the AFK farm looks: west, a little down.
const AFK_LOOK: [f64; 2] = [90.0, 10.0];

/// Waits until the server shows `NAME` facing `look`.
async fn looks(world: &World, minecraft: &Minecraft, look: [f64; 2]) {
    world
        .logs
        .until(&format!("{NAME} facing {look:?}"), async || {
            facing(rotation(minecraft, NAME).await, look)
        })
        .await;
}

/// Waits until `NAME`'s score in `objective` is at least `at_least`, and
/// says when that was seen.
async fn scored(world: &World, minecraft: &Minecraft, objective: &str, at_least: i64) -> Instant {
    world
        .logs
        .until(&format!("{NAME}'s {objective} at {at_least}"), async || {
            score(minecraft, NAME, objective).await >= at_least
        })
        .await;
    Instant::now()
}

#[tokio::test]
async fn slow_the_afk_farm_looks_where_it_says_and_attacks_every_1_2_seconds() {
    on_a_server(afk_farm).await;
}

async fn afk_farm(world: Rc<World>, minecraft: Rc<Minecraft>) -> (Agent, ()) {
    let agent = agent_on_minecraft(&world, 1).await;
    let bot = bot_on(&world, &minecraft, NAME).await;
    let mode = example(&world, "afk_farm").await;
    world.owner.set_mode(bot, mode).await;
    rcon(
        &minecraft,
        "scoreboard objectives add dealt minecraft.custom:minecraft.damage_dealt",
    )
    .await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    started(&world, 1).await;

    // The join's look; a new player faces south.
    looks(&world, &minecraft, AFK_LOOK).await;

    // An iron golem two blocks west, where the bot looks: it never moves,
    // and a fist's hit counts 10 of the bot's damage dealt.
    let [x, y, z] = position(&minecraft, NAME).await;
    rcon(
        &minecraft,
        &format!(
            "summon minecraft:iron_golem {} {y} {z} {{NoAI:1b,Silent:1b}}",
            x - 2.0
        ),
    )
    .await;
    // Five hits, one at each of the timer's swings: four gaps of 1.2 s, as
    // read through RCON, which takes a tenth of a second or two a read.
    let first = scored(&world, &minecraft, "dealt", 10).await;
    let fifth = scored(&world, &minecraft, "dealt", 50).await;
    let took = fifth.duration_since(first).as_secs_f64();
    assert!(
        (4.3..=6.5).contains(&took),
        "four gaps between hits took {took:.2} s; the timer's 1.2 s make 4.8 s"
    );

    // A respawned player faces the spawn's direction: the farm looks again.
    rcon(&minecraft, &format!("kill {NAME}")).await;
    world
        .logs
        .until("the respawn confirmed", async || {
            world.logs.count(CONFIRMED) >= 1
        })
        .await;
    looks(&world, &minecraft, AFK_LOOK).await;
    (agent, ())
}

#[tokio::test]
async fn slow_the_minecart_farm_gets_into_the_free_minecart_at_a_join_a_respawn_and_a_world_change()
{
    on_a_server(minecart_farm).await;
}

/// The `execute` test that `NAME` sits in the free minecart.
fn in_the_free_one() -> String {
    format!("execute as {NAME} on vehicle if entity @s[tag=free]")
}

/// Waits until `NAME` sits in the free minecart.
async fn seated(world: &World, minecraft: &Minecraft, what: &str) {
    world
        .logs
        .until(what, async || passes(minecraft, &in_the_free_one()).await)
        .await;
}

async fn minecart_farm(world: Rc<World>, minecraft: Rc<Minecraft>) -> (Agent, ()) {
    let agent = agent_on_minecraft(&world, 1).await;
    let bot = bot_on(&world, &minecraft, NAME).await;
    let mode = example(&world, "minecart_farm").await;
    world.owner.set_mode(bot, mode).await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    started(&world, 1).await;

    // A respawn puts the bot back here; the nearer minecart holds a pig,
    // and the free one is a little further. The script looks again every
    // second until it sits.
    let [x, y, z] = position(&minecraft, NAME).await;
    rcon(
        &minecraft,
        &format!(
            "spawnpoint {NAME} {} {} {}",
            x.floor(),
            y.floor(),
            z.floor()
        ),
    )
    .await;
    rcon(
        &minecraft,
        &format!(
            "summon minecraft:minecart {} {y} {z} {{Tags:[\"taken\"],Passengers:[{{id:\"minecraft:pig\",NoAI:1b}}]}}",
            x + 1.5
        ),
    )
    .await;
    rcon(
        &minecraft,
        &format!(
            "summon minecraft:minecart {x} {y} {} {{Tags:[\"free\"]}}",
            z + 2.5
        ),
    )
    .await;
    seated(&world, &minecraft, "seated at the join").await;
    assert!(
        passes(
            &minecraft,
            "execute as @e[tag=taken] on passengers if entity @s[type=minecraft:pig]"
        )
        .await,
        "the pig keeps its minecart"
    );

    // A death takes it out of the seat; after the respawn it gets in again.
    rcon(&minecraft, &format!("kill {NAME}")).await;
    world
        .logs
        .until("the respawn confirmed", async || {
            world.logs.count(CONFIRMED) >= 1
        })
        .await;
    seated(&world, &minecraft, "seated after the respawn").await;

    // To another world and back: on the Nether's roof no minecart is in
    // reach; back beside the free one, it gets in again.
    rcon(
        &minecraft,
        &format!("execute in minecraft:the_nether run tp {NAME} 0.5 128 0.5"),
    )
    .await;
    world
        .logs
        .until("in the Nether, out of the seat", async || {
            !passes(&minecraft, &in_the_free_one()).await
                && rcon(&minecraft, &format!("data get entity {NAME} Dimension"))
                    .await
                    .contains("the_nether")
        })
        .await;
    rcon(
        &minecraft,
        &format!("execute in minecraft:overworld run tp {NAME} {x} {y} {z}"),
    )
    .await;
    seated(&world, &minecraft, "seated after the world change and back").await;
    (agent, ())
}

#[tokio::test]
async fn slow_the_cobblestone_generator_mines_one_block_a_pickaxe_and_never_the_wall() {
    on_a_server(generator).await;
}

/// The generator's level: the cobblestone forms here, and the bot stands on
/// the floor below it.
const FLOOR: i32 = -40;
/// The place where the cobblestone forms: two blocks south of the bot,
/// where the example looks (yaw 0, south; pitch 20).
const COBBLE: (i32, i32, i32) = (2, FLOOR, 22);

/// Builds the generator, which `slow_mouse_scenario` built facing east,
/// turned to face the example's south: a floor, a trench along x with water
/// at its west end, a hole the water falls into, the place where the
/// cobblestone forms, lava at its east end, the trench's stone wall behind
/// that place, and its north side open towards the bot. Waits for its first
/// block.
async fn build_the_generator(world: &World, minecraft: &Minecraft) {
    let below = FLOOR - 1;
    let deeper = below - 1;
    let (x, y, z) = COBBLE;
    for command in [
        format!("fill -2 {deeper} 18 6 {below} 24 minecraft:stone"),
        format!("fill -1 {FLOOR} 21 4 {FLOOR} 23 minecraft:stone"),
        format!("setblock 0 {FLOOR} {z} minecraft:air"),
        format!("setblock 1 {FLOOR} {z} minecraft:air"),
        format!("setblock 1 {below} {z} minecraft:air"),
        format!("setblock {x} {y} {z} minecraft:air"),
        format!("setblock {x} {y} {} minecraft:air", z - 1),
        format!("setblock 3 {FLOOR} {z} minecraft:air"),
        format!("setblock 0 {FLOOR} {z} minecraft:water"),
        "scoreboard objectives add mined minecraft.mined:minecraft.cobblestone".to_owned(),
        "scoreboard objectives add mined_stone minecraft.mined:minecraft.stone".to_owned(),
    ] {
        rcon(minecraft, &command).await;
    }
    world
        .logs
        .until("the water in its hole", async || {
            passes(
                minecraft,
                &format!("execute if block 1 {below} {z} minecraft:water"),
            )
            .await
        })
        .await;
    rcon(minecraft, &format!("setblock 3 {FLOOR} {z} minecraft:lava")).await;
    world
        .logs
        .until("the first cobblestone", async || {
            passes(
                minecraft,
                &format!("execute if block {x} {y} {z} minecraft:cobblestone"),
            )
            .await
        })
        .await;
}

async fn generator(world: Rc<World>, minecraft: Rc<Minecraft>) -> (Agent, ()) {
    let agent = agent_on_minecraft(&world, 1).await;
    let bot = bot_on(&world, &minecraft, NAME).await;
    let mode = example(&world, "cobblestone_generator").await;
    world.owner.set_mode(bot, mode).await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    started(&world, 1).await;
    // The bot at the spawn keeps the generator's chunks loaded and ticking:
    // no chunk is without a player near it. Its held button meanwhile only
    // pauses at the ground below its feet.
    build_the_generator(&world, &minecraft).await;

    // Nine wooden pickaxes with 3 of their 59 uses left: the tool guard
    // keeps 2, so each mines one block. Then the bot goes before the
    // generator, looking where its join turned it.
    for slot in 0..9 {
        rcon(
            &minecraft,
            &format!(
                "item replace entity {NAME} hotbar.{slot} with minecraft:wooden_pickaxe[minecraft:damage=56]"
            ),
        )
        .await;
    }
    let (x, _, z) = COBBLE;
    rcon(
        &minecraft,
        &format!("tp {NAME} {}.5 {FLOOR} {}.5", x, z - 2),
    )
    .await;
    assert!(facing(rotation(&minecraft, NAME).await, [0.0, 20.0]));

    world
        .logs
        .until("nine cobblestone mined", async || {
            score(&minecraft, NAME, "mined").await >= 9
        })
        .await;
    until_logged(&world, "Line", "No pickaxe left.", 1).await;
    assert_eq!(score(&minecraft, NAME, "mined").await, 9);
    assert_eq!(
        score(&minecraft, NAME, "mined_stone").await,
        0,
        "the wall stays"
    );
    (agent, ())
}
