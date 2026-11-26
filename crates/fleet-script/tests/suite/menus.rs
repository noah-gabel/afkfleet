//! API level 2's inventory, screens, clicks and signs (Plan.md P16.12;
//! ADR-0036, M1 to M18, R1, R2; ADR-0050, ADR-0052): what a hook reads,
//! what it may send, and what it hears back.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::mc::MenuCommand;
use fleet_core::mode::HotbarSlot;
use fleet_core::script::menu::{
    BotInventory, ClickGate, ClickKind, Item, ItemData, MenuAction, MenuRefusal, Screen, ScreenId,
    ScreenKind, SignAnswer, SignDesk, SignDraft, SignSide, StateId, TradeOffer, WindowId,
};
use fleet_core::script::{
    BlockPosition, ClickOutcome, Hook, HookEvent, Input, Key, MessageDraft, MouseButton, Refusal,
    ScriptDraft, ValueDraft, ValueKind,
};
use fleet_core::time;
use fleet_script::{
    Action, CompileErrorKind, Instance, Misuse, Outcome, PressKind, ScriptErrorKind, compile,
};

use crate::support::{World, draft, id, logged, program_of, script_of, still};

/// A level-2 draft of `source`.
fn draft_2(source: &str) -> ScriptDraft {
    ScriptDraft {
        api: 2,
        ..draft(source)
    }
}

/// A level-2 instance of `source` with `messages` and `signs` declared.
fn level_2(source: &str, messages: Vec<MessageDraft>, signs: Vec<SignDraft>) -> Instance {
    let draft = ScriptDraft {
        messages,
        signs,
        ..draft_2(source)
    };
    Instance::new(program_of(draft), 7, still())
}

/// A level-2 instance of `source` with nothing declared.
fn instance_2(source: &str) -> Instance {
    level_2(source, Vec::new(), Vec::new())
}

fn item(name: &str, count: u32) -> Item {
    Item::new(
        id(name),
        NonZeroU32::new(count).unwrap(),
        ItemData::new(Vec::new()),
    )
}

/// A potion of the Turtle Master listed at `price` by `seller`.
fn listing(price: u32, seller: &str) -> Item {
    item("minecraft:potion", 1)
        .named("Potion of the Turtle Master")
        .with_lore([format!("Price: {price}"), format!("Seller: {seller}")])
        .with_potion(Some("minecraft:turtle_master"))
}

/// The screen `kind` opened in a new window of `gate`, with `items` in its
/// slots from 0, in state `state`.
fn open(gate: &mut ClickGate, kind: ScreenKind, state: u32, items: Vec<Option<Item>>) -> Screen {
    let id = gate.opened(WindowId::new(1));
    Screen::new(id, StateId::new(state), kind, "Auction House", items, None)
}

/// A chest of three rows with listings in slots 0 to 2 and a button in 22.
fn auction(gate: &mut ClickGate) -> Screen {
    let mut items: Vec<Option<Item>> = vec![
        Some(listing(500, "Someone")),
        Some(listing(450, "AfkBot1")),
        Some(listing(700, "Other")),
    ];
    items.resize(22, None);
    items.push(Some(item("minecraft:arrow", 1).named("Next Page")));
    open(gate, ScreenKind::Generic9x3, 1, items)
}

/// The menu actions among `actions`.
fn menu_actions(actions: &[Action]) -> Vec<&MenuCommand> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Menu(command) => Some(command),
            _ => None,
        })
        .collect()
}

/// The clicks among `actions`: slot and kind.
fn clicks(actions: &[Action]) -> Vec<(u16, ClickKind)> {
    menu_actions(actions)
        .into_iter()
        .filter_map(|command| match command {
            MenuCommand::Act(MenuAction::Click(click)) => Some((click.slot(), click.kind())),
            _ => None,
        })
        .collect()
}

fn stopped_for(outcome: &Outcome) -> Option<&ScriptErrorKind> {
    match outcome {
        Outcome::Stopped(error) => Some(&error.kind),
        _ => None,
    }
}

// --- The level ---

