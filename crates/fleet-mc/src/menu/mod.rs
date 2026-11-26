//! A scripted mode's screens, clicks and sign editors (Plan.md P16.8 to
//! P16.10; ADR-0036, M2, M3, M7, M8, M16, R1; ADR-0050, ADR-0051).
//!
//! fleet-core decides (`script::menu`): [`ClickGate`] lets one action out
//! at a time and gives each its fate, an action's guard lets it out only
//! onto the screen, state and whole items its hook saw, and [`SignDesk`]
//! answers only the editor the server opened last, once. This module feeds
//! them what the server sends and asks them before it sends anything, on
//! the session's host thread.
//!
//! - **What the server sends** ([`read_packets`], in `Update`, after
//!   azalea's own handlers ran in `PreUpdate`): a screen opened, its whole
//!   content, a slot, the cursor, a merchant's trades, a close, the
//!   barrier's reply, a sign editor, a world change; a death from azalea's
//!   `DeathEvent`. **Each window keeps its own state ID** (P16.8), from
//!   the content and slot packets a vanilla client applies to it: azalea
//!   keeps one for every window, set from any window's slot packet and
//!   never from a whole content.
//! - **What azalea leaves out is put right** on its `Inventory`, so the
//!   snapshot and the prediction start from what a vanilla client shows:
//!   the cursor that a whole content or `SetCursorItem` carries, a
//!   `SetPlayerInventory`, and a hotbar slot of window 0 in the open
//!   screen's copy of the bot's slots.
//! - **Snapshots** (`read`): the screen and the inventory as fleet-core's
//!   types.
//! - **Sending** (`send`): an action only after the gate allowed it at the
//!   moment of sending, one packet with azalea's prediction (two of its
//!   gaps put right) and the statistics request right behind it; a close;
//!   a sign's answer only through the desk.
//! - **Keys and buttons** (M8): a screen opening lets go of all of them,
//!   and none goes down while it's open ([`screen_open`]). A sign editor
//!   holds none back: the server never sees a client's sign screen.
//! - **Unanswered** ([`expire`], every game tick): an action whose
//!   barrier had no reply in 5 s.
//!
//! The events reach the actor through the session's bridge, bounded as
//! ADR-0051 says.

mod read;
mod send;
#[cfg(test)]
mod tests;

use std::time::Instant;

use azalea::app::{App, Plugin, Update};
use azalea::core::tick::GameTick;
// The `Component` derive names its crate as `bevy_ecs`, which fleet-mc only
// has through azalea.
use azalea::ecs as bevy_ecs;
use azalea::ecs::component::Component;
use azalea::ecs::entity::Entity;
use azalea::ecs::lifecycle::Add;
use azalea::ecs::message::MessageReader;
use azalea::ecs::observer::On;
use azalea::ecs::query::With;
use azalea::ecs::schedule::IntoScheduleConfigs as _;
use azalea::ecs::system::{Commands, Query};
use azalea::ecs::world::World;
use azalea::entity::LocalEntity;
use azalea::entity::inventory::Inventory;
use azalea::inventory::{ClientsideCloseContainerEvent, ItemStack, Player};
use azalea::packet::game::{DeathEvent, ReceiveGamePacketEvent};
use azalea::protocol::packets::game::{
    ClientboundContainerSetContent, ClientboundContainerSetSlot, ClientboundGamePacket,
    ClientboundSetPlayerInventory,
};
use chrono::{DateTime, Utc};
use fleet_core::mode::GameAction;
use fleet_core::script::menu::{
    ClickGate, ScreenId, ScreenKind, SignDesk, SignSide, StateId, TradeOffer, WindowId,
};
use fleet_core::script::{Input, MouseButton};
use tracing::debug;

pub(crate) use self::read::view;
pub(crate) use self::send::command;
use crate::events::{BridgeControl, MenuReport, render_within};

/// On the bot: its screens, its clicks and its sign editor.
#[derive(Component, Debug)]
pub(crate) struct Menus {
    /// One action at a time, and each its fate.
    gate: ClickGate,
    /// Only the editor the server opened last is answered, once.
    desk: SignDesk,
    /// The state the server last gave window 0, the bot's own inventory.
    inventory_state: StateId,
    /// What fleet-mc keeps of the open screen beside the gate.
    screen: Option<Open>,
    /// What an action's time is told from: a monotonic clock, so a
    /// clock set back never makes an action unanswered early or late.
    epoch: Instant,
    /// The session's bridge, for the events.
    reports: BridgeControl,
}

