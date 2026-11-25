//! What a server sends after a click, and how fast clicks may come (Plan.md
//! P16.2; ADR-0036's amendment "Phase 16: what group A measured"; ADR-0049).
//!
//! Each scenario runs once on the vanilla test server and once on the Paper
//! harness, with azalea's own clicks and its prediction of their result, as a
//! script's clicks will go out (ADR-0036, M3). A hook on the bot's App drives
//! the screens from a queue of [`Act`]s at its game ticks and keeps a
//! [`Book`] of every screen packet the bot hears and every click it sends,
//! each with the game tick it came in, counted from the hook's start.
//!
//! **A click carries the state ID the server last gave its window,** as
//! fleet-mc's own clicks will (ADR-0036's amendment, R1): azalea 0.16.0 keeps
//! one state ID for every window and takes it from any window's slot packet,
//! so on a chest its own is the bot's inventory's, and the server reads every
//! click as stale. One click per server keeps azalea's, to show that. The
//! scenarios print `MEASURE` lines, the numbers the ADR records, and assert
//! the facts its two rules rest on.

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use azalea::BlockPos;
use azalea::app::{App, Update};
use azalea::core::tick::GameTick;
use azalea::ecs::entity::Entity;
use azalea::ecs::message::{MessageReader, MessageWriter};
use azalea::ecs::observer::On;
use azalea::ecs::query::With;
use azalea::ecs::schedule::IntoScheduleConfigs as _;
use azalea::ecs::system::{Commands, Query};
use azalea::entity::LocalEntity;
use azalea::entity::inventory::Inventory;
use azalea::interact::StartUseItemEvent;
use azalea::inventory::operations::{ClickOperation, PickupClick, QuickMoveClick, SwapClick};
use azalea::inventory::{CloseContainerEvent, ContainerClickEvent};
use azalea::packet::game::{ReceiveGamePacketEvent, SendGamePacketEvent};
use azalea::physics::PhysicsSystems;
use azalea::protocol::packets::game::s_client_command::{
    self as client_command, ServerboundClientCommand,
};
use azalea::protocol::packets::game::s_interact::InteractionHand;
use azalea::protocol::packets::game::{ClientboundGamePacket, ServerboundGamePacket};
use fleet_core::id::BotId;
use fleet_core::mc::{SessionEvent, SessionEvents as _};
use fleet_core::net::TargetPolicy;
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession};

use crate::harness::{Mode, Server, WITHIN, bot, connect, offline, wait_for};

/// The bot's name.
const NAME: &str = "AfkBot1";
/// Where the bot stands, on the flat world's grass.
const FLOOR: i32 = -60;
/// The chest beside it.
const CHEST: BlockPos = BlockPos::new(2, FLOOR, 0);
/// How long a click's answer may take to come before the scenario stops
/// waiting for more: a second, twenty game ticks.
const ANSWER: u64 = 20;
/// The bot's own inventory's first hotbar slot, as its window numbers it.
const HOTBAR_1: u16 = 36;
/// Its inventory's first slot above the hotbar.
const INVENTORY_1: u16 = 9;

/// What the test asks the bot's App to do at its next game tick.
#[derive(Debug, Clone)]
enum Act {
    /// Right-click the block at a place, as a player opens a chest.
    Use(BlockPos),
    /// One click on the open window, with azalea's prediction and the state
    /// ID the server last gave the window.
    Click(ClickOperation),
    /// The same with azalea's own state ID.
    AzaleaClick(ClickOperation),
    /// The same with a state ID one below the server's last: what a click on
    /// a screen the server has since changed sends.
    StaleClick(ClickOperation),
    /// A click as [`Act::Click`], and in the same tick a request for the
    /// bot's statistics, which the server answers on its main thread in the
    /// order it reads them.
    ClickAndAsk(ClickOperation),
    /// The same with a stale state ID.
    StaleClickAndAsk(ClickOperation),
    /// Close the open window.
    Close,
    /// `per_tick` pickups each game tick for `ticks` ticks on the bot's own
    /// inventory, moving one stack between two slots and back.
    Burst {
        per_tick: u32,
        ticks: u32,
        one: u16,
        other: u16,
    },
}