#[test]
fn a_level_1_script_that_names_a_level_2_object_is_refused_with_the_level() {
    for name in ["inventory", "menu", "sign", "clock"] {
        let script = script_of(draft(&format!("fn on_join(first) {{\n  {name}.open;\n}}")));

        let error = compile(&script).unwrap_err();

        assert_eq!(
            error.kind,
            CompileErrorKind::NeedsLevel {
                name: name.to_owned(),
                level: 2
            }
        );
        assert_eq!(error.at.map(|at| at.line), Some(2), "{name}");
    }
}

#[test]
fn a_level_1_script_that_defines_a_level_2_hook_is_refused_with_the_level() {
    for (hook, parameters) in [
        ("on_menu_open", ""),
        ("on_menu_update", ""),
        ("on_menu_close", ""),
        ("on_click", "result"),
        ("on_sign_open", ""),
    ] {
        let script = script_of(draft(&format!("fn {hook}({parameters}) {{}}")));

        assert_eq!(
            compile(&script).unwrap_err().kind,
            CompileErrorKind::NeedsLevel {
                name: hook.to_owned(),
                level: 2
            }
        );
    }
}

#[test]
fn at_level_1_level_2s_names_stay_a_scripts_own() {
    let source = "fn on_join(first) {\n  let menu = 1; let sign = 2; let clock = 3; let inventory = 4;\n  log.info(`${menu + sign + clock + inventory}`);\n}\nfn number(text) { 7 }";
    let mut world = World::new();
    let mut instance = Instance::new(program_of(draft(source)), 7, still());

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    assert_eq!(world.log, ["10"]);
}

#[test]
fn at_level_2_its_objects_are_reserved() {
    let script = script_of(draft_2("fn on_join(first) { let menu = 1; }"));

    assert_eq!(
        compile(&script).unwrap_err().kind,
        CompileErrorKind::ReservedName {
            name: "menu".to_owned()
        }
    );
}

#[test]
fn bot_name_is_the_accounts_at_level_2_and_stops_a_level_1_script() {
    let mut world = World::new();
    let mut two = instance_2("fn on_join(first) { log.info(bot.name); }");
    let mut one = Instance::new(
        program_of(draft("fn on_join(first) {\n  log.info(bot.name);\n}")),
        7,
        still(),
    );

    world.run(&mut two, &HookEvent::Join { first: true });
    let ran = world.run(&mut one, &HookEvent::Join { first: true });

    assert_eq!(world.log, ["AfkBot1"]);
    assert_eq!(
        stopped_for(&ran.outcome),
        Some(&ScriptErrorKind::Misuse(Misuse::NeedsLevel2))
    );
}

#[test]
fn the_lore_helpers_read_every_line_a_slots_map_couldnt_keep() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let long = listing(800, "Someone").with_lore(
        (0..15)
            .map(|n| format!("{n:02} {}", "x".repeat(120)))
            .chain(["Seller: AfkBot1".to_owned()]),
    );
    world.menu.screen = Some(open(&mut gate, ScreenKind::Generic9x1, 1, vec![Some(long)]));
    let source = r#"fn on_menu_open() {
  let slot = menu.slot(0);
  log.info(`${slot.lore.len()} ${menu.lore_has(slot, "Seller: AfkBot1")} ${menu.lore_has(0, "Seller: AfkBot1")}`);
}"#;
    let mut instance = instance_2(source);

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    assert_eq!(world.log, ["7 true true"]);
}

// --- Reading ---

#[test]
fn a_hook_reads_the_screen_its_slots_and_their_items_as_plain_data() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.screen = Some(auction(&mut gate));
    let source = r#"fn on_menu_open() {
  log.info(`${menu.open} ${menu.kind} ${menu.title} ${menu.rows}x${menu.columns} ${menu.slots.len()} ${menu.player.len()} ${menu.player[0]}`);
  let first = menu.slot(0);
  log.info(`${first.index} ${first.row} ${first.column} ${first.id} ${first.count} ${first.name} ${first.potion}`);
  log.info(`${menu.lore_number(first, "Price")} ${menu.lore_has(0, "Seller: Someone")} ${first.empty}`);
  let button = menu.at(2, 4);
  log.info(`${button.index} ${button.name} ${menu.find("minecraft:arrow").index} ${menu.slot(5).empty} ${menu.slot(5).id}`);
  state.kept = first;
}"#;
    let mut instance = instance_2(source);

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    assert_eq!(
        world.log,
        [
            "true generic_9x3 Auction House 3x9 27 36 27",
            "0 0 0 minecraft:potion 1 Potion of the Turtle Master minecraft:turtle_master",
            "500.0 true false",
            "22 Next Page 22 true ",
        ]
    );
}

