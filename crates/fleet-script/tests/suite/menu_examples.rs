//! The five menu examples, run as written against the fake world through
//! the screens a server shows them (Plan.md P16.12; ADR-0036, M13, M17;
//! ADR-0052). The harness's screens are copied: their titles, their
//! buttons' slots and items.

use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use fleet_core::mc::MenuCommand;
use fleet_core::mode::HotbarSlot;
use fleet_core::script::menu::{
    BotInventory, ClickGate, ClickKind, Item, ItemData, MenuAction, Screen, ScreenKind, StateId,
    WindowId,
};
use fleet_core::script::{ClickOutcome, HookEvent, Input, MouseButton, Script};
use fleet_script::{Action, EXAMPLES, Instance, PressKind, compile};

use crate::support::{World, id};

fn instance_of(name: &str) -> Instance {
    let example = EXAMPLES
        .iter()
        .find(|example| example.id == name)
        .expect("an example of that name");
    let script = Script::try_from(example.draft()).unwrap();
    Instance::new(
        Arc::new(compile(&script).unwrap()),
        7,
        crate::support::still(),
    )
}

fn item(kind: &str, count: u32) -> Item {
    Item::new(
        id(kind),
        NonZeroU32::new(count).unwrap(),
        ItemData::new(Vec::new()),
    )
}

fn named(kind: &str, name: &str) -> Item {
    item(kind, 1).named(name)
}

fn turtle_master() -> Item {
    item("minecraft:potion", 1).with_potion(Some("minecraft:turtle_master"))
}

/// An auction listing as the harness draws it.
fn listing(kind: &str, price: u32, seller: &str) -> Item {
    let base = if kind == "minecraft:potion" {
        turtle_master()
    } else {
        item(kind, 1)
    };
    base.with_lore([
        format!("Price: ${price}.00"),
        format!("Seller: {seller}"),
        "Expires in: 59m 0s".to_owned(),
    ])
}

/// The screen `kind` titled `title`, opened anew in `gate`, with `items` in
/// its numbering (its own slots first, then the bot's 36).
fn screen(gate: &mut ClickGate, kind: ScreenKind, title: &str, items: Vec<Option<Item>>) -> Screen {
    let id = gate.opened(WindowId::new(1));
    Screen::new(id, StateId::new(1), kind, title, items, None)
}

/// `items` at their slots, nothing elsewhere, `len` slots in all.
fn laid_out(len: usize, items: &[(usize, Item)]) -> Vec<Option<Item>> {
    let mut slots = vec![None; len];
    for (slot, item) in items {
        slots[*slot] = Some(item.clone());
    }
    slots
}

/// The bot's inventory with `items` at window 0's slots.
fn inventory(items: &[(usize, Item)]) -> BotInventory {
    BotInventory::new(laid_out(46, items))
}

/// Opens `screen` for the script, then lets its first step's wait pass.
fn show(world: &mut World, instance: &mut Instance, screen: Screen) {
    world.menu.screen = Some(screen);
    world.run(instance, &HookEvent::MenuOpen);
    step(world, instance);
}

/// Lets a step's wait pass: a second, longer than any 300 to 900 ms.
fn step(world: &mut World, instance: &mut Instance) {
    world.pass(instance, Duration::from_secs(1));
}

/// Tells the script its last action's fate came, `ok`, and lets the next
/// step's wait pass.
fn answer(world: &mut World, instance: &mut Instance) {
    world.run(
        instance,
        &HookEvent::Click(ClickOutcome {
            slot: None,
            answer: Ok(()),
        }),
    );
    step(world, instance);
}