/// A screen packet the bot heard.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Heard {
    Open {
        window: i32,
        title: String,
    },
    Content {
        window: i32,
        state: u32,
    },
    Slot {
        window: i32,
        state: u32,
        slot: u16,
    },
    Cursor,
    PlayerSlot {
        slot: u32,
    },
    Close {
        window: i32,
    },
    Data {
        window: i32,
    },
    /// The answer to a request for the bot's statistics.
    Stats,
}

/// A click the bot sent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    tick: u64,
    window: i32,
    state: u32,
    slot: i16,
    /// The slots azalea predicted the click changes.
    changed: Vec<u16>,
}

/// A burst under way.
#[derive(Debug, Clone)]
struct Burst {
    per_tick: u32,
    ticks_left: u32,
    pattern: [u16; 4],
    next: usize,
}

/// What the hook saw, shared with the test.
#[derive(Debug, Default)]
struct Book {
    /// Game ticks since the hook started.
    tick: u64,
    heard: Vec<(u64, Heard)>,
    sent: Vec<Sent>,
    /// The game tick a right-click on a block went out.
    used: Option<u64>,
    acts: VecDeque<Act>,
    burst: Option<Burst>,
    /// The open window as azalea has it, and its state ID.
    window: i32,
    state: u32,
    /// The state ID the server last gave each window.
    states: std::collections::HashMap<i32, u32>,
    /// The bot's own inventory, as azalea has it: each slot's item and count.
    own: Vec<(String, i32)>,
}

type Shared = Arc<Mutex<Book>>;

fn book(shared: &Shared) -> std::sync::MutexGuard<'_, Book> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `packet` as a [`Heard`], if it concerns a screen.
fn heard(packet: &ClientboundGamePacket) -> Option<Heard> {
    Some(match packet {
        ClientboundGamePacket::OpenScreen(open) => Heard::Open {
            window: open.container_id,
            title: open.title.to_string(),
        },
        ClientboundGamePacket::ContainerSetContent(content) => Heard::Content {
            window: content.container_id,
            state: content.state_id,
        },
        ClientboundGamePacket::ContainerSetSlot(slot) => Heard::Slot {
            window: slot.container_id,
            state: slot.state_id,
            slot: slot.slot,
        },
        ClientboundGamePacket::SetCursorItem(_) => Heard::Cursor,
        ClientboundGamePacket::SetPlayerInventory(slot) => Heard::PlayerSlot { slot: slot.slot },
        ClientboundGamePacket::ContainerClose(close) => Heard::Close {
            window: close.container_id,
        },
        ClientboundGamePacket::ContainerSetData(data) => Heard::Data {
            window: data.container_id,
        },
        ClientboundGamePacket::AwardStats(_) => Heard::Stats,
        _ => return None,
    })
}

/// The App hook that drives `tested`'s screens from `shared` and books what
/// it hears and sends.
fn drive(tested: BotId, shared: &Shared) -> impl Fn(BotId, &mut App) + Send + Sync + 'static {
    let shared = Arc::clone(shared);
    move |bot_id, app| {
        if bot_id != tested {
            return;
        }
        let ticking = Arc::clone(&shared);
        app.add_systems(
            GameTick,
            (move |mut commands: Commands,
                   mut bots: Query<(Entity, &mut Inventory), With<LocalEntity>>,
                   mut uses: MessageWriter<StartUseItemEvent>| {
                let Some((entity, mut inventory)) = bots.iter_mut().next() else {
                    return;
                };
                let mut book = book(&ticking);
                book.tick += 1;
                book.window = inventory.id;
                book.state = inventory.state_id;
                book.own = inventory
                    .inventory_menu
                    .slots()
                    .iter()
                    .map(|item| (format!("{:?}", item.kind()), item.count()))
                    .collect();
                burst(&mut book, entity, &mut inventory, &mut commands);
                while let Some(act) = book.acts.pop_front() {
                    if let Act::Use(at) = act {
                        uses.write(StartUseItemEvent {
                            entity,
                            hand: InteractionHand::MainHand,
                            force_block: Some(at),
                        });
                    } else {
                        perform(act, &mut book, entity, &mut inventory, &mut commands);
                    }
                }
            })
            .after(PhysicsSystems),
        );
        let hearing = Arc::clone(&shared);
        app.add_systems(
            Update,
            move |mut packets: MessageReader<ReceiveGamePacketEvent>| {
                let mut book = book(&hearing);
                for received in packets.read() {
                    if let Some(heard) = heard(&received.packet) {
                        if let Heard::Content { window, state }
                        | Heard::Slot { window, state, .. } = heard
                        {
                            book.states.insert(window, state);
                        }
                        let tick = book.tick;
                        book.heard.push((tick, heard));
                    }
                }
            },
        );
        let sending = Arc::clone(&shared);
        app.add_observer(move |sent: On<SendGamePacketEvent>| {
            let mut book = book(&sending);
            let tick = book.tick;
            match &sent.event().packet {
                ServerboundGamePacket::ContainerClick(click) => book.sent.push(Sent {
                    tick,
                    window: click.container_id,
                    state: click.state_id,
                    slot: click.slot_num,
                    changed: click.changed_slots.keys().copied().collect(),
                }),
                ServerboundGamePacket::UseItemOn(_) => book.used = Some(tick),
                _ => {}
            }
        });
    }
}