#[test]
fn without_a_screen_the_menu_reads_as_none() {
    let mut world = World::new();
    let source = r"fn on_join(first) {
  log.info(`${menu.open} ${menu.kind} ${menu.slots.len()} ${menu.slot(0)} ${menu.cursor} ${menu.screen} ${sign.open} ${sign.side}`);
}";
    let mut instance = instance_2(source);

    world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(world.log, ["false  0    false "]);
}

#[test]
fn the_inventory_reads_its_slots_finds_and_counts_items_and_names_the_hotbar() {
    let mut world = World::new();
    let mut slots: Vec<Option<Item>> = vec![None; 46];
    slots[10] = Some(item("minecraft:potion", 2).with_potion(Some("minecraft:turtle_master")));
    slots[36] =
        Some(item("minecraft:diamond_sword", 1).with_enchantments([("minecraft:sharpness", 5)]));
    slots[40] = Some(item("minecraft:potion", 3));
    world.menu.inventory = BotInventory::new(slots);
    let source = r#"fn on_join(first) {
  let potion = inventory.find("minecraft:potion");
  let sword = inventory.hotbar(1);
  log.info(`${inventory.slots.len()} ${inventory.slots[0]} ${potion.index} ${potion.screen} ${inventory.count("minecraft:potion")} ${inventory.slot(10).potion}`);
  log.info(`${sword.index} ${sword.enchantments["minecraft:sharpness"]} ${inventory.hotbar(10)}`);
}"#;
    let mut instance = instance_2(source);

    world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(world.log, ["41 5 10  5 minecraft:turtle_master", "36 5 "]);
}

#[test]
fn a_merchants_trades_read_as_maps_of_items() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let screen =
        open(&mut gate, ScreenKind::Merchant, 1, Vec::new()).with_offers([TradeOffer::new(
            item("minecraft:emerald", 3),
            None,
            item("minecraft:bread", 6),
            false,
        )]);
    world.menu.screen = Some(screen);
    let source = r"fn on_menu_open() {
  let trade = menu.trade(0);
  log.info(`${menu.trades} ${trade.cost_count} ${trade.cost_id} ${trade.second_id} ${trade.result_id} ${trade.result_count} ${trade.out_of_stock} ${menu.trade(1)}`);
}";
    let mut instance = instance_2(source);

    world.run(&mut instance, &HookEvent::MenuOpen);

    assert_eq!(
        world.log,
        ["1 3 minecraft:emerald  minecraft:bread 6 false "]
    );
}

#[test]
fn numbers_read_as_servers_write_them_with_either_decimal_mark() {
    let mut world = World::new();
    let source = r#"fn on_join(first) {
  log.info(`${menu.number("Price: 1,250.5 coins")} ${menu.number("2k")} ${menu.number("Seller: AfkBot1")} ${menu.number("1.234,5", ",")}`);
}"#;
    let mut instance = instance_2(source);

    world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(world.log, ["1250.5 2000.0  1234.5"]);
}

#[test]
fn a_decimal_mark_that_is_neither_a_point_nor_a_comma_stops_the_script() {
    let mut world = World::new();
    let mut instance = instance_2("fn on_join(first) {\n  menu.number(\"5\", \";\");\n}");

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(
        stopped_for(&ran.outcome),
        Some(&ScriptErrorKind::Misuse(Misuse::UnknownDecimalMark))
    );
}

// --- Clicks ---

#[test]
fn a_click_goes_out_as_the_screens_action_and_waits_for_its_fate() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.screen = Some(auction(&mut gate));
    let source = r"fn on_menu_open() {
  let first = menu.click(0);
  let second = menu.click(1);
  log.info(`${first.ok} ${second.ok} ${second.reason} ${menu.pending}`);
}
fn on_click(result) {
  log.info(`${result.ok} ${result.reason} ${result.slot} ${menu.pending}`);
}";
    let mut instance = instance_2(source);

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);
    world.now = time::add(world.now, Duration::from_millis(100));
    world.run(
        &mut instance,
        &HookEvent::Click(ClickOutcome {
            slot: Some(0),
            answer: Ok(()),
        }),
    );

    assert_eq!(clicks(&ran.actions), [(0, ClickKind::Left)]);
    assert_eq!(
        world.log,
        ["true false click_pending true", "true  0 false"]
    );
    assert!(!instance.awaits_fate());
}