/// The screen commands since `from` in what the world performed.
fn commands(world: &World, from: usize) -> Vec<MenuCommand> {
    world
        .performed
        .iter()
        .skip(from)
        .filter_map(|action| match action {
            Action::Menu(command) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

/// The clicks since `from`: slot and kind.
fn clicks(world: &World, from: usize) -> Vec<(u16, ClickKind)> {
    commands(world, from)
        .into_iter()
        .filter_map(|command| match command {
            MenuCommand::Act(MenuAction::Click(click)) => Some((click.slot(), click.kind())),
            _ => None,
        })
        .collect()
}

fn closes(world: &World, from: usize) -> usize {
    commands(world, from)
        .iter()
        .filter(|command| matches!(command, MenuCommand::Close(_)))
        .count()
}

/// The harness's sell menu: its bottom row of panes, and Sell at 49. The
/// bot's 36 slots hold `own` at window 0's numbering.
fn sell_menu(gate: &mut ClickGate, own: &[(usize, Item)]) -> Screen {
    let mut items: Vec<(usize, Item)> = (45..54)
        .map(|slot| (slot, named("minecraft:black_stained_glass_pane", " ")))
        .collect();
    items.push((49, named("minecraft:emerald", "Sell")));
    // Window 0's 9 to 44 are this screen's 54 to 89.
    items.extend(own.iter().map(|(slot, item)| (slot + 45, item.clone())));
    screen(
        gate,
        ScreenKind::Generic9x6,
        "Sell Items",
        laid_out(90, &items),
    )
}

// --- The sell macro ---

#[test]
fn the_sell_macro_moves_every_potion_in_presses_sell_and_closes() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.inventory = inventory(&[
        (10, turtle_master()),
        (36, item("minecraft:diamond_sword", 1)),
    ]);
    let mut instance = instance_of("sell_macro");
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.pass(&mut instance, Duration::from_secs(60));
    assert_eq!(world.sent, ["/sell"]);

    let from = world.performed.len();
    show(
        &mut world,
        &mut instance,
        sell_menu(
            &mut gate,
            &[
                (10, turtle_master()),
                (36, item("minecraft:diamond_sword", 1)),
            ],
        ),
    );
    let menu = world.menu.screen.clone().unwrap();
    // The potion went in; the sword stays.
    world.menu.screen =
        Some(sell_menu(&mut gate, &[(36, item("minecraft:diamond_sword", 1))]).clone());
    let _ = menu;
    answer(&mut world, &mut instance);
    answer(&mut world, &mut instance);

    assert_eq!(
        clicks(&world, from),
        [(55, ClickKind::Shift), (49, ClickKind::Left)]
    );
    assert_eq!(closes(&world, from), 1);
}

// --- The auction scan ---

fn auction_house(gate: &mut ClickGate, listings: &[(usize, Item)]) -> Screen {
    let mut items: Vec<(usize, Item)> = listings.to_vec();
    items.extend((45..54).map(|slot| (slot, named("minecraft:black_stained_glass_pane", " "))));
    items.push((47, named("minecraft:emerald", "Create")));
    screen(
        gate,
        ScreenKind::Generic9x6,
        "Auction House",
        laid_out(54, &items),
    )
}

fn confirm_screen(gate: &mut ClickGate, title: &str, shown: Item) -> Screen {
    let mut items: Vec<(usize, Item)> = (0..27)
        .map(|slot| (slot, named("minecraft:black_stained_glass_pane", " ")))
        .collect();
    items.push((11, named("minecraft:lime_stained_glass_pane", "Confirm")));
    items.push((13, shown));
    items.push((15, named("minecraft:red_stained_glass_pane", "Cancel")));
    screen(gate, ScreenKind::Generic9x3, title, laid_out(27, &items))
}

#[test]
fn the_auction_scan_buys_the_cheapest_within_budget_once_and_judges_by_the_inventory() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = instance_of("auction_scan");
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.pass(&mut instance, Duration::from_secs(30));
    assert_eq!(world.sent, ["/ah"]);

    let from = world.performed.len();
    show(
        &mut world,
        &mut instance,
        auction_house(
            &mut gate,
            &[
                (0, listing("minecraft:diamond", 2000, "Expensive")),
                (1, listing("minecraft:diamond", 1400, "Seller1")),
                (2, listing("minecraft:diamond", 1200, "Seller2")),
                (3, listing("minecraft:emerald", 10, "Bait")),
            ],
        ),
    );
    let picked = clicks(&world, from);
    // The plugin opens the confirm on its next tick: the fate first.
    answer(&mut world, &mut instance);
    let shown = listing("minecraft:diamond", 1200, "Seller2").with_lore([
        "Price: $1200.00",
        "Seller: Seller2",
        "Expires in: 58m 59s",
    ]);
    show(
        &mut world,
        &mut instance,
        confirm_screen(&mut gate, "Confirm Purchase", shown),
    );
    let confirmed = clicks(&world, from);
    // The fate of the confirm, and more time: never a second click.
    answer(&mut world, &mut instance);
    world.pass(&mut instance, Duration::from_secs(5));
    let after = clicks(&world, from);
    // The plugin bought and closed the screen: a diamond more.
    world.menu.inventory = inventory(&[(36, item("minecraft:diamond", 1))]);
    world.menu.screen = None;
    world.run(&mut instance, &HookEvent::MenuClose);

    assert_eq!(picked, [(2, ClickKind::Left)]);
    assert_eq!(confirmed, [(2, ClickKind::Left), (11, ClickKind::Left)]);
    assert_eq!(after, confirmed);
    assert_eq!(world.log, ["Bought 1 diamonds for 1200.0."]);
}

#[test]
fn the_auction_scan_confirms_nothing_but_the_listing_it_picked() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = instance_of("auction_scan");
    world.run(&mut instance, &HookEvent::Join { first: true });
    show(
        &mut world,
        &mut instance,
        auction_house(
            &mut gate,
            &[(0, listing("minecraft:diamond", 900, "Seller1"))],
        ),
    );
    answer(&mut world, &mut instance);
    let from = world.performed.len();

    // Another player bought it; the confirm shows another seller's.
    show(
        &mut world,
        &mut instance,
        confirm_screen(
            &mut gate,
            "Confirm Purchase",
            listing("minecraft:diamond", 900, "Seller9"),
        ),
    );

    assert_eq!(clicks(&world, from), []);
    assert_eq!(closes(&world, from), 1);
    assert_eq!(world.log, ["Not the listing picked; nothing bought."]);
}

