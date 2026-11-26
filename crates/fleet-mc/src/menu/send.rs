//! What a script does in a screen or a sign editor, sent on the host
//! thread (Plan.md P16.9, P16.10; ADR-0036, M3, M7, M8, M16, R1;
//! ADR-0050, B5 to B7; ADR-0051).
//!
//! **An action goes out only after [`ClickGate::send`] allowed it at the
//! moment of sending**, against the screen as it is then, with the slots
//! azalea predicts it to change. Then, in the same job: one packet with
//! that prediction and the state the server last gave the window, the
//! statistics request right behind it (R1), and azalea's copy of the
//! inventory as predicted, as a vanilla client shows it.
//!
//! **azalea's prediction is put right in three places** (azalea 0.16.0,
//! read at its release's commit):
//! - a number key swaps with the hotbar slot it names; azalea reads the
//!   key as a slot of the screen;
//! - a right click on a stack leaves the half it didn't take; azalea
//!   leaves the whole stack;
//! - a stack emptied by a drop is empty; azalea leaves it at count 0.
//!
//! [`ClickGate::send`]: fleet_core::script::menu::ClickGate::send

use azalea::core::registry_holder::RegistryHolder;
use azalea::ecs::entity::Entity;
use azalea::ecs::world::World;
use azalea::entity::PlayerAbilities;
use azalea::entity::inventory::Inventory;
use azalea::inventory::operations::{
    ClickOperation, PickupClick, QuickMoveClick, SwapClick, ThrowClick,
};
use azalea::inventory::{ClientsideCloseContainerEvent, ItemStack, Menu};
use azalea::packet::game::SendGamePacketEvent;
use azalea::protocol::packets::Packet as _;
use azalea::protocol::packets::game::s_client_command::{
    Action as ClientCommand, ServerboundClientCommand,
};
use azalea::protocol::packets::game::s_container_click::HashedStack;
use azalea::protocol::packets::game::{
    ServerboundContainerButtonClick, ServerboundContainerClick, ServerboundContainerClose,
    ServerboundGamePacket, ServerboundRenameItem, ServerboundSelectTrade, ServerboundSignUpdate,
};
use azalea::{BlockPos, Client};
use fleet_core::mc::{MenuCommand, MenuError, SessionError};
use fleet_core::mode::GameAction;
use fleet_core::script::menu::{
    ClickKind, MenuAction, MenuRefusal, Screen, ScreenId, ScreenKind, SignAnswer, SignEditor,
    SignSide, StateId,
};
use tracing::debug;

use super::{Menus, Open, read};
use crate::events::MenuReport;

/// Carries `command` out on the host thread, or says why not.
///
/// # Errors
/// As [`SessionHandle::menu`](fleet_core::mc::SessionHandle::menu) says.
pub(crate) fn command(client: &Client, command: MenuCommand) -> Result<(), MenuError> {
    perform(&mut client.ecs.write(), client.entity, command)
}

/// [`command`] for `bot` in `world`.
pub(super) fn perform(
    world: &mut World,
    bot: Entity,
    command: MenuCommand,
) -> Result<(), MenuError> {
    if world.get::<Menus>(bot).is_none() || world.get::<Inventory>(bot).is_none() {
        return Err(SessionError::NotInWorld.into());
    }
    match command {
        MenuCommand::Act(action) => act(world, bot, &action),
        MenuCommand::Close(seen) => close(world, bot, seen),
        MenuCommand::OpenInventory => open_inventory(world, bot),
        MenuCommand::AnswerSign { editor, answer } => answer_sign(world, bot, &editor, answer),
    }
}

/// What an action sends and what azalea's copy becomes.
struct Planned {
    packet: ServerboundGamePacket,
    /// azalea's copy of the inventory as predicted; `None` when nothing
    /// is predicted.
    after: Option<Inventory>,
    /// The slots the prediction changes.
    predicted: Vec<u16>,
}

