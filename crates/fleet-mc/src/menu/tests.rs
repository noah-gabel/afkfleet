//! A bot's screens, clicks and sign editors on its App, with azalea's own
//! packet handlers and the session's real bridge behind them (Plan.md
//! P16.8 to P16.10; ADR-0051).

use core::num::NonZeroUsize;
use core::time::Duration;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use azalea::app::App;
use azalea::core::registry_holder::RegistryHolder;
use azalea::core::tick::GameTick;
use azalea::ecs::entity::Entity;
use azalea::ecs::observer::On;
use azalea::entity::inventory::Inventory;
use azalea::entity::{LocalEntity, PlayerAbilities};
use azalea::inventory::{ItemStack, components};
use azalea::packet::game::{ReceiveGamePacketEvent, SendGamePacketEvent, process_packet};
use azalea::protocol::packets::game::c_merchant_offers::{
    DataComponentExactPredicate, ItemCost, MerchantOffer,
};
use azalea::protocol::packets::game::s_client_command::Action as ClientCommand;
use azalea::protocol::packets::game::{
    ClientboundAwardStats, ClientboundContainerClose, ClientboundContainerSetContent,
    ClientboundContainerSetSlot, ClientboundGamePacket, ClientboundMerchantOffers,
    ClientboundOpenScreen, ClientboundOpenSignEditor, ClientboundSetCursorItem,
    ClientboundSetPlayerInventory, ServerboundGamePacket,
};
use azalea::registry::DataRegistry as _;
use azalea::registry::builtin::{ItemKind, MenuKind, Potion};
use azalea::world::{WorldName, Worlds};
use azalea::{BlockPos, Event};
use azalea_chat::FormattedText;
use fleet_core::mc::{MenuCommand, MenuError, MenuView, SessionEvent};
use fleet_core::mode::{GameAction, HotbarSlot};
use fleet_core::script::menu::{
    ButtonPress, Click, ClickFate, ClickKind, ClickResult, MenuAction, MenuRefusal, Rename, Screen,
    ScreenKind, SignAnswer, SignRefusal, SignSide, TradeChoice,
};
use fleet_core::script::{Input, Key, MouseButton};
use rstest::rstest;

use super::MenuPlugin;
use super::send::perform;
use crate::events::{EventSink, LivenessStamps, McEvents, bridge};
use crate::script::{Held, ScriptPlugin};

/// A chest's window, as a server numbers it.
const CHEST: i32 = 1;
/// Where the bot's own 36 slots start in a chest of three rows.
const CHEST_PLAYER: u16 = 27;
/// Where its hotbar starts there.
const CHEST_HOTBAR: u16 = 54;
/// Window 0's first hotbar slot.
const HOTBAR_1: u16 = 36;

/// An App with azalea's inventory plugin, fleet-mc's script and menu
/// plugins, the session's real bridge with `Joined` read, and a bot that
/// records every packet it sends.
struct Scene {
    app: App,
    bot: Entity,
    _sink: EventSink,
    events: McEvents,
    /// Keeps the bot's world alive: azalea's `Worlds` holds it only weakly.
    _world: Box<dyn core::any::Any>,
    sent: Arc<Mutex<Vec<ServerboundGamePacket>>>,
}