// --- The farm that sells ---

#[test]
fn the_farm_that_sells_sells_its_potions_drops_its_junk_and_nothing_else_then_farms_on() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let sword = item("minecraft:diamond_sword", 1);
    let junk = item("minecraft:rotten_flesh", 12);
    let mut instance = instance_of("farm_that_sells");
    let joined = world.run(&mut instance, &HookEvent::Join { first: true });
    world.pass(&mut instance, Duration::from_secs(300));
    let release = world
        .performed
        .contains(&Action::Release(Input::Mouse(MouseButton::Right)));

    let from = world.performed.len();
    show(
        &mut world,
        &mut instance,
        sell_menu(
            &mut gate,
            &[
                (10, turtle_master()),
                (11, junk.clone()),
                (36, sword.clone()),
            ],
        ),
    );
    world.menu.screen = Some(sell_menu(&mut gate, &[(11, junk), (36, sword.clone())]));
    answer(&mut world, &mut instance);
    world.menu.screen = Some(sell_menu(&mut gate, &[(36, sword.clone())]));
    answer(&mut world, &mut instance);
    answer(&mut world, &mut instance);
    let closed = closes(&world, from);
    world.menu.screen = None;
    let farm_again = world.run(&mut instance, &HookEvent::MenuClose);

    let farm = [
        Action::SelectHotbar(HotbarSlot::FIRST),
        Action::Press {
            input: Input::Mouse(MouseButton::Right),
            kind: PressKind::Hold,
        },
    ];
    assert!(joined.actions.ends_with(&farm), "{:?}", joined.actions);
    assert!(release);
    assert_eq!(world.sent, ["/sell"]);
    assert_eq!(
        clicks(&world, from),
        [
            (55, ClickKind::Shift),
            (56, ClickKind::DropStack),
            (49, ClickKind::Left)
        ]
    );
    assert_eq!(closed, 1);
    assert_eq!(farm_again.actions, farm);
}

/// The farm's first sell comes 30 to 60 s after a join, then every 5
/// minutes from the last: a farm kicked more often than every 5 minutes
/// still sells (found live, Plan.md P16.17). Here the sell menu never
/// opens, so each sell is only its command.
#[test]
fn the_farms_first_sell_comes_soon_after_a_join_then_every_5_minutes() {
    let mut world = World::new();
    let mut instance = instance_of("farm_that_sells");
    world.run(&mut instance, &HookEvent::Join { first: true });

    world.pass(&mut instance, Duration::from_millis(29_999));
    let before_30_s = world.sent.len();
    world.pass(&mut instance, Duration::from_millis(30_001));
    let by_60_s = world.sent.len();
    world.pass(&mut instance, Duration::from_secs(240));
    let within_5_minutes = world.sent.len();
    world.pass(&mut instance, Duration::from_secs(60));

    assert_eq!((before_30_s, by_60_s, within_5_minutes), (0, 1, 1));
    assert_eq!(world.sent, ["/sell", "/sell"]);
}