#[rstest::rstest]
#[case::corrected(Refusal::Rejected, "rejected")]
#[case::a_result_slot(Refusal::Unclear, "unclear")]
#[case::replaced(Refusal::Replaced, "replaced")]
#[case::unanswered(Refusal::Unanswered, "unanswered")]
#[case::refused_at_sending(Refusal::ScreenChanged, "screen_changed")]
fn on_click_hears_every_fate_and_every_refusal_at_sending(
    #[case] answer: Refusal,
    #[case] reason: &str,
) {
    let mut world = World::new();
    let mut instance = instance_2(
        "fn on_click(result) { log.info(`${result.ok} ${result.reason} ${result.slot}`); }",
    );

    world.run(
        &mut instance,
        &HookEvent::Click(ClickOutcome {
            slot: None,
            answer: Err(answer),
        }),
    );

    assert_eq!(world.log, [format!("false {reason} ")]);
}

#[test]
fn every_kind_of_click_is_its_own_and_a_number_key_names_its_hotbar_slot() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.screen = Some(auction(&mut gate));
    let mut instance = instance_2(
        "fn on_click(result) {}\nfn on_menu_open() { log.info(`${menu.swap(0, 10).reason}`); }\nfn on_timer(name) {\n  if name == \"right\" { menu.right_click(1); }\n  if name == \"shift\" { menu.shift_click(1); }\n  if name == \"swap\" { menu.swap(2, 3); }\n  if name == \"drop\" { menu.drop(2); }\n  if name == \"stack\" { menu.drop_stack(2); }\n}",
    );
    let mut all = Vec::new();
    world.run(&mut instance, &HookEvent::MenuOpen);

    for name in ["right", "shift", "swap", "drop", "stack"] {
        world.now = time::add(world.now, Duration::from_millis(100));
        let ran = world.run(
            &mut instance,
            &HookEvent::Timer(crate::support::timer(name)),
        );
        all.extend(clicks(&ran.actions));
        world.run(
            &mut instance,
            &HookEvent::Click(ClickOutcome {
                slot: None,
                answer: Ok(()),
            }),
        );
    }

    assert_eq!(world.log, ["bad_value"]);
    assert_eq!(
        all,
        [
            (1, ClickKind::Right),
            (1, ClickKind::Shift),
            (2, ClickKind::NumberKey(HotbarSlot::try_from(2).unwrap())),
            (2, ClickKind::Drop),
            (2, ClickKind::DropStack),
        ]
    );
}

#[test]
fn a_slot_read_in_an_earlier_hook_is_clicked_only_where_this_hook_sees_the_same() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let screen = auction(&mut gate);
    world.menu.screen = Some(screen.clone());
    let source = r"fn on_menu_open() { state.chosen = menu.slot(0); }
fn on_timer(name) { log.info(`${menu.click(state.chosen).reason}`); }
fn on_click(result) {}";
    let mut instance = instance_2(source);
    world.run(&mut instance, &HookEvent::MenuOpen);

    // Another player bought the listing; a cheaper one took its slot.
    let mut changed: Vec<Option<Item>> = vec![Some(listing(400, "Someone"))];
    changed.resize(27, None);
    world.menu.screen = Some(Screen::new(
        screen.id(),
        StateId::new(2),
        ScreenKind::Generic9x3,
        "Auction House",
        changed,
        None,
    ));
    world.now = time::add(world.now, Duration::from_millis(500));
    let refused = world.run(
        &mut instance,
        &HookEvent::Timer(crate::support::timer("buy")),
    );

    // The same item on a new screen in the same window.
    let reopened = auction(&mut gate);
    world.menu.screen = Some(reopened);
    world.now = time::add(world.now, Duration::from_millis(500));
    let elsewhere = world.run(
        &mut instance,
        &HookEvent::Timer(crate::support::timer("buy")),
    );

    // Back as it was.
    world.menu.screen = Some(screen);
    world.now = time::add(world.now, Duration::from_millis(500));
    let taken = world.run(
        &mut instance,
        &HookEvent::Timer(crate::support::timer("buy")),
    );

    assert_eq!(clicks(&refused.actions), []);
    assert_eq!(clicks(&elsewhere.actions), []);
    assert_eq!(clicks(&taken.actions), [(0, ClickKind::Left)]);
    assert_eq!(world.log, ["slot_changed", "screen_changed", ""]);
}