impl Scene {
    fn new() -> Self {
        let (sink, events) = bridge(
            "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            NonZeroUsize::new(16).unwrap(),
            Arc::default(),
            Arc::new(LivenessStamps::new(Instant::now())),
        );
        sink.forward(&Event::Spawn);
        let mut app = App::new();
        app.add_message::<azalea::packet::game::DeathEvent>();
        app.add_plugins((
            azalea::inventory::InventoryPlugin,
            ScriptPlugin(sink.control()),
            MenuPlugin(sink.control()),
        ));
        let mut worlds = Worlds::new();
        let overworld = WorldName::new("minecraft:overworld");
        let world = worlds.get_or_insert(overworld.clone(), 384, -64, &RegistryHolder::default());
        app.insert_resource(worlds);
        let bot = app
            .world_mut()
            .spawn((
                LocalEntity,
                Inventory::default(),
                PlayerAbilities::default(),
                overworld,
            ))
            .id();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&sent);
        app.add_observer(move |packet: On<SendGamePacketEvent>| {
            seen.lock().unwrap().push(packet.event().packet.clone());
        });
        app.update();
        let mut scene = Self {
            app,
            bot,
            _sink: sink,
            events,
            _world: Box::new(world),
            sent,
        };
        assert_eq!(scene.events(), [SessionEvent::Joined]);
        scene
    }

    /// The server sends `packet`: azalea's own handler runs, then the
    /// frame that reads it, as in a session.
    fn receives(&mut self, packet: ClientboundGamePacket) {
        process_packet(self.app.world_mut(), self.bot, &packet);
        self.app.world_mut().write_message(ReceiveGamePacketEvent {
            entity: self.bot,
            packet: Arc::new(packet),
        });
        self.app.update();
    }

    /// Only fleet-mc's frame reads `packet`: for one azalea's handler needs
    /// more of a session than this App has.
    fn reads_only(&mut self, packet: ClientboundGamePacket) {
        self.app.world_mut().write_message(ReceiveGamePacketEvent {
            entity: self.bot,
            packet: Arc::new(packet),
        });
        self.app.update();
    }

    fn opens(&mut self, window: i32, kind: MenuKind, title: &str) {
        self.receives(ClientboundGamePacket::OpenScreen(ClientboundOpenScreen {
            container_id: window,
            menu_type: kind,
            title: FormattedText::from(title),
        }));
    }

    fn content(&mut self, window: i32, state: u32, items: Vec<ItemStack>, carried: ItemStack) {
        self.receives(ClientboundGamePacket::ContainerSetContent(
            ClientboundContainerSetContent {
                container_id: window,
                state_id: state,
                items,
                carried_item: carried,
            },
        ));
    }

    fn slot(&mut self, window: i32, state: u32, slot: u16, item: ItemStack) {
        self.receives(ClientboundGamePacket::ContainerSetSlot(
            ClientboundContainerSetSlot {
                container_id: window,
                state_id: state,
                slot,
                item_stack: item,
            },
        ));
    }

    fn cursor(&mut self, item: ItemStack) {
        self.receives(ClientboundGamePacket::SetCursorItem(
            ClientboundSetCursorItem { contents: item },
        ));
    }

    fn closes(&mut self) {
        self.receives(ClientboundGamePacket::ContainerClose(
            ClientboundContainerClose {
                container_id: CHEST,
            },
        ));
    }

    /// The barrier's reply.
    fn stats(&mut self) {
        self.receives(ClientboundGamePacket::AwardStats(ClientboundAwardStats {
            stats: HashMap::new(),
        }));
    }

    /// A chest of three rows in window 1 at state 1: 5 diamonds in slot 0,
    /// 3 emeralds in slot 1, 64 cobblestone in the first hotbar slot.
    fn chest(&mut self) -> Screen {
        self.opens(CHEST, MenuKind::Generic9x3, "Chest");
        let mut items = vec![ItemStack::Empty; 63];
        items[0] = ItemStack::new(ItemKind::Diamond, 5);
        items[1] = ItemStack::new(ItemKind::Emerald, 3);
        items[usize::from(CHEST_HOTBAR)] = ItemStack::new(ItemKind::Cobblestone, 64);
        self.content(CHEST, 1, items, ItemStack::Empty);
        self.screen()
    }

    fn view(&mut self) -> MenuView {
        super::view(self.app.world_mut(), self.bot)
    }

    fn screen(&mut self) -> Screen {
        self.view().screen.expect("a screen is open")
    }

    fn command(&mut self, command: MenuCommand) -> Result<(), MenuError> {
        perform(self.app.world_mut(), self.bot, command)
    }

    fn click(&mut self, screen: &Screen, kind: ClickKind, slot: u16) -> Result<(), MenuError> {
        let click = Click::on(screen, kind, slot).unwrap();
        self.command(MenuCommand::Act(MenuAction::Click(click)))
    }

    fn tick(&mut self) {
        self.app.world_mut().run_schedule(GameTick);
    }

    fn events(&mut self) -> Vec<SessionEvent> {
        self.events.queued()
    }

    /// What the bot sent since the last call.
    fn sent(&self) -> Vec<ServerboundGamePacket> {
        core::mem::take(&mut *self.sent.lock().unwrap())
    }

    fn inventory(&self) -> Inventory {
        self.app.world().get::<Inventory>(self.bot).unwrap().clone()
    }

    fn held(&self) -> Held {
        self.app.world().get::<Held>(self.bot).unwrap().clone()
    }

    /// Moves the menu clock on by `by`, as if that much time had passed.
    fn wait(&mut self, by: Duration) {
        let mut menus = self
            .app
            .world_mut()
            .get_mut::<super::Menus>(self.bot)
            .unwrap();
        menus.epoch = menus.epoch.checked_sub(by).unwrap();
    }
}

/// Whether no key and no button is down.
fn holds_nothing(held: &Held) -> bool {
    held.keys.is_empty() && held.taps.is_empty() && !held.left && held.right.is_none()
}

/// The click packet and the barrier behind it, or a failure that shows
/// what was sent instead.
fn click_and_barrier(sent: &[ServerboundGamePacket]) -> &ServerboundGamePacket {
    let [click, ServerboundGamePacket::ClientCommand(barrier)] = sent else {
        panic!("not one packet and its barrier: {sent:?}");
    };
    assert_eq!(barrier.action, ClientCommand::RequestStats);
    click
}

fn item_id(screen: &Screen, slot: u16) -> Option<String> {
    screen
        .slot(slot)
        .and_then(|slot| slot.item())
        .map(|item| item.id().as_str().to_owned())
}

fn count(screen: &Screen, slot: u16) -> Option<u32> {
    screen
        .slot(slot)
        .and_then(|slot| slot.item())
        .map(|item| item.count().get())
}

// --- The readers and the events (Plan.md P16.8) ---