#[test]
fn the_farm_that_sells_closes_a_screen_it_didnt_open() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = instance_of("farm_that_sells");
    world.run(&mut instance, &HookEvent::Join { first: true });
    let from = world.performed.len();

    world.menu.screen = Some(auction_house(&mut gate, &[]));
    world.run(&mut instance, &HookEvent::MenuOpen);

    assert_eq!(closes(&world, from), 1);
    assert_eq!(world.log, ["Closed a screen it didn't open: Auction House"]);
}

// --- The auction lister ---

fn potions_listed(gate: &mut ClickGate, prices: &[(u32, &str)]) -> Screen {
    let listings: Vec<(usize, Item)> = prices
        .iter()
        .enumerate()
        .map(|(slot, (price, seller))| (slot, listing("minecraft:potion", *price, seller)))
        .collect();
    auction_house(gate, &listings)
}

#[test]
fn the_auction_lister_lists_at_the_middle_of_the_three_cheapest_of_others_and_collects() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    // The potion in hotbar slot 3: window 0's 38.
    world.menu.inventory = inventory(&[(38, turtle_master())]);
    let mut instance = instance_of("auction_lister");
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.pass(&mut instance, Duration::from_secs(600));
    assert_eq!(world.sent, ["/ah"]);

    let from = world.performed.len();
    show(
        &mut world,
        &mut instance,
        potions_listed(
            &mut gate,
            &[
                (500, "Seller1"),
                (300, "AfkBot1"),
                (450, "Seller2"),
                (400, "Seller3"),
                (999, "Seller4"),
            ],
        ),
    );
    let selected = world
        .performed
        .contains(&Action::SelectHotbar(HotbarSlot::try_from(2).unwrap()));
    world.menu.screen = None;
    show(
        &mut world,
        &mut instance,
        confirm_screen(
            &mut gate,
            "Confirm Listing",
            turtle_master().with_lore(["Price: $450.00"]),
        ),
    );
    answer(&mut world, &mut instance);
    let listed = clicks(&world, from);
    world.menu.screen = None;
    world.run(&mut instance, &HookEvent::MenuClose);
    let expired_from = world.performed.len();
    show(
        &mut world,
        &mut instance,
        auction_house_titled(
            &mut gate,
            "Expired Listings",
            &[(0, turtle_master()), (1, turtle_master())],
        ),
    );
    answer(&mut world, &mut instance);

    assert!(selected);
    assert_eq!(world.sent, ["/ah", "/ah sell 450", "/ah expired"]);
    assert_eq!(listed, [(11, ClickKind::Left)]);
    assert_eq!(
        clicks(&world, expired_from),
        [(0, ClickKind::Left), (0, ClickKind::Left)]
    );
}

fn auction_house_titled(gate: &mut ClickGate, title: &str, items: &[(usize, Item)]) -> Screen {
    let mut items: Vec<(usize, Item)> = items.to_vec();
    items.extend((45..54).map(|slot| (slot, named("minecraft:black_stained_glass_pane", " "))));
    screen(gate, ScreenKind::Generic9x6, title, laid_out(54, &items))
}

#[test]
fn a_bait_below_the_floor_never_sets_the_listers_price() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.inventory = inventory(&[(36, turtle_master())]);
    let mut instance = instance_of("auction_lister");
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.pass(&mut instance, Duration::from_secs(600));

    show(
        &mut world,
        &mut instance,
        potions_listed(
            &mut gate,
            &[
                (10, "Bait1"),
                (20, "Bait2"),
                (30, "Bait3"),
                (450, "Seller1"),
            ],
        ),
    );

    assert_eq!(world.sent, ["/ah"]);
    assert_eq!(world.log, ["Not listed at 20.0: below the floor."]);
}