/// The screen that's open, as fleet-mc keeps it.
#[derive(Debug, Clone)]
struct Open {
    id: ScreenId,
    kind: ScreenKind,
    /// The title as the server gave it, rendered within its budget;
    /// fleet-core cleans it when a snapshot is taken.
    title: String,
    /// The state the server last gave its window. The inventory's is
    /// [`Menus::inventory_state`].
    state: StateId,
    /// A merchant's trades, as it last sent them.
    offers: Vec<TradeOffer>,
}

impl Menus {
    /// Whether a screen is open: no key or button goes down then
    /// (ADR-0036, M8).
    pub(crate) const fn screen_is_open(&self) -> bool {
        self.screen.is_some()
    }

    fn new(reports: BridgeControl) -> Self {
        Self {
            gate: ClickGate::new(),
            desk: SignDesk::new(),
            inventory_state: StateId::new(0),
            screen: None,
            epoch: Instant::now(),
            reports,
        }
    }

    /// Now, on the clock the gate's waits are measured by.
    fn now(&self) -> DateTime<Utc> {
        fleet_core::time::add(DateTime::<Utc>::UNIX_EPOCH, self.epoch.elapsed())
    }

    /// The open screen's window, if it's one the server opened.
    fn container(&self) -> Option<WindowId> {
        self.screen
            .as_ref()
            .filter(|open| open.kind != ScreenKind::Inventory)
            .map(|open| open.id.window())
    }

    /// Whether the open screen is the bot's own inventory.
    fn inventory_open(&self) -> bool {
        self.screen
            .as_ref()
            .is_some_and(|open| open.kind == ScreenKind::Inventory)
    }

    /// Reports that the open screen's content changed.
    fn updated(&self) {
        if let Some(open) = &self.screen {
            self.reports.menu(MenuReport::Updated(open.id));
        }
    }

    /// A screen opened in `window`: a new screen, whatever was open, and a
    /// sign editor ends with it (ADR-0050, B7). The screen it replaces is
    /// reported closed.
    fn opened(&mut self, window: WindowId, kind: ScreenKind, title: String) -> ScreenId {
        self.desk.ended();
        if let Some(old) = self.screen.take() {
            self.reports.menu(MenuReport::Closed(old.id));
        }
        let id = self.gate.opened(window);
        self.screen = Some(Open {
            id,
            kind,
            title,
            state: StateId::new(0),
            offers: Vec::new(),
        });
        self.reports.menu(MenuReport::Opened(id));
        id
    }

    /// The open screen is gone: the server closed it, a sign editor took
    /// its place, or the bot died or changed worlds. Returns whether one
    /// was open.
    fn gone(&mut self) -> bool {
        self.gate.closed();
        match self.screen.take() {
            Some(old) => {
                self.reports.menu(MenuReport::Closed(old.id));
                true
            }
            None => false,
        }
    }

    /// A whole content: azalea put the items in; the state and the cursor
    /// are fleet-mc's.
    fn content(&mut self, inventory: &mut Inventory, content: &ClientboundContainerSetContent) {
        let window = WindowId::new(content.container_id);
        let state = StateId::new(content.state_id);
        if window == WindowId::INVENTORY {
            self.inventory_state = state;
            // A vanilla client's window 0 carries its own cursor; it's the
            // one shown while no server screen is open.
            if self.container().is_none() {
                inventory.carried = content.carried_item.clone();
            }
        } else if self.container() == Some(window)
            && let Some(open) = &mut self.screen
        {
            open.state = state;
            inventory.carried = content.carried_item.clone();
        } else {
            return;
        }
        self.gate.content(window);
        if self.shows(window) {
            self.updated();
        }
    }

    /// Whether the open screen shows `window`'s content.
    fn shows(&self, window: WindowId) -> bool {
        if window == WindowId::INVENTORY {
            self.inventory_open()
        } else {
            self.container() == Some(window)
        }
    }

    /// A slot packet, as a vanilla client applies it: the cursor, a hotbar
    /// slot of window 0 at any time, another slot of window 0 only while
    /// no server screen is open, and a slot of the open screen.
    fn slot(&mut self, inventory: &mut Inventory, packet: &ClientboundContainerSetSlot) {
        let window = WindowId::new(packet.container_id);
        let state = StateId::new(packet.state_id);
        let index = usize::from(packet.slot);
        match packet.container_id {
            // The cursor, which azalea sets itself.
            -1 => {
                self.gate.cursor();
                self.updated();
            }
            0 => {
                let hotbar = Player::is_hotbar_slot(index);
                if self.container().is_some() && !hotbar {
                    return;
                }
                self.inventory_state = state;
                if hotbar {
                    // A vanilla client's screens show the bot's own slots,
                    // not a copy of them: the open screen's hotbar is window
                    // 0's.
                    mirror_hotbar(inventory, index, &packet.item_stack);
                }
                self.gate.slot(window, packet.slot);
                if self.screen.is_some() {
                    self.updated();
                }
            }
            _ if self.container() == Some(window) => {
                if let Some(open) = &mut self.screen {
                    open.state = state;
                }
                self.gate.slot(window, packet.slot);
                self.updated();
            }
            _ => {}
        }
    }

