//! Inventories, a server's screens and its sign editors, as a script sees
//! and acts on them (Plan.md Phase 16; ADR-0036, M1 to M18, R1, R2;
//! ADR-0050).
//!
//! Everything here is pure: fleet-mc reads azalea's state into these types
//! and asks them what to do, and fleet-script shows them to a script.
//!
//! - [`ServerText`] and [`Lore`] are the first free text from a server that
//!   a script reads: an item's name and lore and a screen's title, cleaned
//!   as chat is and bounded (M1).
//! - [`Item`] is what a slot holds: its registry ID, count and text, the
//!   few data components a script reads as IDs and numbers
//!   ([`Enchantments`], a potion's kind; M11), and [`ItemData`], every
//!   component the server sent, which only the click guard compares.
//! - [`Screen`] is the open screen at one moment and [`BotInventory`] the
//!   bot's own slots: every [`Slot`], with its [`Place`] in a grid (M18),
//!   the screen's [`ScreenKind`], title and cursor, and which screen and
//!   state it is ([`ScreenId`], [`StateId`]; M2).
//! - [`MenuAction`] is what a script does in a screen: a [`Click`], a
//!   [`ButtonPress`], a [`TradeChoice`] or a [`Rename`]. Its
//!   [`guard`](MenuAction::guard) lets it out only onto the screen, state
//!   and whole items the hook saw (M3), and [`ClickGate`] sends one at a
//!   time and gives each its [`ClickFate`] (R1).
//! - [`SignTemplate`] is a sign the mode declares, [`SignEditor`] the
//!   server's opening of one, and [`SignDesk`] answers only the one opened
//!   last, once (M16).

mod action;
mod gate;
mod item;
mod screen;
mod sign;
mod text;

pub use action::{
    AnvilName, AnvilNameError, ButtonPress, Click, ClickKind, MenuAction, MenuRefusal, Rename,
    TradeChoice,
};
pub use gate::{ClickFate, ClickGate, ClickResult};
pub use item::{Enchantments, Item, ItemData};
pub use screen::{
    BotInventory, Grid, Place, Screen, ScreenId, ScreenKind, Slot, StateId, TradeOffer, WindowId,
};
pub use sign::{
    SignAnswer, SignDesk, SignDraft, SignEditor, SignLine, SignLineError, SignRefusal, SignSide,
    SignTemplate, SignTemplateError, SignText, SignUpdate,
};
pub use text::{Lore, ServerText};
