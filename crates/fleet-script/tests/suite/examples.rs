//! The examples, compiled and run against the fake world as written
//! (P15.8, P15.31, P16.12; ADR-0036, D4). The menu examples' runs are
//! `menu_examples`.

use core::time::Duration;
use std::sync::Arc;

use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    BlockPosition, Captured, ChatMatch, HookEvent, Ident, Input, Key, LookingAt, MouseButton,
    Script,
};
use fleet_script::{Action, EXAMPLES, Example, Instance, Outcome, PressKind, compile};

use crate::support::{World, at, id, pickaxe};

fn example(name: &str) -> &'static Example {
    EXAMPLES
        .iter()
        .find(|example| example.id == name)
        .expect("an example of that name")
}

/// An instance of the example `name`.
fn instance_of(name: &str) -> Instance {
    let script = Script::try_from(example(name).draft()).unwrap();
    Instance::new(
        Arc::new(compile(&script).unwrap()),
        7,
        crate::support::still(),
    )
}

/// Whether every wake in `wakes` ran to its end.
fn all_ran(wakes: &[fleet_script::Ran]) -> bool {
    wakes
        .iter()
        .all(|ran| matches!(ran.outcome, Outcome::Ran { .. }))
}

#[test]
fn every_example_is_a_scripted_mode_that_compiles() {
    assert_eq!(
        EXAMPLES.map(|example| example.id),
        [
            "afk_farm",
            "minecart_farm",
            "cobblestone_generator",
            "balance",
            "sell_macro",
            "auction_scan",
            "farm_that_sells",
            "auction_lister",
            "auction_lister_anvil"
        ]
    );
    for example in &EXAMPLES {
        let script = Script::try_from(example.draft())
            .unwrap_or_else(|error| panic!("{}: {error}", example.id));

        assert!(compile(&script).is_ok(), "{}", example.id);
        assert_eq!(script.leash().blocks(), 8);
        assert_eq!(script.tool_guard().uses_left(), 2);
        // Only the generator mines, and it lists what it mines.
        let listed: Vec<&str> = script
            .block_guard()
            .blocks()
            .iter()
            .map(fleet_core::script::Identifier::as_str)
            .collect();
        if example.id == "cobblestone_generator" {
            assert_eq!(listed, ["minecraft:cobblestone"]);
        } else {
            assert_eq!(listed, [""; 0]);
        }
    }
}

#[test]
fn the_reference_shows_every_example_as_its_file_has_it() {
    let reference = include_str!("../../../../docs/scripting.md").replace("\r\n", "\n");

    for example in &EXAMPLES {
        let heading = format!("#### {}\n", example.title);
        let block = format!("```rhai\n{}```", example.source.replace("\r\n", "\n"));
        let section = reference
            .split_once(&heading)
            .unwrap_or_else(|| panic!("the reference has no section {}", example.title))
            .1;
        let section = section.split("\n#### ").next().unwrap_or_default();

        assert!(
            section.contains(&block),
            "{} differs from its file",
            example.id
        );
    }
}

/// The reference's tables are what the description writes (`just gen`):
/// no table names a member the engine doesn't have, or misses one it has.
#[test]
fn the_references_tables_are_the_descriptions() {
    let reference = include_str!("../../../../docs/scripting.md").replace("\r\n", "\n");

    let written = fleet_script::with_tables(&reference).unwrap();

    assert!(
        written == reference,
        "docs/scripting.md's tables differ from the description: run `just gen`"
    );
    // Every object and the hooks have their table, and so do the reasons.
    for object in fleet_script::API {
        assert!(
            reference.contains(&format!("<!-- generated table: {} -->", object.name)),
            "no table of {}",
            object.name
        );
    }
    assert!(reference.contains("<!-- generated table: reasons -->"));
}

#[test]
fn the_balance_example_declares_what_the_reference_says() {
    let script = Script::try_from(example("balance").draft()).unwrap();

    assert_eq!(
        script
            .messages()
            .iter()
            .map(fleet_core::script::MessageTemplate::text)
            .collect::<Vec<_>>(),
        ["/balance", "/pay AfkMain {amount}"]
    );
    assert_eq!(
        script.command_names().collect::<Vec<_>>(),
        ["balance", "pay"]
    );
    assert_eq!(script.patterns().len(), 1);
    assert_eq!(script.patterns().any_time_count(), 0);
    let reply = script.patterns().get("balance").unwrap();
    assert_eq!(reply.text(), "Balance: ${amount}");
}

