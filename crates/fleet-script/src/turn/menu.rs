//! What a hook does in a screen and a sign editor: clicks, a screen's
//! buttons, trades, an anvil's name, closing, the inventory, signs
//! (Plan.md P16.12; ADR-0036, M2 to M18, R1, R2; ADR-0050, ADR-0052).
//!
//! **Every rule here is the hook's side.** A hook judges against the
//! screen it was given, and what it's given `ok` for goes out after it
//! returns, where the session judges it again at the moment of sending: the
//! guard, the gate and the sign desk of fleet-core (ADR-0050). What the
//! session refuses there, and every action's fate, the script hears in
//! `on_click`.

use fleet_core::mc::{MenuCommand, MenuView};
use fleet_core::mode::HotbarSlot;
use fleet_core::script::menu::{
    AnvilName, ButtonPress, Click, ClickKind, Item, MenuAction, Rename, Screen, SignAnswer,
    TradeChoice,
};
use fleet_core::script::{Limited, Refusal, limits};

use super::{Answer, Turn, Value, given};
use crate::action::{Action, Asked};
use crate::error::Misuse;

/// An item as a script reads it: what a slot's map holds, without where it
/// is. Two items a script can't tell apart read the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemView {
    pub(crate) id: String,
    pub(crate) count: u32,
    pub(crate) name: String,
    pub(crate) lore: Vec<String>,
    pub(crate) durability: Option<u32>,
    pub(crate) potion: Option<String>,
    pub(crate) enchantments: Vec<(String, u8)>,
    pub(crate) stored_enchantments: Vec<(String, u8)>,
}

/// The lore lines a map of `item` keeps: whole lines, in order, while the
/// map's text stays within what Rhai lets one value hold. Rhai counts a
/// value's text over everything nested in it (`limits::MAX_STRING_BYTES`),
/// and a server chooses lore of up to 16 lines of 128 characters, so a
/// slot's map with all of it could stop the script that reads it. The lore
/// helpers (`menu.lore_has`, `menu.lore_number`) read every line, from what
/// the hook sees (ADR-0052).
pub(crate) fn kept_lore(item: &Item) -> Vec<String> {
    let taken = item.id().as_str().len()
        + item.name().as_str().len()
        + item.potion().map_or(0, |potion| potion.as_str().len());
    let mut room = limits::MAX_STRING_BYTES.saturating_sub(taken);
    item.lore()
        .lines()
        .iter()
        .map(fleet_core::script::menu::ServerText::as_str)
        .take_while(|line| {
            let fits = line.len() <= room;
            room = room.saturating_sub(line.len());
            fits
        })
        .map(str::to_owned)
        .collect()
}

impl ItemView {
    /// `item` as a script reads it.
    pub(crate) fn of(item: &Item) -> Self {
        let levels = |enchantments: &fleet_core::script::menu::Enchantments| {
            enchantments
                .iter()
                .map(|(id, level)| (id.as_str().to_owned(), level))
                .collect()
        };
        Self {
            id: item.id().as_str().to_owned(),
            count: item.count().get(),
            name: item.name().as_str().to_owned(),
            lore: kept_lore(item),
            durability: item.durability(),
            potion: item.potion().map(|potion| potion.as_str().to_owned()),
            enchantments: levels(item.enchantments()),
            stored_enchantments: levels(item.stored_enchantments()),
        }
    }
}

/// What a script says it clicks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A slot's number in the screen.
    Index(u16),
    /// A slot as a hook read it (`menu.slot(12)`, an element of
    /// `menu.slots`): its number, the screen it was read on, and what it
    /// held. The click goes out only if this hook sees the same there.
    Read {
        index: u16,
        /// The screen's opening, `None` for a slot of no screen.
        screen: Option<u64>,
        /// What it held; `None` when it was empty.
        item: Option<ItemView>,
    },
    /// Neither: no slot.
    Bad,
}

impl Target {
    /// The slot's number; `None` for no slot.
    pub(crate) const fn index(&self) -> Option<u16> {
        match self {
            Self::Index(index) | Self::Read { index, .. } => Some(*index),
            Self::Bad => None,
        }
    }

    /// Whether it's a slot a hook read somewhere else than `opening` (a
    /// screen's opening, or `None` for the inventory's own slots).
    pub(crate) fn read_elsewhere(&self, opening: Option<u64>) -> bool {
        matches!(self, Self::Read { screen, .. } if *screen != opening)
    }
}

