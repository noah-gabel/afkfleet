//! A bot's screens, clicks and sign editors on real servers, through
//! fleet-mc's own session calls (Plan.md P16.8 to P16.11; ADR-0036, M2, M3,
//! M7, M8, M16, M17, R1; ADR-0051).
//!
//! Every step goes through what the runtime will call: `observe` for what a
//! hook sees, `menu` for what it does, `perform` for its keys, `send_chat`
//! for a command. A hook on the bot's App records the packets the bot sends
//! and the state the server last gave each window, to check the clicks
//! against the wire; it never acts.
//!
//! **Two fates need a server that doesn't answer in time:** `replaced` (the
//! bot closes the screen before the barrier's reply) and `unanswered` (no
//! reply in 5 s). The server's container is frozen for them, right after a
//! keep-alive came, so its own 15 s keep-alive check never sees the pause.

use core::time::Duration;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use azalea::app::{App, Update};
use azalea::ecs::message::MessageReader;
use azalea::ecs::observer::On;
use azalea::packet::game::{ReceiveGamePacketEvent, SendGamePacketEvent};
use azalea::protocol::packets::game::{ClientboundGamePacket, ServerboundGamePacket};
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_core::mc::{
    MenuCommand, MenuError, MenuView, SessionEvent, SessionEvents as _, SessionHandle as _,
};
use fleet_core::mode::GameAction;
use fleet_core::net::TargetPolicy;
use fleet_core::script::menu::{
    AnvilName, ButtonPress, Click, ClickFate, ClickKind, MenuAction, MenuRefusal, Rename, Screen,
    ScreenKind, SignAnswer, SignDraft, SignTemplate, Slot, TradeChoice,
};
use fleet_core::script::{Given, Input, Key, Position, ValueDraft, ValueKind};
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession};

use crate::checks::ticks_pass;
use crate::harness::{Mode, Server, WITHIN, bot, connect, offline, wait_for, within};

const NAME: &str = "AfkBot1";
/// Where the bot's feet are: on the flat world's grass.
const FEET: i32 = -60;

/// What the hook saw on the wire.
#[derive(Debug, Default)]
struct Wire {
    sent: Vec<ServerboundGamePacket>,
    /// The state the server last gave each window, from its content and
    /// slot packets.
    states: HashMap<i32, u32>,
    keep_alives: u64,
    stats_replies: u64,
}

fn lock(wire: &Mutex<Wire>) -> MutexGuard<'_, Wire> {
    wire.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An App hook that records `tested`'s wire in `wire`.
fn record(
    tested: BotId,
    wire: &Arc<Mutex<Wire>>,
) -> impl Fn(BotId, &mut App) + Send + Sync + 'static {
    let wire = Arc::clone(wire);
    move |bot_id, app| {
        if bot_id != tested {
            return;
        }
        let sending = Arc::clone(&wire);
        app.add_observer(move |sent: On<SendGamePacketEvent>| {
            lock(&sending).sent.push(sent.event().packet.clone());
        });
        let hearing = Arc::clone(&wire);
        app.add_systems(
            Update,
            move |mut packets: MessageReader<ReceiveGamePacketEvent>| {
                let mut wire = lock(&hearing);
                for received in packets.read() {
                    match &*received.packet {
                        ClientboundGamePacket::ContainerSetContent(content) => {
                            wire.states.insert(content.container_id, content.state_id);
                        }
                        ClientboundGamePacket::ContainerSetSlot(slot) if slot.container_id >= 0 => {
                            wire.states.insert(slot.container_id, slot.state_id);
                        }
                        ClientboundGamePacket::KeepAlive(_) => wire.keep_alives += 1,
                        ClientboundGamePacket::AwardStats(_) => wire.stats_replies += 1,
                        _ => {}
                    }
                }
            },
        );
    }
}

/// The bot on a server, and its wire.
struct Bench {
    server: Server,
    session: McSession,
    events: McEvents,
    wire: Arc<Mutex<Wire>>,
    name: &'static str,
}

impl Bench {
    async fn new(server: Server, name: &'static str) -> Self {
        let wire = Arc::new(Mutex::new(Wire::default()));
        let connector = AzaleaConnector::new(&McConfig::default())
            .with_targets(TargetPolicy::Unchecked)
            .with_app_hook(record(bot(1), &wire));
        let (session, mut events) = connect(&connector, &server, bot(1), offline(NAME)).await;
        wait_for(&mut events, "the join", |event| {
            *event == SessionEvent::Joined
        })
        .await;
        let bench = Self {
            server,
            session,
            events,
            wire,
            name,
        };
        bench.rcon(&format!("clear {NAME}")).await;
        bench.put(0.5, 0.5, -90.0).await;
        bench
    }

    async fn rcon(&self, command: &str) -> String {
        self.server.rcon(command).await
    }