#[test]
fn a_screen_the_server_opens_is_read_with_its_kind_title_slots_and_state() {
    let mut scene = Scene::new();

    let screen = scene.chest();

    assert_eq!(screen.kind(), ScreenKind::Generic9x3);
    assert_eq!(screen.title().as_str(), "Chest");
    assert_eq!(screen.state().get(), 1);
    assert_eq!(item_id(&screen, 0).as_deref(), Some("minecraft:diamond"));
    assert_eq!(count(&screen, 0), Some(5));
    assert_eq!(
        item_id(&screen, CHEST_HOTBAR).as_deref(),
        Some("minecraft:cobblestone")
    );
    assert_eq!(screen.cursor(), None);
    assert_eq!(
        scene.events(),
        [
            SessionEvent::MenuOpened(screen.id()),
            SessionEvent::MenuUpdated(screen.id())
        ]
    );
}

#[test]
fn each_window_keeps_the_state_the_server_last_gave_it() {
    let mut scene = Scene::new();
    // A slot of the bot's inventory before any screen: azalea takes its
    // state for every window from here on, as group A measured.
    scene.slot(0, 7, 20, ItemStack::new(ItemKind::Stick, 1));
    let chest = scene.chest();

    // The bot's own inventory changes while the chest is open: a hotbar
    // slot of window 0, at a later state of window 0's own.
    scene.slot(0, 9, HOTBAR_1, ItemStack::new(ItemKind::Dirt, 1));

    assert_eq!(scene.screen().state().get(), 1, "the chest's own state");
    assert_eq!(
        scene.inventory().state_id,
        7,
        "azalea's one state ID, the bot's inventory's"
    );
    assert_ne!(scene.screen(), chest, "the hotbar shows in the chest too");
    assert_eq!(
        item_id(&scene.screen(), CHEST_HOTBAR).as_deref(),
        Some("minecraft:dirt")
    );
}

#[test]
fn a_click_on_a_chest_carries_the_chests_state_after_the_bots_inventory_changed() {
    let mut scene = Scene::new();
    scene.chest();
    scene.slot(0, 9, HOTBAR_1, ItemStack::new(ItemKind::Dirt, 1));
    let chest = scene.screen();
    scene.sent();
    let azaleas = scene.inventory().state_id;

    scene.click(&chest, ClickKind::Left, 0).unwrap();

    let sent = scene.sent();
    let ServerboundGamePacket::ContainerClick(click) = click_and_barrier(&sent) else {
        panic!("{sent:?}");
    };
    assert_eq!(click.container_id, CHEST);
    assert_eq!(click.state_id, 1);
    assert_ne!(azaleas, 1, "azalea would have sent another");
}

#[test]
fn a_new_screen_with_the_title_and_window_of_the_one_before_is_a_new_screen() {
    let mut scene = Scene::new();
    let first = scene.chest();
    scene.events();

    let second = scene.chest();

    assert_ne!(first.id(), second.id());
    assert_eq!(first.id().window(), second.id().window());
    assert_eq!(
        scene.events(),
        [
            SessionEvent::MenuClosed(first.id()),
            SessionEvent::MenuOpened(second.id()),
            SessionEvent::MenuUpdated(second.id()),
        ]
    );
    assert_eq!(
        scene.click(&first, ClickKind::Left, 0),
        Err(MenuError::Refused(MenuRefusal::ScreenChanged))
    );
    assert_eq!(scene.sent(), []);
}

#[test]
fn the_cursor_comes_from_its_own_packet_and_from_a_whole_content() {
    let mut scene = Scene::new();
    scene.chest();

    scene.cursor(ItemStack::new(ItemKind::Diamond, 2));
    let after_cursor = scene.screen().cursor().map(|item| item.count().get());
    scene.content(
        CHEST,
        2,
        vec![ItemStack::Empty; 63],
        ItemStack::new(ItemKind::Emerald, 1),
    );
    let after_content = scene
        .screen()
        .cursor()
        .map(|item| item.id().as_str().to_owned());

    assert_eq!(after_cursor, Some(2));
    assert_eq!(after_content.as_deref(), Some("minecraft:emerald"));
    assert_eq!(
        scene.inventory().carried,
        ItemStack::new(ItemKind::Emerald, 1)
    );
}

#[rstest]
#[case::hotbar(0, HOTBAR_1, Some(CHEST_HOTBAR))]
#[case::main(9, 9, Some(CHEST_PLAYER))]
#[case::helmet(39, 5, None)]
#[case::boots(36, 8, None)]
#[case::offhand(40, 45, None)]
fn a_slot_of_the_bots_inventory_shows_in_window_0_and_in_the_open_screen(
    #[case] slot: u32,
    #[case] in_window_0: u16,
    #[case] in_chest: Option<u16>,
) {
    let mut scene = Scene::new();
    scene.chest();
    let gold = ItemStack::new(ItemKind::GoldIngot, 4);

    scene.receives(ClientboundGamePacket::SetPlayerInventory(
        ClientboundSetPlayerInventory {
            slot,
            contents: gold.clone(),
        },
    ));

    let inventory = scene.inventory();
    assert_eq!(
        inventory.inventory_menu.slot(usize::from(in_window_0)),
        Some(&gold)
    );
    if let Some(in_chest) = in_chest {
        assert_eq!(
            item_id(&scene.screen(), in_chest).as_deref(),
            Some("minecraft:gold_ingot")
        );
    }
    assert_eq!(
        scene
            .view()
            .inventory
            .slot(in_window_0)
            .and_then(|slot| slot.item())
            .map(|item| item.count().get()),
        Some(4)
    );
}