impl Turn {
    /// The screen this hook sees as open: none once it closed it.
    pub(crate) fn screen(&self) -> Option<&Screen> {
        if self.hook.closed {
            return None;
        }
        self.view().and_then(|view| view.screen.as_ref())
    }

    /// What the hook was given of the inventory, the screen and the sign
    /// editor; `None` outside a hook.
    pub(crate) const fn view(&self) -> Option<&MenuView> {
        self.menu.as_ref()
    }

    /// Whether a screen is open for this hook: one it was given and didn't
    /// close, or the inventory it opened. Keys, buttons, looks, the hotbar
    /// and vehicles wait meanwhile, as a vanilla player's do (M8).
    pub(crate) fn screen_open(&self) -> bool {
        self.hook.opened || self.screen().is_some()
    }

    /// Whether an action the script sent still waits for its fate.
    pub(crate) const fn awaits_fate(&self) -> bool {
        self.lasting.awaiting
    }

    /// An action's fate came, or it wasn't sent after all: the next may go.
    pub(crate) const fn heard_fate(&mut self) {
        self.lasting.awaiting = false;
    }

    /// The screen to act on now: the bot can act, and a screen is open.
    fn ready(&self) -> Result<&Screen, Refusal> {
        self.can_act()?;
        self.screen().ok_or(Refusal::NoMenu)
    }

    /// Sends `action` past the click limit, once no action of the script's
    /// waits for its fate: it waits for its own from now. What's wrong with
    /// the action itself is said before this (`bad_value`).
    fn act(&mut self, action: MenuAction) -> Answer {
        if self.lasting.awaiting {
            return Err(Refusal::ClickPending);
        }
        self.limit(Limited::Click)?;
        self.actions.push(Action::Menu(MenuCommand::Act(action)));
        self.lasting.awaiting = true;
        Ok(())
    }

    /// Clicks `target` as `kind` says.
    pub(crate) fn click(&mut self, target: &Target, kind: Option<ClickKind>) -> Answer {
        let answer = self.try_click(target, kind);
        self.noted(Asked::Click, answer)
    }

    /// [`click`](Self::click), before its refusal is noted.
    fn try_click(&mut self, target: &Target, kind: Option<ClickKind>) -> Answer {
        let (Some(index), Some(kind)) = (target.index(), kind) else {
            return Err(Refusal::BadValue);
        };
        let screen = self.ready()?;
        let click = Click::on(screen, kind, index).map_err(Refusal::from)?;
        if let Target::Read {
            screen: read_on,
            item,
            ..
        } = target
        {
            if *read_on != Some(screen.id().opening()) {
                return Err(Refusal::ScreenChanged);
            }
            let now = screen.slot(index).and_then(|slot| slot.item());
            if now.map(ItemView::of) != *item {
                return Err(Refusal::SlotChanged);
            }
        }
        self.act(MenuAction::Click(click))
    }

    /// Presses the screen's button `button`; `None` for a number that is
    /// no button's.
    pub(crate) fn press_button(&mut self, button: Option<u8>) -> Answer {
        let answer = self.try_press_button(button);
        self.noted(Asked::Click, answer)
    }

    fn try_press_button(&mut self, button: Option<u8>) -> Answer {
        let button = button.ok_or(Refusal::BadValue)?;
        let screen = self.ready()?;
        let press = ButtonPress::on(screen, button).map_err(Refusal::from)?;
        self.act(MenuAction::Button(press))
    }

    /// Selects a merchant's trade `index`, from 0; `None` for a number that
    /// is no trade's.
    pub(crate) fn select_trade(&mut self, index: Option<u8>) -> Answer {
        let answer = self.try_select_trade(index);
        self.noted(Asked::Click, answer)
    }

    fn try_select_trade(&mut self, index: Option<u8>) -> Answer {
        let index = index.ok_or(Refusal::BadValue)?;
        let screen = self.ready()?;
        let choice = TradeChoice::on(screen, index).map_err(Refusal::from)?;
        self.act(MenuAction::Trade(choice))
    }