#[test]
fn the_afk_farm_swings_every_1200_ms_and_sets_up_again_after_a_respawn() {
    let mut world = World::new();
    let mut instance = instance_of("afk_farm");

    let join = world.run(&mut instance, &HookEvent::Join { first: true });
    assert_eq!(
        join.actions,
        [
            Action::ReleaseAll,
            Action::SelectHotbar(HotbarSlot::FIRST),
            Action::LookAt {
                yaw: 90.0,
                pitch: 10.0
            },
        ]
    );
    let wakes = world.pass(&mut instance, Duration::from_millis(6_100));

    assert!(all_ran(&wakes));
    assert_eq!(world.left_taps(), 5);

    world.bot.selected = HotbarSlot::LAST;
    world.run(&mut instance, &HookEvent::Death);
    let dead = world.pass(&mut instance, Duration::from_secs(10));
    world.now += Duration::from_secs(1);
    let respawn = world.run(&mut instance, &HookEvent::Respawn);

    assert_eq!(dead.len(), 0);
    assert_eq!(world.left_taps(), 5);
    assert_eq!(
        respawn.actions,
        [
            Action::ReleaseAll,
            Action::SelectHotbar(HotbarSlot::FIRST),
            Action::LookAt {
                yaw: 90.0,
                pitch: 10.0
            },
        ]
    );
    let again = world.pass(&mut instance, Duration::from_millis(2_500));
    assert!(all_ran(&again));
    assert!(world.left_taps() > 5);
}

#[test]
fn the_minecart_farm_gets_in_first_and_then_swings_counted() {
    let mut world = World::new();
    world.minecart(at(2.5, 64.0, 0.5));
    let mut instance = instance_of("minecart_farm");

    // At the join the bot is on foot: it asks to mount and looks again
    // in a second.
    let join = world.run(&mut instance, &HookEvent::Join { first: true });
    assert!(join.actions.contains(&Action::Mount(id("minecart"))));
    assert_eq!(world.bot.riding, Some(id("minecart")));
    let wakes = world.pass(&mut instance, Duration::from_millis(3_100));

    assert!(all_ran(&wakes));
    // Seated from the first swing on: one every 600 ms.
    assert_eq!(world.left_taps(), 5);
    assert!(world.performed.contains(&Action::LookAt {
        yaw: 90.0,
        pitch: 0.0
    }));
    // Seated, `get_in` stopped setting its timer again.
    assert_eq!(instance.timers(), 1);
}

#[test]
fn the_minecart_farm_doesnt_swing_on_foot_and_keeps_looking_for_a_minecart() {
    let mut world = World::new();
    let mut instance = instance_of("minecart_farm");

    world.run(&mut instance, &HookEvent::Join { first: true });
    let wakes = world.pass(&mut instance, Duration::from_millis(5_100));

    assert!(all_ran(&wakes));
    assert_eq!(world.left_taps(), 0);
    let mounts = world
        .performed
        .iter()
        .filter(|action| matches!(action, Action::Mount(_)))
        .count();
    // At the join, and once a second since.
    assert_eq!(mounts, 6);

    // A minecart appears: the next look finds it, and the swings start.
    world.minecart(at(1.5, 64.0, 0.5));
    let later = world.pass(&mut instance, Duration::from_millis(2_500));
    assert!(all_ran(&later));
    assert_eq!(world.bot.riding, Some(id("minecart")));
    assert!(world.left_taps() >= 2);
}

#[test]
fn the_minecart_farm_gets_in_again_after_a_world_change_and_a_respawn() {
    let mut world = World::new();
    world.minecart(at(2.5, 64.0, 0.5));
    let mut instance = instance_of("minecart_farm");
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.pass(&mut instance, Duration::from_millis(1_300));
    let swings = world.left_taps();
    assert!(swings >= 1);

    // The server moves the bot to a lobby and back: it's on foot again.
    world.bot.riding = None;
    world.entities.clear();
    world.minecart(at(1.5, 64.0, 1.5));
    world.now += Duration::from_secs(1);
    let changed = world.run(&mut instance, &HookEvent::WorldChange);

    assert!(changed.actions.contains(&Action::Mount(id("minecart"))));
    assert_eq!(world.bot.riding, Some(id("minecart")));

    world.run(&mut instance, &HookEvent::Death);
    world.bot.riding = None;
    world.entities.clear();
    world.minecart(at(0.5, 64.0, 2.5));
    world.now += Duration::from_secs(1);
    let respawned = world.run(&mut instance, &HookEvent::Respawn);
    assert!(respawned.actions.contains(&Action::Mount(id("minecart"))));
    let wakes = world.pass(&mut instance, Duration::from_millis(2_000));

    assert!(all_ran(&wakes));
    assert!(world.left_taps() > swings);
}