#[test]
fn an_items_name_lore_and_components_are_read_as_the_client_shows_them() {
    let mut scene = Scene::new();
    scene.opens(CHEST, MenuKind::Generic9x1, "Listings");
    let named = ItemStack::new(ItemKind::Emerald, 1)
        .with_component(components::CustomName {
            name: FormattedText::from("Create"),
        })
        .with_component(components::ItemName {
            name: FormattedText::from("Not shown"),
        })
        .with_component(components::Lore {
            lines: vec![
                FormattedText::from("Price: $1,000.00"),
                FormattedText::from("Seller: AfkBot2"),
            ],
        });
    let button = ItemStack::new(ItemKind::Arrow, 1).with_component(components::ItemName {
        name: FormattedText::from("Next page"),
    });
    let potion = ItemStack::new(ItemKind::Potion, 1).with_component(components::PotionContents {
        potion: Some(Potion::TurtleMaster),
        custom_color: None,
        custom_effects: Vec::new(),
        custom_name: None,
    });
    let sword =
        ItemStack::new(ItemKind::DiamondSword, 1).with_component(components::Damage { amount: 61 });
    let mut items = vec![ItemStack::Empty; 45];
    items[0] = named;
    items[1] = button;
    items[2] = potion;
    items[3] = sword;

    scene.content(CHEST, 1, items, ItemStack::Empty);
    let screen = scene.screen();
    let item = |slot: u16| screen.slot(slot).unwrap().item().unwrap().clone();

    assert_eq!(item(0).name().as_str(), "Create");
    let lore: Vec<_> = item(0)
        .lore()
        .lines()
        .iter()
        .map(|line| line.as_str().to_owned())
        .collect();
    assert_eq!(lore, ["Price: $1,000.00", "Seller: AfkBot2"]);
    assert_eq!(item(1).name().as_str(), "Next page");
    assert_eq!(
        item(2)
            .potion()
            .map(|potion| potion.as_str().to_owned())
            .as_deref(),
        Some("minecraft:turtle_master")
    );
    assert_eq!(item(3).durability(), Some(1561 - 61));
    assert!(!item(0).data().is_empty());
    assert_ne!(item(0).data(), item(1).data());
}

#[test]
fn an_items_data_is_the_same_whatever_order_its_components_came_in() {
    let name = components::CustomName {
        name: FormattedText::from("Create"),
    };
    let lore = components::Lore {
        lines: vec![FormattedText::from("Price: 5")],
    };
    let one = ItemStack::new(ItemKind::Emerald, 1)
        .with_component(name.clone())
        .with_component(lore.clone());
    let other = ItemStack::new(ItemKind::Emerald, 1)
        .with_component(lore)
        .with_component(name);
    let different = ItemStack::new(ItemKind::Emerald, 1).with_component(components::CustomName {
        name: FormattedText::from("Create "),
    });
    let data = |stack: &ItemStack| super::read::data(&stack.as_present().unwrap().component_patch);

    assert_eq!(data(&one), data(&other));
    assert_ne!(data(&one), data(&different));
    assert_eq!(
        data(&ItemStack::new(ItemKind::Emerald, 1)),
        Default::default()
    );
}

#[test]
fn an_enchantment_the_worlds_registry_doesnt_know_reads_as_unknown() {
    let levels: HashMap<_, _> = [(azalea::registry::data::Enchantment::new_raw(3), 5)].into();
    let sword = ItemStack::new(ItemKind::DiamondSword, 1)
        .with_component(components::Enchantments { levels });

    let read = super::read::item(&sword, None).unwrap();

    assert_eq!(
        read.enchantments()
            .iter()
            .map(|(id, level)| (id.as_str().to_owned(), level))
            .collect::<Vec<_>>(),
        [("minecraft:unknown".to_owned(), 5)]
    );
}

#[test]
fn a_merchants_trades_are_read_with_the_cost_it_asks_now() {
    let mut scene = Scene::new();
    scene.opens(2, MenuKind::Merchant, "Farmer");
    let offer = MerchantOffer {
        base_cost_a: ItemCost {
            item: ItemKind::Emerald,
            count: 10,
            components: DataComponentExactPredicate {
                expected: Vec::new(),
            },
        },
        result: ItemStack::new(ItemKind::Bread, 6),
        cost_b: None,
        out_of_stock: false,
        uses: 0,
        max_uses: 16,
        xp: 2,
        special_price_diff: -1,
        price_multiplier: 0.05,
        demand: 4,
    };

    scene.receives(ClientboundGamePacket::MerchantOffers(
        ClientboundMerchantOffers {
            container_id: 2,
            offers: vec![offer],
            villager_level: 1,
            villager_xp: 0,
            show_progress: true,
            can_restock: true,
        },
    ));
    let screen = scene.screen();

    let [trade] = screen.offers() else {
        panic!("{:?}", screen.offers());
    };
    // 10, raised by floor(10 * 4 * 0.05) = 2, lowered by 1.
    assert_eq!(trade.cost().count().get(), 11);
    assert_eq!(trade.result().id().as_str(), "minecraft:bread");
    assert!(trade.second_cost().is_none());
}