    /// Sets the anvil's name to the declared message `key` with `values`.
    ///
    /// # Errors
    /// [`Misuse::UnknownMessage`] for a key the mode doesn't declare.
    pub(crate) fn rename(
        &mut self,
        key: Option<&str>,
        values: Option<&[(String, Value)]>,
    ) -> Result<Answer, Misuse> {
        let answer = self.try_rename(key, values)?;
        Ok(self.noted(Asked::Click, answer))
    }

    fn try_rename(
        &mut self,
        key: Option<&str>,
        values: Option<&[(String, Value)]>,
    ) -> Result<Answer, Misuse> {
        let program = std::sync::Arc::clone(&self.program);
        let template = key
            .and_then(|key| program.script().message(key))
            .ok_or(Misuse::UnknownMessage)?;
        let name = given(values)
            .and_then(|given| template.render(&given).ok())
            .and_then(|message| AnvilName::try_from(&message).ok());
        let Some(name) = name else {
            return Ok(Err(Refusal::BadValue));
        };
        let screen = match self.ready() {
            Ok(screen) => screen,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let rename = match Rename::on(screen, name) {
            Ok(rename) => rename,
            Err(refusal) => return Ok(Err(Refusal::from(refusal))),
        };
        Ok(self.act(MenuAction::Rename(rename)))
    }

    /// Closes the screen this hook sees, as Esc does. Never limited, and
    /// never refused while a screen is open: like a release, it only lets go
    /// (ADR-0050, B6). An action of the script's that waits for its fate on
    /// it comes back `replaced`.
    pub(crate) fn close_screen(&mut self) -> Answer {
        let answer = self.try_close_screen();
        self.noted(Asked::Screen, answer)
    }

    fn try_close_screen(&mut self) -> Answer {
        let screen = self.screen().ok_or(Refusal::NoMenu)?.id();
        self.actions.push(Action::Menu(MenuCommand::Close(screen)));
        self.hook.closed = true;
        Ok(())
    }

    /// Opens the bot's own inventory, as E does: only with no screen open.
    /// Nothing goes to the server.
    pub(crate) fn open_inventory(&mut self) -> Answer {
        let answer = self.try_open_inventory();
        self.noted(Asked::Screen, answer)
    }

    fn try_open_inventory(&mut self) -> Answer {
        self.can_act()?;
        if self.screen_open() {
            return Err(Refusal::ScreenOpen);
        }
        self.actions.push(Action::Menu(MenuCommand::OpenInventory));
        self.hook.opened = true;
        Ok(())
    }

    /// Answers the sign editor this hook sees with the declared sign `key`
    /// and `values` (ADR-0036, M16).
    ///
    /// # Errors
    /// [`Misuse::UnknownSign`] for a key the mode doesn't declare.
    pub(crate) fn write_sign(
        &mut self,
        key: Option<&str>,
        values: Option<&[(String, Value)]>,
    ) -> Result<Answer, Misuse> {
        let program = std::sync::Arc::clone(&self.program);
        let template = key
            .and_then(|key| program.script().sign(key))
            .ok_or(Misuse::UnknownSign)?;
        let text = given(values).and_then(|given| template.render(&given).ok());
        let answer = match text {
            Some(text) => self.answer_sign(SignAnswer::Write(text)),
            None => Err(Refusal::BadValue),
        };
        Ok(self.noted(Asked::Sign, answer))
    }

    /// Answers the sign editor this hook sees with nothing written.
    pub(crate) fn cancel_sign(&mut self) -> Answer {
        let answer = self.answer_sign(SignAnswer::Cancel);
        self.noted(Asked::Sign, answer)
    }

    /// Answers the editor this hook sees with `answer`, once.
    fn answer_sign(&mut self, answer: SignAnswer) -> Answer {
        self.can_act()?;
        let editor = self
            .view()
            .and_then(|view| view.sign)
            .ok_or(Refusal::NoSign)?;
        if self.hook.sign_answered {
            return Err(Refusal::SignAnswered);
        }
        self.limit(Limited::Click)?;
        self.actions
            .push(Action::Menu(MenuCommand::AnswerSign { editor, answer }));
        self.hook.sign_answered = true;
        Ok(())
    }
}

/// A number key, 1 to 9, as the hotbar slot it swaps with.
pub(crate) fn number_key(key: i64) -> Option<ClickKind> {
    let slot = u8::try_from(key.checked_sub(1)?).ok()?;
    HotbarSlot::try_from(slot).ok().map(ClickKind::NumberKey)
}