    /// Teleports the bot and waits until a snapshot has it there.
    async fn put(&self, x: f64, z: f64, yaw: f32) {
        self.rcon(&format!("tp {NAME} {x} {FEET} {z} {yaw} 0"))
            .await;
        self.until("the bot in place", |seen| {
            let at = seen.facts.bot().position;
            (at.x() - x).abs() < 0.01 && (at.z() - z).abs() < 0.01
        })
        .await;
    }

    async fn observe(&self) -> fleet_core::mc::Observation {
        within("a snapshot", self.session.observe())
            .await
            .expect("the bot is placed")
    }

    /// Waits until a snapshot satisfies `done`, a game tick at a time.
    async fn until(
        &self,
        what: &str,
        done: impl Fn(&fleet_core::mc::Observation) -> bool,
    ) -> fleet_core::mc::Observation {
        let deadline = Instant::now() + WITHIN;
        loop {
            if let Ok(seen) = within("a snapshot", self.session.observe()).await
                && done(&seen)
            {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "{}: {what} didn't happen; the bot sees {:#?}",
                self.name,
                self.session.observe().await.map(|seen| seen.menu)
            );
            ticks_pass(&self.session, 1).await;
        }
    }

    async fn until_menu(&self, what: &str, done: impl Fn(&MenuView) -> bool) -> MenuView {
        self.until(what, |seen| done(&seen.menu)).await.menu
    }

    /// Waits for a screen titled `title` whose content has come.
    async fn screen(&self, title: &str) -> Screen {
        self.until_menu(&format!("the screen {title:?}"), |menu| {
            menu.screen
                .as_ref()
                .is_some_and(|screen| screen.title().as_str() == title && screen.state().get() > 0)
        })
        .await
        .screen
        .unwrap()
    }

    async fn menu(&self, command: MenuCommand) -> Result<(), MenuError> {
        within("a menu command", self.session.menu(command)).await
    }

    async fn perform(&self, action: GameAction) {
        assert_eq!(
            within("an action", self.session.perform(action)).await,
            Ok(()),
            "{action:?}"
        );
    }

    async fn chat(&self, text: &str) {
        let message = ChatMessage::try_from(text).unwrap();
        assert_eq!(
            within("chat", self.session.send_chat(message)).await,
            Ok(())
        );
    }

    /// Clicks and waits for the click's fate.
    async fn click(&mut self, screen: &Screen, kind: ClickKind, slot: u16) -> ClickFate {
        let click = Click::on(screen, kind, slot).unwrap();
        self.act(MenuAction::Click(click)).await
    }

    /// Sends `action` and waits for its fate.
    async fn act(&mut self, action: MenuAction) -> ClickFate {
        let screen = action.screen();
        let slot = action.slot();
        assert_eq!(
            self.menu(MenuCommand::Act(action)).await,
            Ok(()),
            "{}: the action went out",
            self.name
        );
        self.fate(screen, slot).await
    }

    /// Waits for the fate of the action on `screen` and `slot`.
    async fn fate(
        &mut self,
        screen: fleet_core::script::menu::ScreenId,
        slot: Option<u16>,
    ) -> ClickFate {
        let SessionEvent::Clicked(result) = wait_for(&mut self.events, "a click's fate", |event| {
            matches!(event, SessionEvent::Clicked(_))
        })
        .await
        else {
            unreachable!("wait_for found a fate");
        };
        assert_eq!((result.screen, result.slot), (screen, slot), "{result:?}");
        eprintln!("FATE {} slot {slot:?}: {}", self.name, result.fate);
        result.fate
    }

    /// Right-clicks the block at `at`, or the entity standing there, as a
    /// player does with an empty hand.
    async fn use_at(&self, x: f64, y: f64, z: f64) {
        let at = Position::try_new(x, y, z).unwrap();
        self.perform(GameAction::LookAtPosition(at)).await;
        ticks_pass(&self.session, 3).await;
        self.perform(GameAction::Tap(Input::Mouse(
            fleet_core::script::MouseButton::Right,
        )))
        .await;
    }

    fn clicks_sent(&self) -> usize {
        lock(&self.wire)
            .sent
            .iter()
            .filter(|packet| matches!(packet, ServerboundGamePacket::ContainerClick(_)))
            .count()
    }

    fn signs_sent(&self) -> usize {
        lock(&self.wire)
            .sent
            .iter()
            .filter(|packet| matches!(packet, ServerboundGamePacket::SignUpdate(_)))
            .count()
    }

    /// The last click the bot sent: its window and its state.
    fn last_click(&self) -> (i32, u32) {
        lock(&self.wire)
            .sent
            .iter()
            .rev()
            .find_map(|packet| match packet {
                ServerboundGamePacket::ContainerClick(click) => {
                    Some((click.container_id, click.state_id))
                }
                _ => None,
            })
            .expect("a click went out")
    }