#[test]
fn a_click_without_a_screen_on_no_slot_or_too_soon_is_refused_and_sends_nothing() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = instance_2(
        "fn on_join(first) { log.info(menu.click(0).reason); }\nfn on_menu_open() { log.info(`${menu.click(99).reason} ${menu.click(\"x\").reason} ${menu.click(0).reason}`); }\nfn on_click(result) { log.info(menu.click(1).reason); }",
    );
    let joined = world.run(&mut instance, &HookEvent::Join { first: true });
    world.menu.screen = Some(auction(&mut gate));
    let opened = world.run(&mut instance, &HookEvent::MenuOpen);
    // The fate comes 20 ms after the click: within the 50 ms a click waits.
    world.now = time::add(world.now, Duration::from_millis(20));
    let fate = world.run(
        &mut instance,
        &HookEvent::Click(ClickOutcome {
            slot: Some(0),
            answer: Ok(()),
        }),
    );

    assert_eq!(clicks(&joined.actions), []);
    assert_eq!(clicks(&opened.actions), [(0, ClickKind::Left)]);
    assert_eq!(clicks(&fate.actions), []);
    assert_eq!(world.log, ["no_menu", "bad_value bad_value ", "too_soon"]);
}

#[test]
fn a_dead_bot_clicks_nothing() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.screen = Some(auction(&mut gate));
    let mut instance = instance_2("fn on_death() { log.info(menu.click(0).reason); }");

    let ran = world.run(&mut instance, &HookEvent::Death);

    assert_eq!(clicks(&ran.actions), []);
    assert_eq!(world.log, ["dead"]);
}

#[test]
fn a_button_a_trade_and_a_name_go_out_on_their_own_screens_only() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let price = MessageDraft {
        key: "price".to_owned(),
        text: "{price}".to_owned(),
        values: vec![ValueDraft {
            name: "price".to_owned(),
            kind: ValueKind::Integer {
                min: 1,
                max: 1_000_000,
            },
        }],
    };
    let source = r#"fn on_menu_open() {
  log.info(`${menu.kind} ${menu.button(1).reason} ${menu.select_trade(0).reason} ${menu.rename("price", #{ price: 1499.9 }).reason}`);
}
fn on_click(result) {}"#;
    let mut instance = level_2(source, vec![price], Vec::new());
    let mut actions = Vec::new();

    for kind in [
        ScreenKind::Enchantment,
        ScreenKind::Merchant,
        ScreenKind::Anvil,
        ScreenKind::Generic9x1,
    ] {
        let screen = open(&mut gate, kind, 1, Vec::new());
        let screen = if kind == ScreenKind::Merchant {
            screen.with_offers([TradeOffer::new(
                item("minecraft:emerald", 1),
                None,
                item("minecraft:bread", 1),
                false,
            )])
        } else {
            screen
        };
        world.menu.screen = Some(screen);
        world.now = time::add(world.now, Duration::from_millis(100));
        let ran = world.run(&mut instance, &HookEvent::MenuOpen);
        actions.extend(menu_actions(&ran.actions).into_iter().cloned());
        world.run(
            &mut instance,
            &HookEvent::Click(ClickOutcome {
                slot: None,
                answer: Ok(()),
            }),
        );
    }

    assert_eq!(
        world.log,
        [
            "enchantment  bad_value bad_value",
            "merchant bad_value  bad_value",
            "anvil bad_value bad_value ",
            "generic_9x1 bad_value bad_value bad_value",
        ]
    );
    let names: Vec<&str> = actions
        .iter()
        .filter_map(|command| match command {
            MenuCommand::Act(MenuAction::Rename(rename)) => Some(rename.name().as_str()),
            _ => None,
        })
        .collect();
    // M15: a decimal for a whole number counts down.
    assert_eq!(names, ["1499"]);
    assert_eq!(actions.len(), 3);
}