#[test]
fn a_close_ends_the_screen_and_azaleas_copy() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.events();

    scene.closes();

    assert_eq!(scene.view().screen, None);
    assert!(scene.inventory().container_menu.is_none());
    assert_eq!(scene.events(), [SessionEvent::MenuClosed(chest.id())]);
}

// --- Clicks (Plan.md P16.9) ---

#[test]
fn a_click_goes_out_with_azaleas_prediction_and_its_barrier_right_behind() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.sent();

    scene.click(&chest, ClickKind::Left, 0).unwrap();

    let sent = scene.sent();
    let ServerboundGamePacket::ContainerClick(click) = click_and_barrier(&sent) else {
        panic!("{sent:?}");
    };
    assert_eq!(click.slot_num, 0);
    assert_eq!(click.changed_slots.keys().copied().collect::<Vec<_>>(), [0]);
    assert!(click.carried_item.0.is_some(), "the diamonds on the cursor");
    // azalea's copy shows the prediction, as a vanilla client's screen does.
    assert_eq!(
        scene.screen().cursor().map(|item| item.count().get()),
        Some(5)
    );
    assert_eq!(item_id(&scene.screen(), 0), None);
}

#[rstest]
#[case::a_slot(|scene: &mut Scene| scene.slot(CHEST, 2, 0, ItemStack::new(ItemKind::Dirt, 1)), MenuRefusal::ContentChanged)]
#[case::the_cursor(|scene: &mut Scene| scene.cursor(ItemStack::new(ItemKind::Dirt, 1)), MenuRefusal::ContentChanged)]
#[case::a_whole_content(|scene: &mut Scene| scene.content(CHEST, 2, vec![ItemStack::Empty; 63], ItemStack::Empty), MenuRefusal::ContentChanged)]
#[case::a_new_screen(|scene: &mut Scene| { scene.chest(); }, MenuRefusal::ScreenChanged)]
#[case::a_close(Scene::closes, MenuRefusal::NoMenu)]
#[case::the_bots_slot_without_a_state(
    |scene: &mut Scene| scene.receives(ClientboundGamePacket::SetPlayerInventory(
        ClientboundSetPlayerInventory { slot: 9, contents: ItemStack::new(ItemKind::Dirt, 1) },
    )),
    MenuRefusal::SlotChanged
)]
fn a_click_onto_a_screen_that_changed_since_its_hook_saw_it_is_never_sent(
    #[case] change: fn(&mut Scene),
    #[case] refusal: MenuRefusal,
) {
    let mut scene = Scene::new();
    let seen = scene.chest();
    change(&mut scene);
    scene.sent();

    let clicked = scene.click(&seen, ClickKind::Left, CHEST_PLAYER);

    assert_eq!(clicked, Err(MenuError::Refused(refusal)));
    assert_eq!(scene.sent(), []);
}

/// What came between a click and its barrier's reply, and the fate that
/// makes it.
#[rstest]
#[case::nothing(|_: &mut Scene| {}, ClickFate::Ok)]
#[case::a_predicted_slot(|scene: &mut Scene| scene.slot(CHEST, 2, 0, ItemStack::new(ItemKind::Diamond, 5)), ClickFate::Rejected)]
#[case::the_cursor(|scene: &mut Scene| scene.cursor(ItemStack::Empty), ClickFate::Rejected)]
#[case::a_resync(|scene: &mut Scene| scene.content(CHEST, 2, vec![ItemStack::Empty; 63], ItemStack::Empty), ClickFate::Rejected)]
#[case::another_slot(|scene: &mut Scene| scene.slot(CHEST, 2, 5, ItemStack::new(ItemKind::Dirt, 1)), ClickFate::Ok)]
#[case::a_close(Scene::closes, ClickFate::Replaced)]
#[case::a_new_screen(|scene: &mut Scene| scene.opens(2, MenuKind::Generic9x1, "Confirm"), ClickFate::Replaced)]
fn a_clicks_fate_is_what_came_before_its_barriers_reply(
    #[case] between: fn(&mut Scene),
    #[case] fate: ClickFate,
) {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.click(&chest, ClickKind::Left, 0).unwrap();
    between(&mut scene);
    scene.events();

    scene.stats();

    let clicked: Vec<_> = scene
        .events()
        .into_iter()
        .filter(|event| matches!(event, SessionEvent::Clicked(_)))
        .collect();
    assert_eq!(
        clicked,
        [SessionEvent::Clicked(ClickResult {
            screen: chest.id(),
            slot: Some(0),
            fate
        })]
    );
}