    /// The keys the bot last told the server it holds.
    fn keys_on_the_wire(&self) -> Option<[bool; 7]> {
        lock(&self.wire)
            .sent
            .iter()
            .rev()
            .find_map(|packet| match packet {
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

    /// Freezes the server's container right after a keep-alive came, so
    /// the server's 15 s keep-alive check never sees the pause.
    async fn freeze(&self) {
        let before = lock(&self.wire).keep_alives;
        within("a keep-alive", async {
            while lock(&self.wire).keep_alives == before {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        self.server.frozen(true);
    }

    fn thaw(&self) {
        self.server.frozen(false);
    }

    /// Closes the open screen.
    async fn close(&self) {
        let open = self.observe().await.menu.screen.expect("a screen is open");
        assert_eq!(self.menu(MenuCommand::Close(open.id())).await, Ok(()));
    }
}

/// The slot of `screen` that holds an item of kind `id`, the first.
fn holding(screen: &Screen, id: &str, among: impl Fn(&Slot) -> bool) -> u16 {
    screen
        .own()
        .chain(screen.player())
        .filter(|slot| among(slot))
        .find(|slot| slot.item().is_some_and(|item| item.id().as_str() == id))
        .unwrap_or_else(|| panic!("no {id} in {screen:#?}"))
        .index()
}

fn id_at(screen: &Screen, slot: u16) -> Option<String> {
    screen
        .slot(slot)
        .and_then(Slot::item)
        .map(|item| item.id().as_str().to_owned())
}

// --- The vanilla test server: a chest, the four fates, a villager, an anvil
// and an enchanting table (P16.11) ---

#[tokio::test(flavor = "multi_thread")]
async fn slow_screens_on_vanilla() {
    let mut bench = Bench::new(Server::start(Mode::Offline).await, "vanilla").await;

    a_chest_and_the_keys_while_it_is_open(&mut bench).await;
    a_screen_that_changed_since_the_hook_takes_no_click(&mut bench).await;
    a_rejection_replaced_and_unanswered_on_vanilla(&mut bench).await;
    a_villagers_trade(&mut bench).await;
    an_anvils_name(&mut bench).await;
    an_enchanting_tables_button(&mut bench).await;

    within("disconnect", bench.session.disconnect()).await;
}

/// The chest east of the bot.
const CHEST: (i32, i32, i32) = (2, FEET, 0);

async fn open_chest(bench: &Bench) -> Screen {
    bench
        .use_at(f64::from(CHEST.0) + 0.5, f64::from(FEET) + 0.5, 0.5)
        .await;
    bench.screen("Chest").await
}

async fn a_chest_and_the_keys_while_it_is_open(bench: &mut Bench) {
    // Window 0's state goes well past where a chest's starts: five slots
    // of the bot's inventory, each a slot packet of its own. The hotbar
    // stays empty, so the hand is.
    for slot in 0..5 {
        bench
            .rcon(&format!(
                "item replace entity {NAME} inventory.{slot} with minecraft:cobblestone 16"
            ))
            .await;
    }
    bench
        .rcon(&format!(
            "setblock {} {} {} minecraft:chest{{Items:[{{Slot:0b,id:\"minecraft:diamond\",count:5}},{{Slot:1b,id:\"minecraft:emerald\",count:3}}]}}",
            CHEST.0, CHEST.1, CHEST.2
        ))
        .await;
    bench
        .until_menu("the cobblestone in the inventory", |menu| {
            (9..14).all(|slot| menu.inventory.slot(slot).and_then(Slot::item).is_some())
        })
        .await;

    // Keys down that don't move the bot: sneak and sprint.
    bench
        .perform(GameAction::Press(Input::Key(Key::Sneak)))
        .await;
    bench
        .perform(GameAction::Press(Input::Key(Key::Sprint)))
        .await;
    ticks_pass(&bench.session, 2).await;
    let held = bench.keys_on_the_wire();

    let chest = open_chest(bench).await;
    ticks_pass(&bench.session, 2).await;
    let opened = bench.keys_on_the_wire();
    let from = bench.observe().await.facts.bot().position;
    bench
        .perform(GameAction::Press(Input::Key(Key::Forward)))
        .await;
    bench
        .perform(GameAction::Press(Input::Key(Key::Jump)))
        .await;
    ticks_pass(&bench.session, 10).await;
    let while_open = bench.keys_on_the_wire();
    let still = bench.observe().await.facts.bot().position;

    assert_eq!(held.map(|keys| keys[5]), Some(true), "sneaking: {held:?}");
    assert_eq!(
        opened,
        Some([false; 7]),
        "every key up once the chest opened"
    );
    assert_eq!(while_open, Some([false; 7]), "none down while it's open");
    assert!((from.x() - still.x()).abs() < 0.001 && (from.z() - still.z()).abs() < 0.001);
    assert_eq!(chest.kind(), ScreenKind::Generic9x3);
    assert_eq!(id_at(&chest, 0).as_deref(), Some("minecraft:diamond"));

    // The bot's own inventory changes while the chest is open: the chest's
    // copy of its slots, with the chest's next state.
    bench
        .rcon(&format!(
            "item replace entity {NAME} inventory.5 with minecraft:dirt"
        ))
        .await;
    let changed = bench
        .until_menu("the dirt in the chest's copy", |menu| {
            menu.screen
                .as_ref()
                .is_some_and(|screen| id_at(screen, 27 + 5).as_deref() == Some("minecraft:dirt"))
        })
        .await
        .screen
        .unwrap();
    let fate = bench.click(&changed, ClickKind::Left, 0).await;
    let (window, state) = bench.last_click();
    let states = lock(&bench.wire).states.clone();
    eprintln!("MEASURE vanilla chest: click window {window} state {state}; last states {states:?}");

    assert_eq!(fate, ClickFate::Ok);
    assert_eq!(window, chest.id().window().get());
    assert_eq!(Some(&state), states.get(&window), "the chest's own state");
    assert_ne!(Some(&state), states.get(&0), "not the bot's inventory's");

    // The diamonds go back.
    let carrying = bench.observe().await.menu.screen.unwrap();
    assert_eq!(
        bench.click(&carrying, ClickKind::Left, 0).await,
        ClickFate::Ok
    );
    bench.close().await;
}

/// The click guard at the moment of sending: the server changed the
/// chest between what the hook saw and the click, and nothing went out.
async fn a_screen_that_changed_since_the_hook_takes_no_click(bench: &mut Bench) {
    let seen = open_chest(bench).await;
    let click = Click::on(&seen, ClickKind::Left, 1).unwrap();
    let before = bench.clicks_sent();

    bench
        .rcon(&format!(
            "item replace block {} {} {} container.4 with minecraft:gold_ingot",
            CHEST.0, CHEST.1, CHEST.2
        ))
        .await;
    bench
        .until_menu("the gold in the chest", |menu| {
            menu.screen
                .as_ref()
                .is_some_and(|screen| id_at(screen, 4).as_deref() == Some("minecraft:gold_ingot"))
        })
        .await;
    let refused = bench.menu(MenuCommand::Act(MenuAction::Click(click))).await;

    assert_eq!(
        refused,
        Err(MenuError::Refused(MenuRefusal::ContentChanged))
    );
    assert_eq!(bench.clicks_sent(), before, "nothing went out");
    bench.close().await;
}

async fn a_rejection_replaced_and_unanswered_on_vanilla(bench: &mut Bench) {
    // Rejected: a furnace takes nothing into its result slot, which azalea
    // doesn't know; the server puts the slot and the cursor back.
    let furnace = (2, FEET, 2);
    bench
        .rcon(&format!(
            "setblock {} {} {} minecraft:furnace",
            furnace.0, furnace.1, furnace.2
        ))
        .await;
    bench
        .use_at(f64::from(furnace.0) + 0.5, f64::from(FEET) + 0.5, 2.5)
        .await;
    let open = bench.screen("Furnace").await;
    let cobblestone = holding(&open, "minecraft:cobblestone", |slot| slot.index() >= 3);
    assert_eq!(
        bench.click(&open, ClickKind::Left, cobblestone).await,
        ClickFate::Ok
    );
    let carrying = bench.observe().await.menu.screen.unwrap();
    let rejected = bench.click(&carrying, ClickKind::Left, 2).await;
    let after = bench.observe().await.menu.screen.unwrap();
    assert_eq!(rejected, ClickFate::Rejected);
    assert_eq!(id_at(&after, 2), None, "the result slot stays empty");
    assert_eq!(
        bench.click(&after, ClickKind::Left, cobblestone).await,
        ClickFate::Ok
    );
    bench.close().await;

    // Replaced: the bot closes the chest before the reply can come.
    let chest = open_chest(bench).await;
    let click = Click::on(&chest, ClickKind::Left, 1).unwrap();
    bench.freeze().await;
    let sent = bench.menu(MenuCommand::Act(MenuAction::Click(click))).await;
    let closed = bench.menu(MenuCommand::Close(chest.id())).await;
    bench.thaw();
    assert_eq!((sent, closed), (Ok(()), Ok(())));
    assert_eq!(bench.fate(chest.id(), Some(1)).await, ClickFate::Replaced);

    unanswered(bench, open_chest(bench).await, 0).await;
}

/// Unanswered: no reply in 5 s while the server is frozen; the screen
/// takes no more until it's opened again, and the late reply answers
/// nothing.
async fn unanswered(bench: &mut Bench, screen: Screen, slot: u16) {
    let click = Click::on(&screen, ClickKind::Left, slot).unwrap();
    let again = Click::on(&screen, ClickKind::Left, slot).unwrap();
    bench.freeze().await;
    // Counted while nothing can answer: the server may answer while
    // `docker unpause` is still returning.
    let replies = lock(&bench.wire).stats_replies;
    let sent = bench.menu(MenuCommand::Act(MenuAction::Click(click))).await;
    let fate = bench.fate(screen.id(), Some(slot)).await;
    let refused = bench.menu(MenuCommand::Act(MenuAction::Click(again))).await;
    bench.thaw();
    assert_eq!(sent, Ok(()));
    assert_eq!(fate, ClickFate::Unanswered);
    assert_eq!(refused, Err(MenuError::Refused(MenuRefusal::Unanswered)));
    // The late reply comes and answers nothing.
    within("the late reply", async {
        while lock(&bench.wire).stats_replies == replies {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    ticks_pass(&bench.session, 5).await;
    let mut late = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), bench.events.next()).await
    {
        if matches!(event, SessionEvent::Clicked(_)) {
            late.push(event);
        }
    }
    assert_eq!(late, [], "a late reply answers nothing");
    bench.close().await;
}

async fn a_villagers_trade(bench: &mut Bench) {
    bench
        .rcon(&format!(
            "item replace entity {NAME} inventory.6 with minecraft:emerald 1"
        ))
        .await;
    bench
        .rcon(&format!(
            "summon minecraft:villager 1.5 {FEET} -1.5 {{NoAI:1b,Silent:1b,Invulnerable:1b,VillagerData:{{profession:\"minecraft:farmer\",level:2,type:\"minecraft:plains\"}},Offers:{{Recipes:[{{buy:{{id:\"minecraft:emerald\",count:1}},sell:{{id:\"minecraft:bread\",count:6}},maxUses:99}}]}}}}"
        ))
        .await;
    bench.use_at(1.5, f64::from(FEET) + 1.0, -1.5).await;
    let merchant = bench
        .until_menu("the villager's trades", |menu| {
            menu.screen.as_ref().is_some_and(|screen| {
                screen.kind() == ScreenKind::Merchant && !screen.offers().is_empty()
            })
        })
        .await
        .screen
        .unwrap();
    let [offer] = merchant.offers() else {
        panic!("{:?}", merchant.offers());
    };
    assert_eq!(offer.cost().id().as_str(), "minecraft:emerald");
    assert_eq!(offer.result().id().as_str(), "minecraft:bread");

    let chosen = bench
        .act(MenuAction::Trade(TradeChoice::on(&merchant, 0).unwrap()))
        .await;
    let paid = bench
        .until_menu("the bread to take", |menu| {
            menu.screen
                .as_ref()
                .is_some_and(|screen| id_at(screen, 2).as_deref() == Some("minecraft:bread"))
        })
        .await
        .screen
        .unwrap();
    let taken = bench.click(&paid, ClickKind::Shift, 2).await;
    eprintln!("MEASURE vanilla villager: trade {chosen}, take {taken}");
    bench.close().await;

    let inventory = bench
        .until_menu("the bread in the inventory", |menu| {
            menu.inventory.slots().any(|slot| {
                slot.item()
                    .is_some_and(|item| item.id().as_str() == "minecraft:bread")
            })
        })
        .await
        .inventory;
    assert_eq!(chosen, ClickFate::Ok);
    assert!(
        !inventory.slots().any(|slot| slot
            .item()
            .is_some_and(|item| item.id().as_str() == "minecraft:emerald")),
        "the emerald paid"
    );
}

async fn an_anvils_name(bench: &mut Bench) {
    let anvil = (-2, FEET, 0);
    bench
        .rcon(&format!(
            "setblock {} {} {} minecraft:anvil",
            anvil.0, anvil.1, anvil.2
        ))
        .await;
    bench
        .rcon(&format!(
            "item replace entity {NAME} inventory.7 with minecraft:diamond_sword"
        ))
        .await;
    bench.rcon(&format!("xp add {NAME} 30 levels")).await;
    bench
        .use_at(f64::from(anvil.0) + 0.5, f64::from(FEET) + 0.5, 0.5)
        .await;
    let open = bench.screen("Repair & Name").await;
    let sword = holding(&open, "minecraft:diamond_sword", |slot| slot.index() >= 3);
    let moved = bench.click(&open, ClickKind::Shift, sword).await;
    let placed = bench
        .until_menu("the sword in the anvil", |menu| {
            menu.screen.as_ref().is_some_and(|screen| {
                id_at(screen, 0).as_deref() == Some("minecraft:diamond_sword")
            })
        })
        .await
        .screen
        .unwrap();
    let name = AnvilName::try_from(&ChatMessage::try_from("Blade").unwrap()).unwrap();
    let renamed = bench
        .act(MenuAction::Rename(Rename::on(&placed, name).unwrap()))
        .await;
    let result = bench
        .until_menu("the named sword as the result", |menu| {
            menu.screen.as_ref().is_some_and(|screen| {
                screen
                    .slot(2)
                    .and_then(Slot::item)
                    .is_some_and(|item| item.name().as_str() == "Blade")
            })
        })
        .await
        .screen
        .unwrap();
    let taken = bench.click(&result, ClickKind::Shift, 2).await;
    eprintln!("MEASURE vanilla anvil: move {moved}, rename {renamed}, take {taken}");
    bench.close().await;

    assert_eq!(renamed, ClickFate::Ok);
    bench
        .until_menu("the named sword in the inventory", |menu| {
            menu.inventory.slots().any(|slot| {
                slot.item().is_some_and(|item| {
                    item.id().as_str() == "minecraft:diamond_sword"
                        && item.name().as_str() == "Blade"
                })
            })
        })
        .await;
}

async fn an_enchanting_tables_button(bench: &mut Bench) {
    let table = (-2, FEET, 2);
    bench
        .rcon(&format!(
            "setblock {} {} {} minecraft:enchanting_table",
            table.0, table.1, table.2
        ))
        .await;
    bench
        .rcon(&format!(
            "item replace entity {NAME} inventory.8 with minecraft:iron_pickaxe"
        ))
        .await;
    bench
        .rcon(&format!(
            "item replace entity {NAME} inventory.9 with minecraft:lapis_lazuli 3"
        ))
        .await;
    bench
        .use_at(f64::from(table.0) + 0.5, f64::from(FEET) + 0.75, 2.5)
        .await;
    let open = bench.screen("Enchant").await;
    let pickaxe = holding(&open, "minecraft:iron_pickaxe", |slot| slot.index() >= 2);
    let moved = bench.click(&open, ClickKind::Shift, pickaxe).await;
    let with_pickaxe = bench.observe().await.menu.screen.unwrap();
    let lapis = holding(&with_pickaxe, "minecraft:lapis_lazuli", |slot| {
        slot.index() >= 2
    });
    let paid = bench.click(&with_pickaxe, ClickKind::Shift, lapis).await;
    let ready = bench
        .until_menu("the pickaxe and the lapis in the table", |menu| {
            menu.screen.as_ref().is_some_and(|screen| {
                id_at(screen, 0).as_deref() == Some("minecraft:iron_pickaxe")
                    && id_at(screen, 1).as_deref() == Some("minecraft:lapis_lazuli")
            })
        })
        .await
        .screen
        .unwrap();
    // The table offers its enchantments a moment after both are in.
    ticks_pass(&bench.session, 10).await;
    let ready = bench
        .observe()
        .await
        .menu
        .screen
        .filter(|screen| screen.id() == ready.id())
        .unwrap();
    let pressed = bench
        .act(MenuAction::Button(ButtonPress::on(&ready, 0).unwrap()))
        .await;
    let enchanted = bench
        .until_menu("the pickaxe enchanted", |menu| {
            menu.screen.as_ref().is_some_and(|screen| {
                screen
                    .slot(0)
                    .and_then(Slot::item)
                    .is_some_and(|item| !item.enchantments().is_empty())
            })
        })
        .await
        .screen
        .unwrap();
    let names: Vec<_> = enchanted
        .slot(0)
        .and_then(Slot::item)
        .unwrap()
        .enchantments()
        .iter()
        .map(|(id, level)| format!("{}:{level}", id.as_str()))
        .collect();
    eprintln!(
        "MEASURE vanilla enchanting: move {moved}, lapis {paid}, button {pressed}, {names:?}"
    );
    bench.close().await;

    assert_eq!(pressed, ClickFate::Ok);
    assert!(
        names
            .iter()
            .all(|name| name.starts_with("minecraft:") && !name.contains("unknown")),
        "named by the server's registry: {names:?}"
    );
}

// --- The Paper harness: the four fates, a new screen, the user's server's
// auction flow and a sign nobody answers (P16.9 to P16.11) ---

#[tokio::test(flavor = "multi_thread")]
async fn slow_screens_on_paper() {
    let mut bench = Bench::new(Server::start_paper().await, "paper").await;
    bench.rcon("harness reset").await;

    the_four_fates_on_paper(&mut bench).await;
    a_new_screen_between_the_hook_and_its_click(&mut bench).await;
    the_users_auction_flow(&mut bench).await;
    a_sign_nobody_answers(&mut bench).await;

    let log = bench.server.log().await;
    let kicks: Vec<_> = log
        .lines()
        .filter(|line| line.contains("harness-kick"))
        .collect();
    let warnings: Vec<_> = log
        .lines()
        .filter(|line| line.contains("WARN") && line.contains(NAME))
        .collect();
    let (requests, replies) = {
        let wire = lock(&bench.wire);
        let requests = wire
            .sent
            .iter()
            .filter(|packet| matches!(packet, ServerboundGamePacket::ClientCommand(_)))
            .count();
        (requests, wire.stats_replies)
    };
    eprintln!(
        "MEASURE paper: {requests} statistics requests, {replies} replies; kicks {kicks:?}; warnings about the bot {warnings:?}"
    );
    assert_eq!(kicks, Vec::<&str>::new());
    within("disconnect", bench.session.disconnect()).await;
}

async fn the_four_fates_on_paper(bench: &mut Bench) {
    bench
        .rcon(&format!(
            "item replace entity {NAME} inventory.0 with minecraft:cobblestone 16"
        ))
        .await;
    bench.rcon(&format!("harness open {NAME} sell")).await;
    let sell = bench.screen("Sell Items").await;
    // Ok: the sell menu's slots take items, as a chest's do.
    let cobblestone = holding(&sell, "minecraft:cobblestone", |slot| slot.index() >= 54);
    assert_eq!(
        bench.click(&sell, ClickKind::Left, cobblestone).await,
        ClickFate::Ok
    );
    let carrying = bench.observe().await.menu.screen.unwrap();
    assert_eq!(
        bench.click(&carrying, ClickKind::Left, 0).await,
        ClickFate::Ok
    );
    // Rejected: a button, whose click the plugin cancels.
    let placed = bench.observe().await.menu.screen.unwrap();
    assert_eq!(
        bench.click(&placed, ClickKind::Left, 49).await,
        ClickFate::Rejected
    );
    // Replaced: the bot closes the menu before the reply.
    let before = bench.observe().await.menu.screen.unwrap();
    let click = Click::on(&before, ClickKind::Left, 0).unwrap();
    bench.freeze().await;
    let sent = bench.menu(MenuCommand::Act(MenuAction::Click(click))).await;
    let closed = bench.menu(MenuCommand::Close(before.id())).await;
    bench.thaw();
    assert_eq!((sent, closed), (Ok(()), Ok(())));
    assert_eq!(bench.fate(before.id(), Some(0)).await, ClickFate::Replaced);
    // Unanswered.
    bench.rcon(&format!("harness open {NAME} sell")).await;
    let again = bench.screen("Sell Items").await;
    unanswered(bench, again, 0).await;
}

/// The plugin opens its auction house again under the same title while
/// the hook still has the first: the click goes nowhere.
async fn a_new_screen_between_the_hook_and_its_click(bench: &mut Bench) {
    bench.rcon("harness fill 10").await;
    bench.rcon(&format!("harness open {NAME} ah")).await;
    let seen = bench.screen("Auction House").await;
    let click = Click::on(&seen, ClickKind::Left, 0).unwrap();
    let before = bench.clicks_sent();

    bench.rcon(&format!("harness open {NAME} ah")).await;
    bench
        .until_menu("the second auction house", |menu| {
            menu.screen
                .as_ref()
                .is_some_and(|screen| screen.id() != seen.id() && screen.state().get() > 0)
        })
        .await;
    let refused = bench.menu(MenuCommand::Act(MenuAction::Click(click))).await;

    assert_eq!(refused, Err(MenuError::Refused(MenuRefusal::ScreenChanged)));
    assert_eq!(bench.clicks_sent(), before, "nothing went out");
    bench.close().await;
}

/// The user's server's flow (P16.11's note, ADR-0036 M17), with
/// fleet-mc's own calls: a search, the Create button by its slot in the
/// last row, the potion selected wherever it is in the inventory, a
/// confirm screen, the price on a sign, the final confirm.
async fn the_users_auction_flow(bench: &mut Bench) {
    bench.rcon("harness reset").await;
    bench.rcon("harness fill 10").await;
    // A listing renamed "Create", in the rows above the buttons.
    bench
        .rcon("harness list Seller2 100 minecraft:emerald[custom_name=\"Create\"]")
        .await;
    bench
        .rcon(&format!(
            "item replace entity {NAME} inventory.13 with minecraft:potion[potion_contents={{potion:\"minecraft:turtle_master\"}}]"
        ))
        .await;
    bench
        .until_menu("the potion in the inventory", |menu| {
            menu.inventory.slots().any(|slot| {
                slot.item()
                    .and_then(|item| item.potion())
                    .is_some_and(|potion| potion.as_str() == "minecraft:turtle_master")
            })
        })
        .await;

    // 1. A search shows only what matches.
    bench.chat("/ah turtle").await;
    let found = bench.screen("Auction House").await;
    let listed: Vec<_> = found
        .own()
        .filter(|slot| slot.place().is_some_and(|place| place.row < 5))
        .filter_map(Slot::item)
        .collect();
    assert_ne!(listed.len(), 0, "the search found the potions");
    assert!(
        listed.iter().all(|item| item
            .potion()
            .is_some_and(|potion| potion.as_str().contains("turtle_master"))),
        "{listed:#?}"
    );
    bench.close().await;

    listing_a_potion_through_the_flow(bench).await;
}

/// Steps 2 to 7 of the flow: the auction house, Create by its slot, the
/// potion, the confirm screen, the price's sign and the final confirm.
async fn listing_a_potion_through_the_flow(bench: &mut Bench) {
    // 2. The auction house; Create in the last row, and the trap above.
    bench.chat("/ah").await;
    let house = bench.screen("Auction House").await;
    let grid = house.grid().unwrap();
    let create = house.at(grid.rows - 1, 2).unwrap();
    let create_item = create.item().unwrap();
    let trap = house
        .own()
        .filter(|slot| slot.place().is_some_and(|place| place.row < grid.rows - 1))
        .find(|slot| {
            slot.item()
                .is_some_and(|item| item.name().as_str() == "Create")
        })
        .expect("the listing renamed Create");
    assert_eq!(create_item.name().as_str(), "Create");
    assert_eq!(create_item.id().as_str(), "minecraft:emerald");
    assert_eq!(trap.item().unwrap().id(), create_item.id());
    assert_ne!(
        trap.item(),
        Some(create_item),
        "two items, told apart whole"
    );

    // 3. Create, by its slot.
    assert_eq!(
        bench.click(&house, ClickKind::Left, create.index()).await,
        ClickFate::Rejected,
        "a plugin's button is cancelled and put back"
    );
    // 4. The potion, wherever it is in the inventory.
    let select = bench.screen("Create Listing").await;
    let potion = select
        .player()
        .find(|slot| {
            slot.item()
                .and_then(|item| item.potion())
                .is_some_and(|potion| potion.as_str() == "minecraft:turtle_master")
        })
        .expect("the potion")
        .index();
    assert_eq!(
        bench.click(&select, ClickKind::Left, potion).await,
        ClickFate::Rejected
    );
    // 5. The confirm screen.
    let confirm = bench.screen("Confirm Item").await;
    assert_eq!(
        confirm
            .slot(13)
            .and_then(Slot::item)
            .and_then(|item| item.potion())
            .map(|p| p.as_str().to_owned())
            .as_deref(),
        Some("minecraft:turtle_master")
    );
    assert_eq!(
        bench.click(&confirm, ClickKind::Left, 11).await,
        ClickFate::Rejected
    );
    // 6. The price on the sign the server opens.
    let editor = bench
        .until_menu("the sign editor", |menu| {
            menu.sign.is_some() && menu.screen.is_none()
        })
        .await
        .sign
        .unwrap();
    let price = price_sign(1_500);
    assert_eq!(
        bench
            .menu(MenuCommand::AnswerSign {
                editor,
                answer: SignAnswer::Write(price),
            })
            .await,
        Ok(())
    );
    // 7. The final confirm.
    let last = bench.screen("Confirm Listing").await;
    assert!(
        last.slot(13)
            .and_then(Slot::item)
            .is_some_and(|item| item.lore().has("Price: $1,500.00")),
        "{last:#?}"
    );
    assert_eq!(
        bench.click(&last, ClickKind::Left, 11).await,
        ClickFate::Rejected
    );
    bench
        .until_menu("the confirm closed", |menu| menu.screen.is_none())
        .await;

    the_potion_is_listed(bench).await;
}

/// The user's server's price sign: the price alone on its first line.
fn price_sign(price: i64) -> fleet_core::script::menu::SignText {
    SignTemplate::try_from(SignDraft {
        key: "price".to_owned(),
        lines: vec!["{price}".to_owned()],
        values: vec![ValueDraft {
            name: "price".to_owned(),
            kind: ValueKind::Integer {
                min: 1,
                max: 1_000_000,
            },
        }],
    })
    .unwrap()
    .render(&[("price", Given::Integer(price))])
    .unwrap()
}

/// The listing is the bot's at its price, nothing was bought, and the
/// potion left the inventory.
async fn the_potion_is_listed(bench: &Bench) {
    let listings = bench.rcon("harness listings").await;
    assert!(
        listings
            .lines()
            .any(|line| line.ends_with(&format!("{NAME} 1500 minecraft:potion"))),
        "{listings}"
    );
    assert!(
        !bench.server.log().await.contains("harness-bought"),
        "nothing was bought: the trap was never clicked"
    );
    let left = bench.observe().await.menu.inventory;
    assert!(
        !left
            .slots()
            .any(|slot| slot.item().and_then(|item| item.potion()).is_some()),
        "the potion was listed"
    );
}

/// A sign editor the script doesn't answer is never answered: not while it
/// waits, not when a screen ends it (ADR-0036, M16).
async fn a_sign_nobody_answers(bench: &mut Bench) {
    let signs = bench.signs_sent();
    let logged = bench
        .server
        .log()
        .await
        .matches("harness-sign player=")
        .count();
    bench.rcon(&format!("harness sign {NAME}")).await;
    wait_for(&mut bench.events, "the sign editor", |event| {
        matches!(event, SessionEvent::SignOpened(_))
    })
    .await;
    assert!(bench.observe().await.menu.sign.is_some());
    ticks_pass(&bench.session, 40).await;
    bench.rcon(&format!("harness open {NAME} ah")).await;
    bench.screen("Auction House").await;
    ticks_pass(&bench.session, 20).await;

    assert_eq!(bench.observe().await.menu.sign, None, "the screen ended it");
    assert_eq!(bench.signs_sent(), signs, "the bot sent no sign");
    assert_eq!(
        bench
            .server
            .log()
            .await
            .matches("harness-sign player=")
            .count(),
        logged,
        "the server got no sign"
    );
    bench.close().await;
}