    /// A slot of the bot's own inventory, which azalea leaves out: it's
    /// put into window 0 and into the open screen's copy of the bot's
    /// slots, as a vanilla client shows it in both.
    fn player_slot(&mut self, inventory: &mut Inventory, packet: &ClientboundSetPlayerInventory) {
        let Ok(slot) = usize::try_from(packet.slot) else {
            return;
        };
        let Some(own) = inventory_index(slot) else {
            return;
        };
        if let Some(stack) = inventory.inventory_menu.slot_mut(own) {
            *stack = packet.contents.clone();
        }
        let in_screen = if own == Player::OFFHAND_SLOT || own < *Player::INVENTORY_SLOTS.start() {
            None
        } else {
            inventory.container_menu.as_mut().and_then(|menu| {
                let at =
                    menu.player_slots_range().start() + (own - Player::INVENTORY_SLOTS.start());
                menu.slot_mut(at).map(|stack| (at, stack))
            })
        };
        self.gate
            .slot(WindowId::INVENTORY, u16::try_from(own).unwrap_or(u16::MAX));
        if let Some((at, stack)) = in_screen {
            *stack = packet.contents.clone();
            if let Some(window) = self.container() {
                self.gate
                    .slot(window, u16::try_from(at).unwrap_or(u16::MAX));
            }
        }
        if self.screen.is_some() {
            self.updated();
        }
    }
}

/// Where a slot of the bot's inventory, as a `SetPlayerInventory` numbers
/// it (the hotbar 0 to 8, the rest 9 to 35, the armour 36 to 39 from the
/// feet up, the offhand 40), is in window 0.
fn inventory_index(slot: usize) -> Option<usize> {
    match slot {
        0..=8 => Some(Player::HOTBAR_SLOTS.start() + slot),
        9..=35 => Some(slot),
        36..=39 => Some(Player::ARMOR_SLOTS.end() - (slot - 36)),
        40 => Some(Player::OFFHAND_SLOT),
        _ => None,
    }
}

/// Puts a hotbar slot of window 0, numbered as window 0 numbers it, into
/// the open screen's copy of the bot's hotbar.
fn mirror_hotbar(inventory: &mut Inventory, index: usize, stack: &ItemStack) {
    let Some(menu) = inventory.container_menu.as_mut() else {
        return;
    };
    let at = menu.hotbar_slots_range().start() + (index - Player::HOTBAR_SLOTS.start());
    if let Some(slot) = menu.slot_mut(at) {
        *slot = stack.clone();
    }
}

/// Keeps the bot's screens, its clicks and its sign editor; see the
/// [module docs](self).
pub(crate) struct MenuPlugin(pub(crate) BridgeControl);

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        let reports = self.0.clone();
        // azalea registers both; registering them again is a no-op, and it
        // lets the plugin work on its own.
        app.add_message::<ReceiveGamePacketEvent>()
            .add_message::<DeathEvent>()
            .add_observer(
                move |spawned: On<Add, LocalEntity>, mut commands: Commands| {
                    commands
                        .entity(spawned.entity)
                        .insert(Menus::new(reports.clone()));
                },
            )
            // A death of the frame first, then its packets in order: both
            // end a screen, and a screen the server opens after a death
            // stays open. Both before the one azalea system of the frame
            // that reads the inventory, so it reads it as put right.
            .add_systems(
                Update,
                (
                    deaths.after(azalea::packet::death_event_on_0_health),
                    read_packets,
                )
                    .chain()
                    .before(azalea::pathfinder::path_found_listener),
            )
            .add_systems(GameTick, expire);
    }
}