/// This tick's clicks of the burst under way, if there is one.
fn burst(book: &mut Book, entity: Entity, inventory: &mut Inventory, commands: &mut Commands) {
    let Some(mut burst) = book.burst.take() else {
        return;
    };
    let window = inventory.id;
    if let Some(state) = book.states.get(&window) {
        inventory.state_id = *state;
    }
    for _ in 0..burst.per_tick {
        let slot = burst.pattern[burst.next % 4];
        burst.next += 1;
        commands.trigger(ContainerClickEvent {
            entity,
            window_id: window,
            operation: pickup(slot),
        });
    }
    burst.ticks_left -= 1;
    if burst.ticks_left > 0 {
        book.burst = Some(burst);
    }
}

/// Does `act`, every one but a right-click, at this game tick.
fn perform(
    act: Act,
    book: &mut Book,
    entity: Entity,
    inventory: &mut Inventory,
    commands: &mut Commands,
) {
    let window = inventory.id;
    let servers = book.states.get(&window).copied();
    let act = match act {
        Act::ClickAndAsk(operation) => {
            perform(Act::Click(operation), book, entity, inventory, commands);
            ask_for_stats(entity, commands);
            return;
        }
        Act::StaleClickAndAsk(operation) => {
            perform(
                Act::StaleClick(operation),
                book,
                entity,
                inventory,
                commands,
            );
            ask_for_stats(entity, commands);
            return;
        }
        other => other,
    };
    match act {
        Act::Click(operation) => {
            if let Some(state) = servers {
                inventory.state_id = state;
            }
            commands.trigger(ContainerClickEvent {
                entity,
                window_id: window,
                operation,
            });
        }
        Act::AzaleaClick(operation) => commands.trigger(ContainerClickEvent {
            entity,
            window_id: window,
            operation,
        }),
        Act::StaleClick(operation) => {
            inventory.state_id = servers.unwrap_or(inventory.state_id).wrapping_sub(1);
            commands.trigger(ContainerClickEvent {
                entity,
                window_id: window,
                operation,
            });
        }
        Act::Close => commands.trigger(CloseContainerEvent { entity, id: window }),
        Act::Burst {
            per_tick,
            ticks,
            one,
            other,
        } => {
            book.burst = Some(Burst {
                per_tick,
                ticks_left: ticks,
                pattern: [one, other, other, one],
                next: 0,
            });
        }
        Act::Use(_) | Act::ClickAndAsk(_) | Act::StaleClickAndAsk(_) => {}
    }
}

/// Asks the server for the bot's statistics, as the vanilla client's
/// statistics screen does.
fn ask_for_stats(entity: Entity, commands: &mut Commands) {
    commands.trigger(SendGamePacketEvent::new(
        entity,
        ServerboundClientCommand {
            action: client_command::Action::RequestStats,
        },
    ));
}

/// A bot on a server, with its book.
struct Bench {
    server: Server,
    /// Kept: dropping it would end the session.
    _session: McSession,
    events: McEvents,
    shared: Shared,
    /// The server's name in the report.
    name: &'static str,
}