/// An action onto the open screen: guarded, sent with its barrier, and
/// azalea's copy updated as predicted.
fn act(world: &mut World, bot: Entity, action: &MenuAction) -> Result<(), MenuError> {
    let abilities = world
        .get::<PlayerAbilities>(bot)
        .cloned()
        .unwrap_or_default();
    let (now, planned, at) = read::with_registries(world, bot, |registries| {
        let menus = world.get::<Menus>(bot).ok_or(SessionError::NotInWorld)?;
        let inventory = world
            .get::<Inventory>(bot)
            .ok_or(SessionError::NotInWorld)?;
        // The fate before it is the actor's to read first: no more than
        // one is ever unread (ADR-0051).
        if menus.reports.fate_unread() {
            return Err(MenuRefusal::ClickPending.into());
        }
        let now = read::screen(menus, inventory, registries);
        let planned = match (&now, registries) {
            (Some(screen), Some(registries)) => {
                Some(plan(action, screen, inventory, &abilities, registries)?)
            }
            _ => None,
        };
        Ok::<_, MenuError>((now, planned, menus.now()))
    })?;
    let predicted = planned
        .as_ref()
        .map_or(&[][..], |planned| planned.predicted.as_slice());
    world
        .get_mut::<Menus>(bot)
        .ok_or(SessionError::NotInWorld)?
        .gate
        .send(action, now.as_ref(), predicted, at)?;
    // The gate let it out, so there is a screen and a plan; this keeps the
    // function total without a panic.
    let Some(planned) = planned else {
        return Err(SessionError::NotInWorld.into());
    };
    debug!(kind = action_kind(action), slot = ?action.slot(), "an action onto the screen");
    world.trigger(SendGamePacketEvent::new(bot, planned.packet));
    world.trigger(SendGamePacketEvent::new(bot, barrier()));
    if let Some(after) = planned.after
        && let Some(mut inventory) = world.get_mut::<Inventory>(bot)
    {
        *inventory = after;
    }
    Ok(())
}

/// The statistics request behind every action, which the server answers
/// after everything the action brought (ADR-0036, R1).
fn barrier() -> ServerboundClientCommand {
    ServerboundClientCommand {
        action: ClientCommand::RequestStats,
    }
}

/// The action's kind, for the log.
const fn action_kind(action: &MenuAction) -> &'static str {
    match action {
        MenuAction::Click(click) => click.kind().as_str(),
        MenuAction::Button(_) => "button",
        MenuAction::Trade(_) => "trade",
        MenuAction::Rename(_) => "rename",
    }
}

/// What `action` sends onto `screen`, and for a click, azalea's copy as
/// predicted.
fn plan(
    action: &MenuAction,
    screen: &Screen,
    inventory: &Inventory,
    abilities: &PlayerAbilities,
    registries: &RegistryHolder,
) -> Result<Planned, MenuRefusal> {
    let window = screen.id().window().get();
    Ok(match action {
        MenuAction::Click(click) => {
            let operation = operation(click.kind(), click.slot());
            let after = predict(inventory, click.kind(), click.slot(), abilities);
            let changed = changed(inventory.menu(), after.menu());
            let packet = ServerboundContainerClick {
                container_id: window,
                state_id: screen.state().get(),
                slot_num: i16::try_from(click.slot()).map_err(|_| MenuRefusal::BadValue)?,
                button_num: operation.button_num(),
                click_type: operation.click_type(),
                changed_slots: changed
                    .iter()
                    .map(|(slot, stack)| (*slot, HashedStack::from_item_stack(stack, registries)))
                    .collect(),
                carried_item: HashedStack::from_item_stack(&after.carried, registries),
            };
            Planned {
                packet: packet.into_variant(),
                predicted: changed.into_iter().map(|(slot, _)| slot).collect(),
                after: Some(after),
            }
        }
        MenuAction::Button(press) => Planned {
            packet: ServerboundContainerButtonClick {
                container_id: window,
                button_id: u32::from(press.button()),
            }
            .into_variant(),
            after: None,
            predicted: Vec::new(),
        },
        MenuAction::Trade(choice) => Planned {
            packet: ServerboundSelectTrade {
                item: u32::from(choice.index()),
            }
            .into_variant(),
            after: None,
            predicted: Vec::new(),
        },
        MenuAction::Rename(rename) => Planned {
            packet: ServerboundRenameItem {
                name: rename.name().as_str().to_owned(),
            }
            .into_variant(),
            after: None,
            predicted: Vec::new(),
        },
    })
}

/// azalea's operation for a click of `kind` on `slot`, as a vanilla
/// client sends it.
pub(super) fn operation(kind: ClickKind, slot: u16) -> ClickOperation {
    match kind {
        ClickKind::Left => PickupClick::Left { slot: Some(slot) }.into(),
        ClickKind::Right => PickupClick::Right { slot: Some(slot) }.into(),
        ClickKind::Shift => QuickMoveClick::Left { slot }.into(),
        ClickKind::NumberKey(key) => SwapClick {
            source_slot: slot,
            target_slot: key.get(),
        }
        .into(),
        ClickKind::Drop => ThrowClick::Single { slot }.into(),
        ClickKind::DropStack => ThrowClick::All { slot }.into(),
    }
}