#[test]
fn every_slot_of_a_full_screen_and_a_trade_with_the_most_a_server_sends_reads_within_the_limits() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let heavy = || {
        let many: Vec<(String, i32)> = (0..20)
            .map(|n| (format!("minecraft:e{n:02}"), 255))
            .collect();
        item("minecraft:enchanted_book", 64)
            .named(&"\u{1f600}".repeat(100))
            .with_lore((0..20).map(|_| "\u{1f600}".repeat(200)))
            .with_enchantments(many.iter().map(|(id, level)| (id.as_str(), *level)))
            .with_stored_enchantments(many.iter().map(|(id, level)| (id.as_str(), *level)))
    };
    let screen = open(&mut gate, ScreenKind::Merchant, 1, vec![Some(heavy()); 3])
        .with_offers((0..64).map(|_| TradeOffer::new(heavy(), Some(heavy()), heavy(), false)));
    world.menu.screen = Some(screen);
    let source = r"fn on_menu_open() {
  let n = 0;
  for i in menu.slots { let slot = menu.slot(i); n += slot.enchantments.len() + slot.stored_enchantments.len() + slot.lore.len(); }
  for i in 0..menu.trades { let trade = menu.trade(i); n += trade.result_stored_enchantments.len(); }
  state.kept = menu.slot(0).id;
  log.info(`${n}`);
}";
    let mut instance = instance_2(source);

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    // 3 slots of 16 enchantments of each kind and 1 line of lore (a name of
    // 64 four-byte characters takes 256 bytes, a line of 128 takes 512),
    // and 64 trades of 16.
    assert_eq!(world.log, [format!("{}", 3 * (32 + 1) + 64 * 16)]);
}

#[test]
fn a_name_from_a_message_the_mode_doesnt_declare_stops_the_script() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.screen = Some(open(&mut gate, ScreenKind::Anvil, 1, Vec::new()));
    let mut instance =
        instance_2("fn on_menu_open() {\n  menu.rename(\"price\", #{ price: 5 });\n}");

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert_eq!(
        stopped_for(&ran.outcome),
        Some(&ScriptErrorKind::Misuse(Misuse::UnknownMessage))
    );
}

#[test]
fn a_close_names_the_screen_the_hook_saw_and_frees_the_keys_for_the_rest_of_the_hook() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let screen = auction(&mut gate);
    world.menu.screen = Some(screen.clone());
    let source = r"fn on_menu_open() {
  log.info(`${keys.jump.tap().reason} ${mouse.right.hold().reason} ${look.at(0, 0).reason} ${hotbar.select(2).reason} ${inventory.open().reason}`);
  let closed = menu.close();
  log.info(`${closed.ok} ${menu.close().reason} ${keys.jump.tap().ok} ${menu.click(0).reason}`);
}";
    let mut instance = instance_2(source);

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert_eq!(
        world.log,
        [
            "screen_open screen_open screen_open screen_open screen_open",
            "true no_menu true no_menu",
        ]
    );
    assert_eq!(
        menu_actions(&ran.actions),
        [&MenuCommand::Close(screen.id())]
    );
}

#[test]
fn the_inventory_opens_only_with_no_screen_and_then_holds_the_keys_back() {
    let mut world = World::new();
    let mut instance = instance_2(
        "fn on_join(first) { log.info(`${inventory.open().ok} ${keys.forward.hold().reason}`); }",
    );

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(menu_actions(&ran.actions), [&MenuCommand::OpenInventory]);
    assert_eq!(world.log, ["true screen_open"]);
}

#[test]
fn a_release_is_never_refused_while_a_screen_is_open() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.screen = Some(auction(&mut gate));
    let mut instance =
        instance_2("fn on_menu_update() { log.info(`${mouse.right.release().ok}`); }");

    let ran = world.run(&mut instance, &HookEvent::MenuUpdate);

    assert_eq!(world.log, ["true"]);
    assert!(
        ran.actions
            .contains(&Action::Release(Input::Mouse(MouseButton::Right)))
    );
}

// --- A screen opening ---

/// A farm that holds the right button from `on_join`, at `level`.
fn farm(level: u8, source: &str) -> Instance {
    let draft = ScriptDraft {
        api: level,
        ..draft(source)
    };
    Instance::new(program_of(draft), 7, still())
}