/// Reads the frame's packets for the bot's screens, in the order they
/// came, after azalea's own handlers.
fn read_packets(
    mut commands: Commands,
    mut packets: MessageReader<ReceiveGamePacketEvent>,
    mut bots: Query<(&mut Menus, &mut Inventory), With<LocalEntity>>,
) {
    for received in packets.read() {
        let bot = received.entity;
        let Ok((mut menus, mut inventory)) = bots.get_mut(bot) else {
            continue;
        };
        match &*received.packet {
            ClientboundGamePacket::OpenScreen(open) => {
                let kind = read::screen_kind(open.menu_type);
                let title = render_within(&open.title, read::TITLE_CHARS).text;
                menus.opened(WindowId::new(open.container_id), kind, title);
                let_go(&mut commands, bot);
            }
            ClientboundGamePacket::ContainerSetContent(content) => {
                menus.content(&mut inventory, content);
            }
            ClientboundGamePacket::ContainerSetSlot(slot) => menus.slot(&mut inventory, slot),
            ClientboundGamePacket::SetCursorItem(cursor) => {
                inventory.carried = cursor.contents.clone();
                menus.gate.cursor();
                menus.updated();
            }
            ClientboundGamePacket::SetPlayerInventory(slot) => {
                menus.player_slot(&mut inventory, slot);
            }
            ClientboundGamePacket::MerchantOffers(offers) => {
                if menus.container() == Some(WindowId::new(offers.container_id)) {
                    let read: Vec<_> = offers
                        .offers
                        .iter()
                        .filter_map(|offer| read::offer(offer, None))
                        .collect();
                    if let Some(open) = &mut menus.screen {
                        open.offers = read;
                    }
                    menus.updated();
                }
            }
            // azalea closes its own copy, whatever the window.
            ClientboundGamePacket::ContainerClose(_) => {
                menus.gone();
            }
            ClientboundGamePacket::OpenSignEditor(sign) => {
                // A vanilla client's sign screen takes the screen's place
                // (ADR-0050, B7): closed on the client, with nothing sent.
                if menus.gone() {
                    commands.trigger(ClientsideCloseContainerEvent { entity: bot });
                }
                let side = if sign.is_front_text {
                    SignSide::Front
                } else {
                    SignSide::Back
                };
                let editor = menus.desk.opened(crate::script::block(sign.pos), side);
                menus.reports.menu(MenuReport::SignOpened(editor));
            }
            ClientboundGamePacket::AwardStats(_) => {
                if let Some(result) = menus.gate.barrier_answered() {
                    debug!(fate = %result.fate, slot = ?result.slot, "a click's barrier was answered");
                    menus.reports.menu(MenuReport::Clicked(result));
                }
            }
            ClientboundGamePacket::Login(_) | ClientboundGamePacket::Respawn(_) => {
                ended(&mut menus, &mut commands, bot);
            }
            _ => {}
        }
    }
}

/// The bot died: its screen and its sign editor end (ADR-0050, B7).
fn deaths(
    mut commands: Commands,
    mut died: MessageReader<DeathEvent>,
    mut bots: Query<&mut Menus, With<LocalEntity>>,
) {
    for death in died.read() {
        if let Ok(mut menus) = bots.get_mut(death.entity) {
            ended(&mut menus, &mut commands, death.entity);
        }
    }
}

/// A death or a world change: the screen and the editor end, and azalea's
/// copy of the screen with them.
fn ended(menus: &mut Menus, commands: &mut Commands, bot: Entity) {
    if menus.gone() {
        commands.trigger(ClientsideCloseContainerEvent { entity: bot });
    }
    menus.desk.ended();
}

/// Lets go of every key and button, as a vanilla client does when a screen
/// opens (ADR-0036, M8), in the same frame.
fn let_go(commands: &mut Commands, bot: Entity) {
    commands.queue(move |world: &mut World| let_go_now(world, bot));
}

/// Lets go of every key and button of `bot`. Only a right button held
/// down sends anything: the release of its use.
pub(super) fn let_go_now(world: &mut World, bot: Entity) {
    let using = world
        .get::<crate::script::Held>(bot)
        .is_some_and(|held| held.right.is_some());
    let mut actions = vec![
        GameAction::ReleaseKeys,
        GameAction::Release(Input::Mouse(MouseButton::Left)),
    ];
    if using {
        actions.push(GameAction::Release(Input::Mouse(MouseButton::Right)));
    }
    for action in actions {
        // A bot azalea no longer has holds nothing.
        let _ = crate::script::act(world, bot, action);
    }
    if let Ok(mut entity) = world.get_entity_mut(bot) {
        entity.remove::<crate::script::GettingOut>();
    }
}

/// An action whose barrier had no reply in time is unanswered.
fn expire(mut bots: Query<&mut Menus, With<LocalEntity>>) {
    for mut menus in &mut bots {
        if !menus.gate.is_pending() {
            continue;
        }
        let now = menus.now();
        if let Some(result) = menus.gate.expire(now) {
            debug!(slot = ?result.slot, "a click had no answer in time");
            menus.reports.menu(MenuReport::Clicked(result));
        }
    }
}

/// Whether a screen is open for `bot`: no key or button goes down then
/// (ADR-0036, M8).
pub(crate) fn screen_open(world: &World, bot: Entity) -> bool {
    world.get::<Menus>(bot).is_some_and(Menus::screen_is_open)
}