impl Bench {
    async fn new(server: Server, name: &'static str) -> Self {
        let shared = Arc::new(Mutex::new(Book::default()));
        let connector = AzaleaConnector::new(&McConfig::default())
            .with_targets(TargetPolicy::Unchecked)
            .with_app_hook(drive(bot(1), &shared));
        let (session, mut events) = connect(&connector, &server, bot(1), offline(NAME)).await;
        wait_for(&mut events, "the join", |event| {
            matches!(event, SessionEvent::Joined)
        })
        .await;
        let bench = Self {
            server,
            _session: session,
            events,
            shared,
            name,
        };
        bench.rcon(&format!("tp {NAME} 0.5 {FLOOR} 0.5")).await;
        bench.rcon(&format!("clear {NAME}")).await;
        bench
            .rcon(&format!("give {NAME} minecraft:cobblestone 64"))
            .await;
        bench
            .until("the cobblestone in the first hotbar slot", |book| {
                book.own
                    .get(usize::from(HOTBAR_1))
                    .is_some_and(|(kind, count)| kind.contains("Cobblestone") && *count == 64)
            })
            .await;
        bench
    }

    async fn rcon(&self, command: &str) -> String {
        self.server.rcon(command).await
    }

    /// Waits until the book satisfies `done`, within [`WITHIN`].
    async fn until(&self, what: &str, done: impl Fn(&Book) -> bool) {
        until(&self.shared, what, done).await;
    }

    /// The bot's game tick now.
    fn tick(&self) -> u64 {
        book(&self.shared).tick
    }

    /// Queues `act` and waits until it's been done.
    async fn act(&self, act: Act) {
        book(&self.shared).acts.push_back(act);
        self.until("the act", |book| book.acts.is_empty()).await;
    }

    /// Clicks with `act`, waits [`ANSWER`] ticks, and returns the click as
    /// sent and every screen packet heard from its tick on, with the ticks
    /// counted from the click's.
    async fn click(&self, act: Act) -> (Sent, Vec<(u64, Heard)>) {
        let before = book(&self.shared).sent.len();
        self.act(act).await;
        self.until("the click going out", |book| book.sent.len() > before)
            .await;
        let sent = book(&self.shared).sent[before].clone();
        let end = sent.tick + ANSWER;
        self.until("the answer's time", |book| book.tick >= end)
            .await;
        let answered = book(&self.shared)
            .heard
            .iter()
            .filter(|(tick, _)| *tick >= sent.tick && *tick <= end)
            .map(|(tick, heard)| (tick - sent.tick, heard.clone()))
            .collect();
        (sent, answered)
    }

    /// Prints a measurement line.
    fn report(&self, what: &str, sent: &Sent, answered: &[(u64, Heard)]) {
        eprintln!(
            "MEASURE {} {what}: sent at tick {} window {} state {} slot {} predicting slots {:?}; heard within {ANSWER} ticks: {answered:?}",
            self.name, sent.tick, sent.window, sent.state, sent.slot, sent.changed
        );
    }
}