const FARM: &str = "fn on_join(first) { mouse.right.hold(); keys.sneak.hold_for(60000); }";
const RIGHT: Input = Input::Mouse(MouseButton::Right);
const SNEAK: Input = Input::Key(Key::Sneak);

#[test]
fn a_level_1_farm_has_a_servers_screen_closed_and_its_buttons_pressed_again() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = farm(1, FARM);
    world.run(&mut instance, &HookEvent::Join { first: true });
    let screen = auction(&mut gate);
    world.menu.screen = Some(screen.clone());
    world.now = time::add(world.now, Duration::from_secs(20));

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert_eq!(ran.outcome, Outcome::NotDefined);
    assert_eq!(
        ran.actions,
        [
            Action::Menu(MenuCommand::Close(screen.id())),
            Action::Press {
                input: RIGHT,
                kind: PressKind::Hold
            },
            Action::Press {
                input: SNEAK,
                kind: PressKind::Hold
            },
        ]
    );
    assert!(instance.is_held(RIGHT));
    assert!(instance.is_held(SNEAK));
    // The sneak's hold ends when it was to end: 40 s on.
    assert_eq!(
        instance.next_wake(),
        Some(time::add(world.now, Duration::from_secs(40)))
    );
}

#[test]
fn a_level_1_script_hears_the_screen_as_a_let_go_even_when_a_press_is_refused() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = farm(1, "fn on_join(first) { mouse.right.hold(); }");
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.menu.screen = Some(auction(&mut gate));
    // 100 ms on: the use button's hold waits 500 ms between presses.
    world.now = time::add(world.now, Duration::from_millis(100));

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert!(!instance.is_held(RIGHT));
    assert_eq!(ran.refused.len(), 1);
    assert_eq!(
        ran.refused.first().map(|refused| refused.reason),
        Some(Refusal::TooSoon)
    );
}

#[test]
fn a_hold_until_used_is_pressed_again_as_one_and_an_ended_hold_for_isnt() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = farm(
        1,
        "fn on_join(first) { mouse.right.hold_until_used(); keys.forward.hold_for(1000); }",
    );
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.menu.screen = Some(auction(&mut gate));
    world.now = time::add(world.now, Duration::from_millis(990));

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    let presses: Vec<&Action> = ran
        .actions
        .iter()
        .filter(|action| matches!(action, Action::Press { .. }))
        .collect();
    assert_eq!(
        presses,
        [&Action::Press {
            input: RIGHT,
            kind: PressKind::HoldUntilUsed
        }]
    );
}

#[test]
fn a_level_2_script_without_on_menu_open_is_treated_as_level_1s() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = farm(2, FARM);
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.menu.screen = Some(auction(&mut gate));
    world.now = time::add(world.now, Duration::from_secs(20));

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert!(matches!(
        ran.actions.first(),
        Some(Action::Menu(MenuCommand::Close(_)))
    ));
    assert!(instance.is_held(RIGHT));
}

#[test]
fn a_level_2_script_with_on_menu_open_hears_the_let_go_and_handles_the_screen() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = farm(
        2,
        "fn on_join(first) { mouse.right.hold(); }\nfn on_menu_open() { log.info(`${mouse.right.held}`); }",
    );
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.menu.screen = Some(auction(&mut gate));

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert_eq!(world.log, ["false"]);
    assert_eq!(menu_actions(&ran.actions), Vec::<&MenuCommand>::new());
    assert!(!instance.is_held(RIGHT));
}

#[test]
fn the_bots_own_inventory_is_never_closed_for_a_script_without_the_hook() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = farm(2, "fn on_join(first) { inventory.open(); }");
    world.run(&mut instance, &HookEvent::Join { first: true });
    let id: ScreenId = gate.opened(WindowId::INVENTORY);
    world.menu.screen = Some(Screen::new(
        id,
        StateId::new(0),
        ScreenKind::Inventory,
        "",
        Vec::new(),
        None,
    ));

    let ran = world.run(&mut instance, &HookEvent::MenuOpen);

    assert_eq!(menu_actions(&ran.actions), Vec::<&MenuCommand>::new());
}