#[test]
fn a_click_without_a_reply_in_five_seconds_is_unanswered_and_its_screen_takes_no_more() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.click(&chest, ClickKind::Left, 0).unwrap();
    scene.events();

    scene.wait(Duration::from_millis(4_900));
    scene.tick();
    let early = scene.events();
    scene.wait(Duration::from_millis(100));
    scene.tick();
    let unanswered = scene.events();
    let next = scene.screen();
    let again = scene.click(&next, ClickKind::Left, 1);
    scene.stats();
    let late = scene.events();

    assert_eq!(early, []);
    assert_eq!(
        unanswered,
        [SessionEvent::Clicked(ClickResult {
            screen: chest.id(),
            slot: Some(0),
            fate: ClickFate::Unanswered
        })]
    );
    assert_eq!(again, Err(MenuError::Refused(MenuRefusal::Unanswered)));
    assert_eq!(late, [], "a late reply answers nothing");
}

#[test]
fn no_action_goes_out_while_one_is_pending_or_its_fate_is_unread() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.click(&chest, ClickKind::Left, 0).unwrap();
    let after_first = scene.screen();
    scene.sent();

    let pending = scene.click(&after_first, ClickKind::Left, 1);
    scene.stats();
    let unread = scene.click(&after_first, ClickKind::Left, 1);
    let fates = scene.events();
    let read = scene.click(&after_first, ClickKind::Left, 1);

    assert_eq!(pending, Err(MenuError::Refused(MenuRefusal::ClickPending)));
    assert_eq!(unread, Err(MenuError::Refused(MenuRefusal::ClickPending)));
    assert!(
        fates
            .iter()
            .any(|event| matches!(event, SessionEvent::Clicked(_)))
    );
    assert_eq!(read, Ok(()));
    assert_eq!(scene.sent().len(), 2, "only the last, with its barrier");
}

/// azalea's prediction, as fleet-mc sends it, for each kind of click in a
/// chest: what the slot and the cursor hold after it.
#[rstest]
#[case::left_picks_the_stack_up(ClickKind::Left, 0, None, Some(5))]
#[case::right_picks_half_up(ClickKind::Right, 0, Some(2), Some(3))]
#[case::shift_moves_it_to_the_bot(ClickKind::Shift, 0, None, None)]
#[case::a_number_key_swaps_with_that_hotbar_slot(ClickKind::NumberKey(HotbarSlot::try_from(0).unwrap()), 0, Some(64), None)]
#[case::drop_throws_one(ClickKind::Drop, 0, Some(4), None)]
#[case::drop_stack_throws_them_all(ClickKind::DropStack, 0, None, None)]
fn each_kind_of_click_is_predicted_as_a_vanilla_client_predicts_it(
    #[case] kind: ClickKind,
    #[case] slot: u16,
    #[case] left_in_slot: Option<u32>,
    #[case] on_cursor: Option<u32>,
) {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.sent();

    scene.click(&chest, kind, slot).unwrap();

    let after = scene.screen();
    assert_eq!(count(&after, slot), left_in_slot);
    assert_eq!(after.cursor().map(|item| item.count().get()), on_cursor);
    let sent = scene.sent();
    let ServerboundGamePacket::ContainerClick(click) = click_and_barrier(&sent) else {
        panic!("{sent:?}");
    };
    assert!(click.changed_slots.contains_key(&slot), "{click:?}");
}

#[test]
fn a_number_key_swaps_the_slot_with_the_hotbar_slot_it_names() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.sent();

    scene
        .click(
            &chest,
            ClickKind::NumberKey(HotbarSlot::try_from(0).unwrap()),
            0,
        )
        .unwrap();

    let after = scene.screen();
    assert_eq!(item_id(&after, 0).as_deref(), Some("minecraft:cobblestone"));
    assert_eq!(
        item_id(&after, CHEST_HOTBAR).as_deref(),
        Some("minecraft:diamond")
    );
    let sent = scene.sent();
    let ServerboundGamePacket::ContainerClick(click) = click_and_barrier(&sent) else {
        panic!("{sent:?}");
    };
    assert_eq!(click.button_num, 0, "the key, not a slot");
    let mut changed: Vec<_> = click.changed_slots.keys().copied().collect();
    changed.sort_unstable();
    assert_eq!(changed, [0, CHEST_HOTBAR]);
}

#[test]
fn a_click_in_the_bots_own_inventory_goes_to_window_0_with_its_state() {
    let mut scene = Scene::new();
    let mut items = vec![ItemStack::Empty; 46];
    items[usize::from(HOTBAR_1)] = ItemStack::new(ItemKind::Cobblestone, 64);
    scene.content(0, 4, items, ItemStack::Empty);

    scene.command(MenuCommand::OpenInventory).unwrap();
    let inventory = scene.screen();
    scene.sent();
    scene.click(&inventory, ClickKind::Left, HOTBAR_1).unwrap();

    assert_eq!(inventory.kind(), ScreenKind::Inventory);
    let sent = scene.sent();
    let ServerboundGamePacket::ContainerClick(click) = click_and_barrier(&sent) else {
        panic!("{sent:?}");
    };
    assert_eq!((click.container_id, click.state_id), (0, 4));
}