/// azalea's prediction of a click of `kind` on `slot`, on a copy of
/// `inventory`, put right where it's wrong (see the module docs).
pub(super) fn predict(
    inventory: &Inventory,
    kind: ClickKind,
    slot: u16,
    abilities: &PlayerAbilities,
) -> Inventory {
    let mut after = inventory.clone();
    let index = usize::from(slot);
    if let ClickKind::NumberKey(key) = kind {
        let hotbar = after.menu().hotbar_slots_range().start() + usize::from(key.get());
        swap(after.menu_mut(), index, hotbar);
        return after;
    }
    after.simulate_click(&operation(kind, slot), abilities);
    if kind == ClickKind::Right {
        leave_the_other_half(inventory, &mut after, index);
    }
    for at in 0..after.menu().len() {
        if let Some(stack) = after.menu_mut().slot_mut(at) {
            empty_at_zero(stack);
        }
    }
    empty_at_zero(&mut after.carried);
    after
}

/// Swaps two slots of `menu`, as a number key swaps a slot with the hotbar.
fn swap(menu: &mut Menu, one: usize, other: usize) {
    let (Some(first), Some(second)) = (menu.slot(one).cloned(), menu.slot(other).cloned()) else {
        return;
    };
    if let Some(slot) = menu.slot_mut(one) {
        *slot = second;
    }
    if let Some(slot) = menu.slot_mut(other) {
        *slot = first;
    }
}

/// A right click on a stack with an empty cursor takes half of it, and the
/// other half stays; azalea predicts the cursor and leaves the slot whole.
fn leave_the_other_half(before: &Inventory, after: &mut Inventory, index: usize) {
    if before.carried.is_present() || after.menu().slot(index) != before.menu().slot(index) {
        return;
    }
    let (Some(ItemStack::Present(was)), ItemStack::Present(taken)) =
        (before.menu().slot(index), &after.carried)
    else {
        return;
    };
    let left = was.count.saturating_sub(taken.count);
    let remaining = if left > 0 {
        let mut kept = was.clone();
        kept.count = left;
        ItemStack::Present(kept)
    } else {
        ItemStack::Empty
    };
    if let Some(slot) = after.menu_mut().slot_mut(index) {
        *slot = remaining;
    }
}

/// A stack with nothing left in it is empty.
fn empty_at_zero(stack: &mut ItemStack) {
    if stack.as_present().is_some_and(|data| data.count <= 0) {
        *stack = ItemStack::Empty;
    }
}

/// The slots `after` changed from `before`, with what each holds now.
fn changed(before: &Menu, after: &Menu) -> Vec<(u16, ItemStack)> {
    before
        .slots()
        .into_iter()
        .zip(after.slots())
        .enumerate()
        .filter(|(_, (was, now))| was != now)
        .filter_map(|(at, (_, now))| Some((u16::try_from(at).ok()?, now)))
        .collect()
}

/// The bot closes `seen`, as Esc does: guarded by its screen, never
/// answered, and an action pending on it is `replaced`.
fn close(world: &mut World, bot: Entity, seen: ScreenId) -> Result<(), MenuError> {
    {
        let mut menus = world
            .get_mut::<Menus>(bot)
            .ok_or(SessionError::NotInWorld)?;
        menus.gate.close(seen)?;
        menus.screen = None;
        menus.reports.menu(MenuReport::Closed(seen));
    }
    // The close for the window the hook saw (0 for the inventory), then
    // azalea's copy closed as the client closes it.
    world.trigger(SendGamePacketEvent::new(
        bot,
        ServerboundContainerClose {
            container_id: seen.window().get(),
        },
    ));
    world.trigger(ClientsideCloseContainerEvent { entity: bot });
    Ok(())
}

/// The bot opens its own inventory, as E does: nothing goes to the server.
fn open_inventory(world: &mut World, bot: Entity) -> Result<(), MenuError> {
    {
        let mut menus = world
            .get_mut::<Menus>(bot)
            .ok_or(SessionError::NotInWorld)?;
        let id = menus.gate.open_inventory()?;
        menus.desk.ended();
        menus.screen = Some(Open {
            id,
            kind: ScreenKind::Inventory,
            title: String::new(),
            state: StateId::new(0),
            offers: Vec::new(),
        });
        menus.reports.menu(MenuReport::Opened(id));
    }
    // As any screen does (ADR-0036, M8).
    super::let_go_now(world, bot);
    Ok(())
}

/// Answers the sign editor the hook saw, through the desk alone.
fn answer_sign(
    world: &mut World,
    bot: Entity,
    seen: &SignEditor,
    answer: SignAnswer,
) -> Result<(), MenuError> {
    let update = world
        .get_mut::<Menus>(bot)
        .ok_or(SessionError::NotInWorld)?
        .desk
        .answer(seen, answer)?;
    let at = update.at();
    let packet = ServerboundSignUpdate {
        pos: BlockPos::new(at.x, at.y, at.z),
        is_front_text: update.side() == SignSide::Front,
        lines: update.lines().map(str::to_owned),
    };
    debug!(cancelled = update.cancelled(), "a sign answered");
    world.trigger(SendGamePacketEvent::new(bot, packet));
    Ok(())
}