#[test]
fn the_cobblestone_generator_mines_and_changes_pickaxes_until_none_is_left() {
    let left = Input::Mouse(MouseButton::Left);
    let mut world = World::new();
    // Nine pickaxes, each with a few uses above the tool guard's two.
    world.hotbar = core::array::from_fn(|_| Some(pickaxe(5)));
    world.bot.held = Some(pickaxe(5));
    world.bot.looking_at = Some(LookingAt::Block {
        id: id("cobblestone"),
        at: BlockPosition { x: 0, y: 64, z: 1 },
    });
    let mut instance = instance_of("cobblestone_generator");

    let join = world.run(&mut instance, &HookEvent::Join { first: true });
    assert_eq!(
        join.actions,
        [
            Action::ReleaseAll,
            Action::SelectHotbar(HotbarSlot::FIRST),
            Action::LookAt {
                yaw: 0.0,
                pitch: 20.0
            },
        ]
    );

    for slot in 0..9_u8 {
        // The timer finds the button up and holds it: mining starts.
        let wakes = world.pass(&mut instance, Duration::from_millis(1_000));
        assert!(all_ran(&wakes), "slot {slot}: {wakes:?}");
        assert!(instance.is_held(left), "slot {slot}");
        assert_eq!(world.bot.selected.get(), slot);

        // While it mines, the timer leaves it alone.
        let before = world.performed.len();
        world.pass(&mut instance, Duration::from_millis(3_000));
        assert_eq!(world.performed.len(), before, "slot {slot}");

        // The pickaxe is down to its last two uses: the guard below the
        // script stops the mining and lets go of the button.
        let slot_index = usize::from(slot);
        if let Some(Some(tool)) = world.hotbar.get_mut(slot_index) {
            tool.durability = Some(2);
        }
        world.bot.held = Some(pickaxe(2));
        instance.let_go([left]);

        // The next timer's hold is refused for the tool, and the script
        // takes the next pickaxe, or gives up after the ninth.
        let wakes = world.pass(&mut instance, Duration::from_millis(1_000));
        assert!(all_ran(&wakes), "slot {slot}: {wakes:?}");
    }

    assert_eq!(world.log, ["No pickaxe left."]);
    assert_eq!(instance.timers(), 0);
    assert_eq!(world.bot.selected, HotbarSlot::LAST);
    let holds = world
        .presses()
        .iter()
        .filter(|press| **press == (left, PressKind::Hold))
        .count();
    assert_eq!(holds, 9);
    assert!(!instance.is_held(left));
}

/// While the next cobblestone forms, the crosshair is on the wall behind
/// it: the mode lists cobblestone, so a new hold there is refused, nothing
/// is pressed, and the script keeps its pickaxe and tries again.
#[test]
fn the_cobblestone_generator_never_starts_on_the_wall_behind_its_block() {
    let left = Input::Mouse(MouseButton::Left);
    let block = |kind: &str| {
        Some(LookingAt::Block {
            id: id(kind),
            at: BlockPosition { x: 0, y: 64, z: 1 },
        })
    };
    let mut world = World::new();
    world.hotbar = core::array::from_fn(|_| Some(pickaxe(50)));
    world.bot.held = Some(pickaxe(50));
    world.bot.looking_at = block("stone");
    let mut instance = instance_of("cobblestone_generator");
    world.run(&mut instance, &HookEvent::Join { first: true });

    let wakes = world.pass(&mut instance, Duration::from_millis(3_100));

    assert!(all_ran(&wakes));
    assert_eq!(wakes.len(), 3);
    assert_eq!(world.presses(), []);
    assert!(!instance.is_held(left));
    assert_eq!(world.bot.selected, HotbarSlot::FIRST);
    assert_eq!(world.log.len(), 0);

    // The cobblestone is there: the next timer's hold is taken.
    world.bot.looking_at = block("cobblestone");
    world.pass(&mut instance, Duration::from_millis(1_000));

    assert_eq!(world.presses(), [(left, PressKind::Hold)]);
    assert!(instance.is_held(left));
}