/// Waits until `shared` satisfies `done`, within [`WITHIN`], looking every
/// 5 ms: a game tick is 50.
async fn until(shared: &Shared, what: &str, done: impl Fn(&Book) -> bool) {
    let waited = tokio::time::timeout(WITHIN, async {
        while !done(&book(shared)) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        waited.is_ok(),
        "{what} didn't happen within {WITHIN:?}; the book: {:#?}",
        book(shared)
    );
}

/// The session's end, as the server gave it, or none if its events ended.
async fn disconnected(events: &mut McEvents) -> String {
    loop {
        match events.next().await {
            Some(SessionEvent::Disconnected(reason)) => return format!("{reason:?}"),
            None => return "the events ended".to_owned(),
            Some(_) => {}
        }
    }
}

/// A left click on `slot`.
fn pickup(slot: u16) -> ClickOperation {
    ClickOperation::Pickup(PickupClick::Left { slot: Some(slot) })
}

// --- What follows a click ------------------------------------------------------------------

/// The most game ticks a server's answer took on loopback: 0 or 1 in every
/// measurement, so 2 leaves a tick for a slow machine.
const PROMPT: u64 = 2;

/// Where the first of `answered` that `wanted` matches is, failing with
/// `what` and the answer when there is none or it came later than [`PROMPT`].
fn place(answered: &[(u64, Heard)], what: &str, wanted: impl Fn(&Heard) -> bool) -> usize {
    let (at, (tick, _)) = answered
        .iter()
        .enumerate()
        .find(|(_, (_, heard))| wanted(heard))
        .unwrap_or_else(|| panic!("no {what} in {answered:?}"));
    assert!(
        *tick <= PROMPT,
        "{what} came {tick} ticks late: {answered:?}"
    );
    at
}

/// Whether `heard` is a statistics reply.
fn stats(heard: &Heard) -> bool {
    *heard == Heard::Stats
}

/// Whether `heard` is the whole content of `window` with a state newer than
/// `than`.
fn content_of(heard: &Heard, window: i32, than: u32) -> bool {
    matches!(heard, Heard::Content { window: w, state } if *w == window && *state > than)
}

/// Whether `heard` is a slot packet for `slot` of `window`.
fn slot_of(heard: &Heard, window: i32, slot: u16) -> bool {
    matches!(heard, Heard::Slot { window: w, slot: s, .. } if *w == window && *s == slot)
}

/// Whether `heard` opens a screen titled `title`.
fn opens(heard: &Heard, title: &str) -> bool {
    matches!(heard, Heard::Open { title: t, .. } if t == title)
}

#[tokio::test]
async fn slow_what_follows_a_click_on_vanilla() {
    let bench = Bench::new(Server::start(Mode::Offline).await, "vanilla").await;
    own_inventory(&bench).await;
    chest(&bench).await;
}

#[tokio::test]
async fn slow_what_follows_a_click_on_paper() {
    let bench = Bench::new(Server::start_paper().await, "paper").await;
    own_inventory(&bench).await;
    chest(&bench).await;
    plugin_buttons(&bench).await;
}

/// A stack picked up and put down in the bot's own inventory, with no screen
/// open: window 0. A click the server takes as predicted is answered with
/// nothing at all; a stale one with the window's whole content; and a
/// statistics request sent right after a click is answered after whatever the
/// click brought.
async fn own_inventory(bench: &Bench) {
    let (sent, answered) = bench.click(Act::Click(pickup(HOTBAR_1))).await;
    bench.report("own inventory: pick up a stack", &sent, &answered);
    assert_eq!(answered, [], "a click taken as predicted");
    let (sent, answered) = bench.click(Act::Click(pickup(INVENTORY_1))).await;
    bench.report("own inventory: put it down", &sent, &answered);
    assert_eq!(answered, [], "a click taken as predicted");
    let (sent, answered) = bench.click(Act::StaleClick(pickup(INVENTORY_1))).await;
    bench.report(
        "own inventory: pick it up with a stale state",
        &sent,
        &answered,
    );
    place(&answered, "the whole content", |heard| {
        content_of(heard, 0, sent.state)
    });
    let (sent, answered) = bench.click(Act::Click(pickup(HOTBAR_1))).await;
    bench.report("own inventory: put it back", &sent, &answered);
    assert_eq!(answered, [], "a click taken as predicted");
    let (sent, answered) = bench.click(Act::ClickAndAsk(pickup(HOTBAR_1))).await;
    bench.report(
        "own inventory: pick it up, then ask for the statistics",
        &sent,
        &answered,
    );
    place(&answered, "the statistics", stats);
    assert_eq!(answered.len(), 1, "the reply alone: {answered:?}");
    let (sent, answered) = bench.click(Act::StaleClickAndAsk(pickup(HOTBAR_1))).await;
    bench.report(
        "own inventory: put it down with a stale state, then ask",
        &sent,
        &answered,
    );
    let content = place(&answered, "the whole content", |heard| {
        content_of(heard, 0, sent.state)
    });
    let reply = place(&answered, "the statistics", stats);
    assert!(
        content < reply,
        "the correction before the reply: {answered:?}"
    );
}

/// A vanilla chest: opened with a right-click, stacks moved between it and
/// the bot's inventory, a shift-click with a stale state, closed. azalea's
/// own state ID is the bot's inventory's, not the chest's.
async fn chest(bench: &Bench) {
    bench
        .rcon(&format!(
            "setblock {} {} {} minecraft:chest{{Items:[{{Slot:0b,id:\"minecraft:diamond\",count:5}},{{Slot:1b,id:\"minecraft:emerald\",count:3}}]}}",
            CHEST.x, CHEST.y, CHEST.z
        ))
        .await;
    let opened_from = bench.tick();
    bench.act(Act::Use(CHEST)).await;
    bench
        .until("the chest's screen and its content", |book| {
            book.heard.iter().any(|(tick, heard)| {
                *tick >= opened_from
                    && matches!(heard, Heard::Content { window, .. } if *window > 0)
            })
        })
        .await;
    let (used, opened) = {
        let book = book(&bench.shared);
        let used = book.used.expect("the right-click went out");
        let after: Vec<_> = book
            .heard
            .iter()
            .filter(|(tick, _)| *tick >= used)
            .map(|(tick, heard)| (tick - used, heard.clone()))
            .collect();
        (used, after)
    };
    eprintln!(
        "MEASURE {} chest: right-click at tick {used}; heard: {opened:?}",
        bench.name
    );
    let open = place(&opened, "the chest's screen", |heard| opens(heard, "Chest"));
    let Heard::Open { window, .. } = opened[open].1 else {
        unreachable!("place found an Open");
    };
    let content = place(&opened, "the chest's content", |heard| {
        content_of(heard, window, 0)
    });
    assert!(open < content, "{opened:?}");
    // The chest's 27 slots, then the bot's 27 and its hotbar.
    let (sent, answered) = bench.click(Act::AzaleaClick(pickup(0))).await;
    bench.report(
        "chest: take the diamonds with azalea's own state",
        &sent,
        &answered,
    );
    place(&answered, "the whole content", |heard| {
        content_of(heard, window, 0)
    });
    for (what, slot) in [
        ("chest: put them in the bot's inventory", 27),
        ("chest: take them up again", 27),
        ("chest: put them back in the chest", 0),
    ] {
        let (sent, answered) = bench.click(Act::Click(pickup(slot))).await;
        bench.report(what, &sent, &answered);
        assert_eq!(answered, [], "{what}: a click taken as predicted");
    }
    let (sent, answered) = bench
        .click(Act::StaleClick(ClickOperation::QuickMove(
            QuickMoveClick::Left { slot: 1 },
        )))
        .await;
    bench.report(
        "chest: shift-click the emeralds with a stale state",
        &sent,
        &answered,
    );
    place(&answered, "the whole content", |heard| {
        content_of(heard, window, sent.state)
    });
    let (sent, answered) = bench
        .click(Act::Click(ClickOperation::QuickMove(
            QuickMoveClick::Left { slot: 1 },
        )))
        .await;
    bench.report(
        "chest: shift-click the emeralds after the resync",
        &sent,
        &answered,
    );
    assert_eq!(answered, [], "a click taken as predicted");
    let closed_from = bench.tick();
    bench.act(Act::Close).await;
    let until = closed_from + ANSWER;
    bench
        .until("the close's time", |book| book.tick >= until)
        .await;
    let after: Vec<_> = book(&bench.shared)
        .heard
        .iter()
        .filter(|(tick, _)| *tick > closed_from)
        .cloned()
        .collect();
    eprintln!(
        "MEASURE {} chest: closed at tick {closed_from}; heard: {after:?}",
        bench.name
    );
    assert_eq!(after, [], "a close the bot sent is answered with nothing");
}

/// The harness plugin's auction house: a page turned in place, a listing
/// opened and cancelled, a shift-click and a number key on a listing, and a
/// purchase. Every click on a button is cancelled, and the server puts back
/// each slot the click said it changed, and the cursor; a screen the plugin
/// opens on its next tick comes after a statistics request's reply.
async fn plugin_buttons(bench: &Bench) {
    bench.rcon("harness reset").await;
    bench.rcon(&format!("harness money {NAME} 1000000")).await;
    bench.rcon("harness fill 50").await;
    let opened_from = bench.tick();
    bench.rcon(&format!("harness open {NAME} ah")).await;
    bench
        .until("the auction house", |book| {
            book.heard
                .iter()
                .any(|(tick, heard)| *tick >= opened_from && opens(heard, "Auction House"))
        })
        .await;
    let (sent, answered) = bench.click(Act::Click(pickup(53))).await;
    bench.report("plugin: next page", &sent, &answered);
    let window = sent.window;
    let redrawn = answered
        .iter()
        .filter(|(_, heard)| matches!(heard, Heard::Slot { window: w, .. } if *w == window))
        .count();
    assert!(
        redrawn >= 45,
        "the page redrawn in place, slot by slot: {answered:?}"
    );
    place(&answered, "the button put back", |heard| {
        slot_of(heard, window, 53)
    });
    place(&answered, "the cursor put back", |heard| {
        *heard == Heard::Cursor
    });
    let (sent, answered) = bench.click(Act::Click(pickup(0))).await;
    bench.report("plugin: a listing", &sent, &answered);
    let put_back = place(&answered, "the listing put back", |heard| {
        slot_of(heard, sent.window, 0)
    });
    let confirm = place(&answered, "the confirmation", |heard| {
        opens(heard, "Confirm Purchase")
    });
    assert!(put_back < confirm, "{answered:?}");
    let (sent, answered) = bench.click(Act::Click(pickup(15))).await;
    bench.report("plugin: cancel", &sent, &answered);
    place(&answered, "the auction house again", |heard| {
        opens(heard, "Auction House")
    });
    let (sent, answered) = bench
        .click(Act::Click(ClickOperation::QuickMove(
            QuickMoveClick::Left { slot: 2 },
        )))
        .await;
    bench.report("plugin: shift-click a listing", &sent, &answered);
    for &slot in &sent.changed {
        place(&answered, "a predicted slot put back", |heard| {
            slot_of(heard, sent.window, slot)
        });
    }
    let (sent, answered) = bench
        .click(Act::Click(ClickOperation::Swap(SwapClick {
            source_slot: 3,
            target_slot: 1,
        })))
        .await;
    bench.report("plugin: number key 2 on a listing", &sent, &answered);
    for &slot in &sent.changed {
        place(&answered, "a predicted slot put back", |heard| {
            slot_of(heard, sent.window, slot)
        });
    }
    place(&answered, "the cursor put back", |heard| {
        *heard == Heard::Cursor
    });
    plugin_purchase(bench).await;
}

/// The plugin's screens again, a statistics request after each click: the
/// reply comes after the click's own correction and before the screen the
/// plugin opens on its next tick. Then a purchase.
async fn plugin_purchase(bench: &Bench) {
    let (sent, answered) = bench.click(Act::ClickAndAsk(pickup(15))).await;
    bench.report(
        "plugin: cancel, then ask for the statistics",
        &sent,
        &answered,
    );
    let put_back = place(&answered, "the button put back", |heard| {
        slot_of(heard, sent.window, 15)
    });
    let reply = place(&answered, "the statistics", stats);
    let next = place(&answered, "the auction house again", |heard| {
        opens(heard, "Auction House")
    });
    assert!(put_back < reply && reply < next, "{answered:?}");
    let (sent, answered) = bench.click(Act::ClickAndAsk(pickup(4))).await;
    bench.report(
        "plugin: a listing, then ask for the statistics",
        &sent,
        &answered,
    );
    let put_back = place(&answered, "the listing put back", |heard| {
        slot_of(heard, sent.window, 4)
    });
    let reply = place(&answered, "the statistics", stats);
    let next = place(&answered, "the confirmation", |heard| {
        opens(heard, "Confirm Purchase")
    });
    assert!(put_back < reply && reply < next, "{answered:?}");
    let (sent, answered) = bench.click(Act::Click(pickup(11))).await;
    bench.report("plugin: confirm the purchase", &sent, &answered);
    place(
        &answered,
        "the bought item in the bot's inventory",
        |heard| matches!(heard, Heard::Slot { window: 0, .. }),
    );
    place(&answered, "the screen closed", |heard| {
        *heard
            == Heard::Close {
                window: sent.window,
            }
    });
    let log = bench.server.log().await;
    for line in log.lines().filter(|line| {
        [
            "harness-click",
            "harness-open",
            "harness-close",
            "harness-bought",
        ]
        .iter()
        .any(|kind| line.contains(kind))
    }) {
        eprintln!("MEASURE {} server: {line}", bench.name);
    }
    let bought = log
        .lines()
        .filter(|line| line.contains("harness-bought"))
        .count();
    assert_eq!(bought, 1, "{log}");
}

// --- How fast clicks may come ---------------------------------------------------------------

/// One burst's outcome.
#[derive(Debug)]
struct Rate {
    per_tick: u32,
    sent: usize,
    /// The clicks the Paper harness logged; 0 on vanilla, which logs none.
    logged: usize,
    resyncs: usize,
    kicked: Option<String>,
}

#[tokio::test]
async fn slow_click_rate_on_vanilla() {
    let bench = Bench::new(Server::start(Mode::Offline).await, "vanilla").await;
    let rates = rates(bench, &[1, 5, 10, 20, 25, 30, 50, 100]).await;

    // No limit: every burst, up to 2,000 clicks a second, went through as
    // predicted.
    assert_eq!(rates.len(), 8, "{rates:#?}");
    for rate in &rates {
        assert_eq!((rate.resyncs, &rate.kicked), (0, &None), "{rate:?}");
    }
}

#[tokio::test]
async fn slow_click_rate_on_paper() {
    let bench = Bench::new(Server::start_paper().await, "paper").await;
    let rates = rates(bench, &[1, 5, 10, 20, 22, 25]).await;

    // Every click below the packet limiter's kick reached the server, in
    // order and as predicted; 25 a tick, 500 a second with the bot's own
    // packets on top, is past the limiter's 500 a second over 7 s.
    let (kicked, below) = rates.split_last().expect("a burst");
    for rate in below {
        assert_eq!(
            (rate.logged, rate.resyncs, &rate.kicked),
            (rate.sent, 0, &None),
            "{rate:?}"
        );
    }
    assert_eq!(kicked.per_tick, 25, "{rates:#?}");
    let reason = kicked.kicked.as_deref().unwrap_or_default();
    assert!(
        reason.contains("disconnect.exceeded_packet_rate"),
        "{reason}"
    );
}

/// Bursts of clicks on the bot's own inventory at each of `per_tick` clicks a
/// game tick, eight seconds each, until the server kicks the bot.
async fn rates(mut bench: Bench, per_tick: &[u32]) -> Vec<Rate> {
    const TICKS: u32 = 160;
    let mut rates = Vec::new();
    for &rate in per_tick {
        let log_before = bench_clicks(&bench).await;
        let (sent_before, heard_before) = {
            let book = book(&bench.shared);
            (book.sent.len(), book.heard.len())
        };
        bench
            .act(Act::Burst {
                per_tick: rate,
                ticks: TICKS,
                one: HOTBAR_1,
                other: INVENTORY_1,
            })
            .await;
        let started = bench.tick();
        let end = started + u64::from(TICKS) + ANSWER;
        let kicked = tokio::select! {
            reason = disconnected(&mut bench.events) => Some(reason),
            () = until(&bench.shared, "the burst's end", |book| book.tick >= end) => None,
        };
        let (sent, resyncs) = {
            let book = book(&bench.shared);
            let resyncs = book.heard[heard_before..]
                .iter()
                .filter(|(_, heard)| matches!(heard, Heard::Content { .. }))
                .count();
            (book.sent.len() - sent_before, resyncs)
        };
        let logged = bench_clicks(&bench).await - log_before;
        eprintln!(
            "MEASURE {} rate {rate} a tick ({} a second): sent {sent}, server logged {logged}, full resyncs {resyncs}, kicked {kicked:?}",
            bench.name,
            rate * 20
        );
        let done = kicked.is_some();
        rates.push(Rate {
            per_tick: rate,
            sent,
            logged,
            resyncs,
            kicked,
        });
        if done {
            break;
        }
        bench
            .until("the stack back in its slot", |book| {
                book.own
                    .get(usize::from(HOTBAR_1))
                    .is_some_and(|(_, count)| *count == 64)
            })
            .await;
    }
    rates
}

/// How many clicks the Paper harness has logged so far; 0 on vanilla, which
/// logs none.
async fn bench_clicks(bench: &Bench) -> usize {
    if bench.name != "paper" {
        return 0;
    }
    bench
        .server
        .log()
        .await
        .lines()
        .filter(|line| line.contains("harness-click"))
        .count()
}