#[test]
fn the_inventory_opens_only_with_no_screen_open() {
    let mut scene = Scene::new();
    scene.chest();

    assert_eq!(
        scene.command(MenuCommand::OpenInventory),
        Err(MenuError::Refused(MenuRefusal::ScreenOpen))
    );
}

#[test]
fn a_close_goes_out_and_an_action_pending_on_the_screen_is_replaced() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.click(&chest, ClickKind::Left, 0).unwrap();
    scene.sent();
    scene.events();

    scene.command(MenuCommand::Close(chest.id())).unwrap();
    let sent = scene.sent();
    scene.stats();

    assert!(
        matches!(sent.as_slice(), [ServerboundGamePacket::ContainerClose(close)] if close.container_id == CHEST),
        "{sent:?}"
    );
    assert_eq!(
        scene.events(),
        [
            SessionEvent::MenuClosed(chest.id()),
            SessionEvent::Clicked(ClickResult {
                screen: chest.id(),
                slot: Some(0),
                fate: ClickFate::Replaced
            })
        ]
    );
    assert_eq!(
        scene.command(MenuCommand::Close(chest.id())),
        Err(MenuError::Refused(MenuRefusal::NoMenu))
    );
}

#[test]
fn every_key_and_button_is_let_go_of_when_a_screen_opens_and_none_goes_down_while_it_is_open() {
    let mut scene = Scene::new();
    let forward = GameAction::Press(Input::Key(Key::Forward));
    let left = GameAction::Press(Input::Mouse(MouseButton::Left));
    for action in [forward, left] {
        assert_eq!(
            crate::script::act(scene.app.world_mut(), scene.bot, action),
            Ok(())
        );
    }
    let before = scene.held();

    let chest = scene.chest();
    let opened = scene.held();
    for action in [forward, left, GameAction::Tap(Input::Key(Key::Jump))] {
        assert_eq!(
            crate::script::act(scene.app.world_mut(), scene.bot, action),
            Ok(())
        );
    }
    let while_open = scene.held();
    scene.command(MenuCommand::Close(chest.id())).unwrap();
    assert_eq!(
        crate::script::act(scene.app.world_mut(), scene.bot, forward),
        Ok(())
    );

    assert!(!holds_nothing(&before));
    assert!(holds_nothing(&opened), "{opened:?}");
    assert!(holds_nothing(&while_open), "{while_open:?}");
    assert!(!holds_nothing(&scene.held()), "down again once it's closed");
    assert!(
        !scene
            .sent()
            .iter()
            .any(|packet| matches!(packet, ServerboundGamePacket::PlayerAction(_))),
        "nothing was in use, so no release went out"
    );
}