#[test]
fn the_cobblestone_generator_moves_on_from_an_empty_slot() {
    let mut world = World::new();
    world.hotbar = core::array::from_fn(|slot| (slot != 0).then(|| pickaxe(50)));
    let mut instance = instance_of("cobblestone_generator");
    world.run(&mut instance, &HookEvent::Join { first: true });

    // Slot 1 is empty: the hold is taken (an empty hand mines too), so
    // the script keeps it. Nothing is refused, nothing breaks.
    let wakes = world.pass(&mut instance, Duration::from_millis(2_100));

    assert!(all_ran(&wakes));
    assert_eq!(wakes.len(), 2);
    assert!(instance.is_held(Input::Mouse(MouseButton::Left)));
    assert_eq!(world.log.len(), 0);
}

#[test]
fn the_balance_example_asks_at_every_join_and_every_ten_hours_and_pays_what_is_above() {
    let balance = |amount: f64| {
        HookEvent::Chat(ChatMatch {
            key: Ident::try_from("balance").unwrap(),
            values: vec![(Ident::try_from("amount").unwrap(), Captured::Amount(amount))],
        })
    };
    let mut world = World::new();
    let mut instance = instance_of("balance");

    let join = world.run(&mut instance, &HookEvent::Join { first: true });
    assert_eq!(world.sent, ["/balance"]);
    assert!(join.actions.iter().any(|action| matches!(
        action,
        Action::Expect { pattern, window, .. }
            if pattern.as_str() == "balance" && *window == Duration::from_secs(5)
    )));
    assert_eq!(join.next_wake, Some(world.now + Duration::from_hours(10)));

    // The server answers: 12,500.50 is 2,500.50 above the threshold.
    world.run(&mut instance, &balance(12_500.5));
    assert_eq!(world.sent, ["/balance", "/pay AfkMain 2500.50"]);

    // Below the threshold, and less than 1 above it: nothing is paid.
    world.run(&mut instance, &balance(9_000.0));
    world.run(&mut instance, &balance(10_000.5));
    assert_eq!(world.sent.len(), 2);

    // Ten hours later it asks again.
    let before = world.pass(
        &mut instance,
        Duration::from_hours(10) - Duration::from_secs(1),
    );
    assert_eq!(before.len(), 0);
    let due = world.pass(&mut instance, Duration::from_secs(1));
    assert!(all_ran(&due));
    assert_eq!(world.sent.last().map(String::as_str), Some("/balance"));
    assert_eq!(world.sent.len(), 3);

    // A reconnect asks at once, and sets the one timer again.
    instance.disconnected();
    world.now += Duration::from_secs(30);
    world.run(&mut instance, &HookEvent::Join { first: false });
    assert_eq!(world.sent.len(), 4);
    assert_eq!(instance.timers(), 1);
}

#[test]
fn the_balance_example_never_pays_more_than_its_declaration_allows() {
    let mut world = World::new();
    let mut instance = instance_of("balance");
    world.run(&mut instance, &HookEvent::Join { first: true });

    // A server that prints an absurd balance: the declared value ends at
    // 1,000,000, so nothing is sent at all.
    let ran = world.run(
        &mut instance,
        &HookEvent::Chat(ChatMatch {
            key: Ident::try_from("balance").unwrap(),
            values: vec![(
                Ident::try_from("amount").unwrap(),
                Captured::Amount(999_999_999.0),
            )],
        }),
    );

    assert!(matches!(ran.outcome, Outcome::Ran { .. }));
    assert_eq!(world.sent, ["/balance"]);
}

#[test]
fn no_example_presses_a_key_it_then_leaves_down_at_a_stop() {
    for example in &EXAMPLES {
        let script = Script::try_from(example.draft()).unwrap();
        let mut instance = Instance::new(
            Arc::new(compile(&script).unwrap()),
            7,
            crate::support::still(),
        );
        let mut world = World::new();
        world.minecart(at(1.5, 64.0, 0.5));
        world.run(&mut instance, &HookEvent::Join { first: true });
        world.pass(&mut instance, Duration::from_secs(5));

        let stop = world.run(&mut instance, &HookEvent::Stop);

        assert_eq!(
            stop.actions.last(),
            Some(&Action::ReleaseAll),
            "{}",
            example.id
        );
        for input in [
            Input::Mouse(MouseButton::Left),
            Input::Mouse(MouseButton::Right),
            Input::Key(Key::Forward),
        ] {
            assert!(!instance.is_held(input), "{}", example.id);
        }
    }
}