/// A lister's first listing comes 10 to 30 s after a join, then every 10
/// minutes from the last try: a bot kicked more often than every 10
/// minutes still lists (found live, Plan.md P16.17). Each try here finds
/// fewer than three potions listed, so it ends at once.
#[rstest::rstest]
#[case::the_lister(
    "auction_lister",
    "Fewer than three potions listed by others; nothing listed."
)]
#[case::the_anvil_lister(
    "auction_lister_anvil",
    "Listing stopped: fewer than three potions listed by others."
)]
fn a_listers_first_try_comes_soon_after_a_join_then_every_10_minutes(
    #[case] name: &str,
    #[case] nothing: &str,
) {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.inventory = inventory(&[(36, turtle_master())]);
    let mut instance = instance_of(name);
    world.run(&mut instance, &HookEvent::Join { first: true });

    world.pass(&mut instance, Duration::from_millis(9_999));
    let before_10_s = world.sent.len();
    world.pass(&mut instance, Duration::from_millis(20_001));
    let by_30_s = world.sent.len();
    show(
        &mut world,
        &mut instance,
        potions_listed(&mut gate, &[(450, "Seller1")]),
    );
    world.menu.screen = None;
    world.run(&mut instance, &HookEvent::MenuClose);
    world.pass(&mut instance, Duration::from_secs(570));
    let within_10_minutes = world.sent.len();
    world.pass(&mut instance, Duration::from_secs(30));

    assert_eq!((before_10_s, by_30_s), (0, 1));
    assert_eq!(within_10_minutes, 1);
    assert_eq!(world.sent, ["/ah", "/ah"]);
    assert_eq!(world.log, [nothing]);
}

// --- The auction lister on the anvil's flow ---

/// The user's server's auction house: listings, a trap renamed "Create"
/// in the rows above the buttons, and Create at the last row's third slot.
fn house_with_trap(gate: &mut ClickGate) -> Screen {
    auction_house(
        gate,
        &[
            (0, listing("minecraft:potion", 500, "Seller1")),
            (1, listing("minecraft:potion", 450, "Seller2")),
            (2, listing("minecraft:potion", 300, "AfkBot1")),
            (
                3,
                named("minecraft:emerald", "Create").with_lore(["Price: $1.00", "Seller: Trap"]),
            ),
            (4, listing("minecraft:potion", 400, "Seller3")),
        ],
    )
}

fn price_anvil(gate: &mut ClickGate) -> Screen {
    screen(
        gate,
        ScreenKind::Anvil,
        "Preis eingeben",
        laid_out(
            3,
            &[
                (0, named("minecraft:paper", "Preis")),
                (2, named("minecraft:lime_dye", "Bestätigen")),
            ],
        ),
    )
}

/// The flow up to the anvil, its price named: what was clicked.
fn up_to_the_anvil(
    world: &mut World,
    instance: &mut Instance,
    gate: &mut ClickGate,
) -> Vec<(u16, ClickKind)> {
    // The potion in the main inventory, window 0's 20.
    world.menu.inventory = inventory(&[(20, turtle_master())]);
    world.run(instance, &HookEvent::Join { first: true });
    world.pass(instance, Duration::from_secs(30));
    let from = world.performed.len();
    show(world, instance, house_with_trap(gate));
    answer(world, instance);
    // Create Listing: the potion is the screen's 27 + (20 - 9) = 38.
    let create = screen(
        gate,
        ScreenKind::Generic9x3,
        "Create Listing",
        laid_out(
            63,
            &[
                (13, named("minecraft:paper", "Select an item")),
                (38, turtle_master()),
            ],
        ),
    );
    show(world, instance, create);
    answer(world, instance);
    show(
        world,
        instance,
        confirm_screen(gate, "Confirm Item", turtle_master()),
    );
    answer(world, instance);
    show(world, instance, price_anvil(gate));
    clicks(world, from)
}

#[test]
fn the_anvil_lister_never_clicks_the_trap_finds_the_potion_and_names_the_price() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = instance_of("auction_lister_anvil");

    let clicked = up_to_the_anvil(&mut world, &mut instance, &mut gate);
    let from = world.performed.len();
    let named_price = commands(&world, 0)
        .into_iter()
        .find_map(|command| match command {
            MenuCommand::Act(MenuAction::Rename(rename)) => Some(rename.name().as_str().to_owned()),
            _ => None,
        });
    answer(&mut world, &mut instance);
    let confirmed = clicks(&world, from);
    world.menu.screen = None;
    show(
        &mut world,
        &mut instance,
        confirm_screen(
            &mut gate,
            "Confirm Listing",
            turtle_master().with_lore(["Price: $450.00"]),
        ),
    );
    answer(&mut world, &mut instance);
    let last = clicks(&world, from);
    world.menu.screen = None;
    world.run(&mut instance, &HookEvent::MenuClose);
    world.pass(&mut instance, Duration::from_secs(120));

    // Create by its slot (47), never the trap (3); the potion where it is.
    assert_eq!(
        clicked,
        [
            (47, ClickKind::Left),
            (38, ClickKind::Left),
            (11, ClickKind::Left)
        ]
    );
    assert_eq!(named_price.as_deref(), Some("450"));
    assert_eq!(confirmed, [(2, ClickKind::Left)]);
    assert_eq!(last, [(2, ClickKind::Left), (11, ClickKind::Left)]);
    assert_eq!(world.sent, ["/ah"]);
    // The watchdog was cancelled: one line, the listing's.
    assert_eq!(world.log, ["Listed a potion for 450.0."]);
}