#[rstest]
#[case::a_button(MenuKind::Enchantment, |screen: &Screen| MenuAction::Button(ButtonPress::on(screen, 1).unwrap()))]
#[case::a_name(MenuKind::Anvil, |screen: &Screen| MenuAction::Rename(Rename::on(screen, (&"Sword".parse::<fleet_core::chat::ChatMessage>().unwrap()).try_into().unwrap()).unwrap()))]
fn a_screens_own_actions_go_out_with_their_barrier(
    #[case] kind: MenuKind,
    #[case] action: fn(&Screen) -> MenuAction,
) {
    let mut scene = Scene::new();
    scene.opens(3, kind, "Table");
    let screen = scene.screen();
    scene.sent();

    scene.command(MenuCommand::Act(action(&screen))).unwrap();

    let sent = scene.sent();
    let packet = click_and_barrier(&sent);
    match packet {
        ServerboundGamePacket::ContainerButtonClick(button) => {
            assert_eq!((button.container_id, button.button_id), (3, 1));
        }
        ServerboundGamePacket::RenameItem(rename) => assert_eq!(rename.name, "Sword"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_trade_goes_out_by_its_place_with_its_barrier() {
    let mut scene = Scene::new();
    scene.opens(2, MenuKind::Merchant, "Farmer");
    let offer = MerchantOffer {
        base_cost_a: ItemCost {
            item: ItemKind::Emerald,
            count: 1,
            components: DataComponentExactPredicate {
                expected: Vec::new(),
            },
        },
        result: ItemStack::new(ItemKind::Bread, 6),
        cost_b: None,
        out_of_stock: false,
        uses: 0,
        max_uses: 16,
        xp: 2,
        special_price_diff: 0,
        price_multiplier: 0.05,
        demand: 0,
    };
    scene.receives(ClientboundGamePacket::MerchantOffers(
        ClientboundMerchantOffers {
            container_id: 2,
            offers: vec![offer.clone(), offer],
            villager_level: 1,
            villager_xp: 0,
            show_progress: true,
            can_restock: true,
        },
    ));
    let screen = scene.screen();
    scene.sent();

    scene
        .command(MenuCommand::Act(MenuAction::Trade(
            TradeChoice::on(&screen, 1).unwrap(),
        )))
        .unwrap();

    let sent = scene.sent();
    let ServerboundGamePacket::SelectTrade(trade) = click_and_barrier(&sent) else {
        panic!("{sent:?}");
    };
    assert_eq!(trade.item, 1);
}

// --- Sign editors (Plan.md P16.10; ADR-0036, M16) ---

fn sign_editor(scene: &mut Scene, front: bool) {
    scene.receives(ClientboundGamePacket::OpenSignEditor(
        ClientboundOpenSignEditor {
            pos: BlockPos::new(3, 64, -2),
            is_front_text: front,
        },
    ));
}

/// The user's server's price sign: the price alone on its first line.
fn price() -> fleet_core::script::menu::SignText {
    let template =
        fleet_core::script::menu::SignTemplate::try_from(fleet_core::script::menu::SignDraft {
            key: "price".to_owned(),
            lines: vec!["{price}".to_owned()],
            values: vec![fleet_core::script::ValueDraft {
                name: "price".to_owned(),
                kind: fleet_core::script::ValueKind::Integer {
                    min: 1,
                    max: 1_000_000,
                },
            }],
        })
        .unwrap();
    template
        .render(&[("price", fleet_core::script::Given::Integer(1_500))])
        .unwrap()
}

#[test]
fn a_sign_editor_is_reported_and_answered_once_through_the_desk() {
    let mut scene = Scene::new();
    sign_editor(&mut scene, false);
    let editor = scene.view().sign.expect("an editor waits");
    let events = scene.events();
    scene.sent();

    let answered = scene.command(MenuCommand::AnswerSign {
        editor,
        answer: SignAnswer::Write(price()),
    });
    let again = scene.command(MenuCommand::AnswerSign {
        editor,
        answer: SignAnswer::Cancel,
    });

    assert_eq!(events, [SessionEvent::SignOpened(editor)]);
    assert_eq!(editor.side(), SignSide::Back);
    assert_eq!(answered, Ok(()));
    assert_eq!(again, Err(MenuError::Sign(SignRefusal::Answered)));
    let sent = scene.sent();
    let [ServerboundGamePacket::SignUpdate(update)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(update.pos, BlockPos::new(3, 64, -2));
    assert!(!update.is_front_text);
    assert_eq!(update.lines, ["1500", "", "", ""]);
    assert_eq!(scene.view().sign, None);
}

#[test]
fn a_cancelled_sign_gets_four_empty_lines() {
    let mut scene = Scene::new();
    sign_editor(&mut scene, true);
    let editor = scene.view().sign.unwrap();
    scene.sent();

    scene
        .command(MenuCommand::AnswerSign {
            editor,
            answer: SignAnswer::Cancel,
        })
        .unwrap();

    let sent = scene.sent();
    let [ServerboundGamePacket::SignUpdate(update)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(update.lines, ["", "", "", ""]);
}

#[test]
fn a_sign_editor_takes_the_open_screens_place() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    scene.events();
    scene.sent();

    sign_editor(&mut scene, true);

    let view = scene.view();
    assert_eq!(view.screen, None);
    assert!(scene.inventory().container_menu.is_none());
    assert_eq!(
        scene.events(),
        [
            SessionEvent::MenuClosed(chest.id()),
            SessionEvent::SignOpened(view.sign.unwrap())
        ]
    );
    assert_eq!(scene.sent(), [], "nothing goes to the server");
}

/// An editor the script doesn't answer is never answered, whatever ends it
/// (ADR-0036, M16).
#[rstest]
#[case::a_screen_opening(|scene: &mut Scene| scene.opens(2, MenuKind::Generic9x3, "Chest"))]
#[case::a_close(Scene::closes)]
#[case::time(|scene: &mut Scene| for _ in 0..200 { scene.tick(); })]
#[case::a_world_change(|scene: &mut Scene| scene.reads_only(crate::testing::respawn()))]
fn an_editor_the_script_doesnt_answer_is_never_answered(#[case] then: fn(&mut Scene)) {
    let mut scene = Scene::new();
    sign_editor(&mut scene, true);
    let editor = scene.view().sign.unwrap();
    scene.sent();

    then(&mut scene);

    assert!(
        !scene
            .sent()
            .iter()
            .any(|packet| matches!(packet, ServerboundGamePacket::SignUpdate(_))),
        "an answer went out by itself"
    );
    if scene.view().sign.is_none() {
        assert_eq!(
            scene.command(MenuCommand::AnswerSign {
                editor,
                answer: SignAnswer::Cancel
            }),
            Err(MenuError::Sign(SignRefusal::NoSign))
        );
    }
}

#[test]
fn a_death_ends_the_screen_and_the_editor() {
    let mut scene = Scene::new();
    let chest = scene.chest();
    sign_editor(&mut scene, true);
    scene.chest();
    scene.events();

    scene
        .app
        .world_mut()
        .write_message(azalea::packet::game::DeathEvent {
            entity: scene.bot,
            packet: None,
        });
    scene.app.update();

    assert_eq!(scene.view().screen, None);
    assert_eq!(scene.view().sign, None);
    assert!(
        scene
            .events()
            .iter()
            .any(|event| matches!(event, SessionEvent::MenuClosed(id) if *id != chest.id()))
    );
}