#[test]
fn the_menus_other_hooks_run_when_their_events_come() {
    let mut world = World::new();
    let mut instance = instance_2(
        "fn on_menu_update() { log.info(\"update\"); }\nfn on_menu_close() { log.info(\"close\"); }\nfn on_sign_open() { log.info(\"sign\"); }",
    );

    for event in [
        HookEvent::MenuUpdate,
        HookEvent::MenuClose,
        HookEvent::SignOpen,
    ] {
        world.run(&mut instance, &event);
    }

    assert_eq!(world.log, ["update", "close", "sign"]);
    assert_eq!(Hook::Click.parameters(), ["result"]);
}

#[test]
fn a_fate_that_never_came_before_a_disconnect_lets_the_next_click_go() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.screen = Some(auction(&mut gate));
    let mut instance = instance_2("fn on_menu_open() { menu.click(0); }");
    world.run(&mut instance, &HookEvent::MenuOpen);
    assert!(instance.awaits_fate());

    instance.disconnected();

    assert!(!instance.awaits_fate());
}

// --- Signs ---

fn price_sign() -> SignDraft {
    SignDraft {
        key: "price".to_owned(),
        lines: vec!["{price}".to_owned()],
        values: vec![ValueDraft {
            name: "price".to_owned(),
            kind: ValueKind::Integer {
                min: 1,
                max: 1_000_000,
            },
        }],
    }
}

#[test]
fn a_sign_is_answered_with_a_declared_sign_once_and_reads_where_it_is() {
    let mut world = World::new();
    let mut desk = SignDesk::new();
    let editor = desk.opened(BlockPosition { x: 3, y: 64, z: -2 }, SignSide::Front);
    world.menu.sign = Some(editor);
    let source = r#"fn on_sign_open() {
  log.info(`${sign.open} ${sign.x} ${sign.y} ${sign.z} ${sign.side}`);
  log.info(`${sign.write("price", #{ price: 250 }).ok} ${sign.cancel().reason}`);
}"#;
    let mut instance = level_2(source, Vec::new(), vec![price_sign()]);

    let ran = world.run(&mut instance, &HookEvent::SignOpen);

    assert_eq!(world.log, ["true 3 64 -2 front", "true sign_answered"]);
    let answers: Vec<&MenuCommand> = menu_actions(&ran.actions);
    let [
        MenuCommand::AnswerSign {
            editor: answered,
            answer: SignAnswer::Write(text),
        },
    ] = answers.as_slice()
    else {
        panic!("{answers:?}");
    };
    assert_eq!(*answered, editor);
    assert_eq!(text.lines(), ["250", "", "", ""]);
}

#[test]
fn a_sign_without_an_editor_with_a_bad_value_or_too_soon_is_refused() {
    let mut world = World::new();
    let mut desk = SignDesk::new();
    let source = r#"fn on_join(first) { log.info(sign.cancel().reason); }
fn on_sign_open() { log.info(`${sign.write("price", #{ price: 0 }).reason} ${sign.cancel().ok}`); }
fn on_timer(name) { log.info(sign.cancel().reason); }"#;
    let mut instance = level_2(source, Vec::new(), vec![price_sign()]);
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.menu.sign = Some(desk.opened(BlockPosition { x: 0, y: 0, z: 0 }, SignSide::Back));
    world.run(&mut instance, &HookEvent::SignOpen);
    world.now = time::add(world.now, Duration::from_millis(10));
    world.run(
        &mut instance,
        &HookEvent::Timer(crate::support::timer("again")),
    );

    assert_eq!(world.log, ["no_sign", "bad_value true", "too_soon"]);
}

#[test]
fn a_sign_the_mode_doesnt_declare_stops_the_script() {
    let mut world = World::new();
    let mut instance =
        instance_2("fn on_sign_open() {\n  sign.write(\"price\", #{ price: 5 });\n}");

    let ran = world.run(&mut instance, &HookEvent::SignOpen);

    assert_eq!(
        stopped_for(&ran.outcome),
        Some(&ScriptErrorKind::Misuse(Misuse::UnknownSign))
    );
}

#[test]
fn the_screens_refusals_read_as_the_scripts_reasons() {
    for refusal in MenuRefusal::ALL {
        let outcome = ClickOutcome::refused(Some(1), refusal);
        assert_eq!(
            outcome.answer.map_err(Refusal::as_str),
            Err(refusal.as_str())
        );
    }
    let _ = logged(&[]);
}