#[test]
fn the_anvil_lister_stops_when_the_anvil_keeps_its_price() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    let mut instance = instance_of("auction_lister_anvil");
    up_to_the_anvil(&mut world, &mut instance, &mut gate);
    answer(&mut world, &mut instance);
    let from = world.performed.len();

    // The plugin refused the price: no screen follows, the anvil stays.
    world.pass(&mut instance, Duration::from_secs(4));

    assert_eq!(closes(&world, from), 1);
    assert_eq!(world.log, ["Listing stopped: the price was refused."]);
}

#[test]
fn the_anvil_lister_closes_an_unexpected_screen_and_its_watchdog_ends_a_stuck_listing() {
    let mut gate = ClickGate::new();
    let mut world = World::new();
    world.menu.inventory = inventory(&[(20, turtle_master())]);
    let mut instance = instance_of("auction_lister_anvil");
    world.run(&mut instance, &HookEvent::Join { first: true });
    world.pass(&mut instance, Duration::from_secs(30));
    let from = world.performed.len();
    show(&mut world, &mut instance, sell_menu(&mut gate, &[]));
    let unexpected = closes(&world, from);

    // A new listing 10 minutes after the first try, which never gets a
    // screen: the watchdog ends it a minute later, before the next try.
    world.menu.screen = None;
    world.pass(&mut instance, Duration::from_secs(661));

    assert_eq!(unexpected, 1);
    assert_eq!(
        world.log,
        [
            "Listing stopped: an unexpected screen, Sell Items.",
            "Listing stopped: it took longer than a minute."
        ]
    );
    assert_eq!(world.sent, ["/ah", "/ah"]);
}

// --- The operation budget on real pages (Plan.md P16.14) ---

/// A full page of the harness's auction house: 45 listings with their
/// price, seller and time left, and its buttons.
fn full_page(gate: &mut ClickGate, kind: &str) -> Screen {
    let listings: Vec<(usize, Item)> = (0..45)
        .map(|slot| {
            let price = u32::try_from(slot).unwrap() * 37 + 400;
            (slot, listing(kind, price, &format!("Seller{slot}")))
        })
        .collect();
    auction_house(gate, &listings)
}

/// The operations the hook that reads `screen` ran for the example `name`:
/// the step a timer of `timer`'s name runs after the screen opened.
fn operations_on(name: &str, screen: Screen, inventory_items: &[(usize, Item)]) -> u64 {
    let mut world = World::new();
    world.menu.inventory = inventory(inventory_items);
    let mut instance = instance_of(name);
    world.run(&mut instance, &HookEvent::Join { first: true });
    // Its own timer first: a listing starts with the auction house.
    world.pass(&mut instance, Duration::from_secs(30));
    world.menu.screen = Some(screen);
    world.run(&mut instance, &HookEvent::MenuOpen);
    let wakes = world.pass(&mut instance, Duration::from_secs(1));
    wakes
        .iter()
        .filter_map(|ran| match ran.outcome {
            fleet_script::Outcome::Ran { operations } => Some(operations),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn reading_a_full_page_takes_each_example_well_within_a_hooks_operations() {
    let mut gate = ClickGate::new();
    let scan = operations_on(
        "auction_scan",
        full_page(&mut gate, "minecraft:diamond"),
        &[],
    );
    let lister = operations_on(
        "auction_lister",
        full_page(&mut gate, "minecraft:potion"),
        &[(36, turtle_master())],
    );
    let anvil = operations_on(
        "auction_lister_anvil",
        full_page(&mut gate, "minecraft:potion"),
        &[(20, turtle_master())],
    );
    eprintln!(
        "MEASURE a full page's read: scan {scan}, lister {lister}, anvil lister {anvil} operations"
    );

    // A hook may run 10,000; a page's read stays under a third of it.
    for operations in [scan, lister, anvil] {
        assert!(operations > 500, "{operations}: the page wasn't read");
        assert!(operations < 3_300, "{operations}");
    }
}
