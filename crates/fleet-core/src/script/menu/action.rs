//! [`MenuAction`]: what a script does in a screen, and the guard that lets
//! it go out only onto the screen, the state and the items the script saw
//! (ADR-0036, M3, M7; P16.6).

use core::fmt;

use super::item::Item;
use super::screen::{Screen, ScreenId, ScreenKind, StateId, TradeOffer};
use crate::chat::ChatMessage;
use crate::mode::HotbarSlot;
use crate::text;

/// How a slot is clicked, as a vanilla client clicks it (ADR-0036, D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClickKind {
    /// The left button: picks a stack up, puts it down or swaps it with
    /// the cursor's.
    Left,
    /// The right button: picks half a stack up or puts one item down.
    Right,
    /// Shift and the left button: moves the stack to the other part of the
    /// screen.
    Shift,
    /// A number key over the slot: swaps it with that hotbar slot.
    NumberKey(HotbarSlot),
    /// Q over the slot: drops one item.
    Drop,
    /// Ctrl+Q over the slot: drops the stack.
    DropStack,
}

impl ClickKind {
    /// The kind as a log line names it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Shift => "shift",
            Self::NumberKey(_) => "number_key",
            Self::Drop => "drop",
            Self::DropStack => "drop_stack",
        }
    }
}

/// Why something a script did in a screen wasn't sent: `result.reason`,
/// from a closed list (ADR-0036, M3, M4, M8, R1).
///
/// Nothing is sent when one of these comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuRefusal {
    /// No screen is open.
    NoMenu,
    /// Another screen is open than the one the hook saw.
    ScreenChanged,
    /// The screen's content changed since the hook saw it: its state, its
    /// cursor, or for a trade, the trade.
    ContentChanged,
    /// A slot the click touches holds something else now: anything about
    /// the whole item.
    SlotChanged,
    /// An earlier click has no fate yet (R1).
    ClickPending,
    /// The screen's last click stayed unanswered: it takes no click until
    /// it's opened again (R1).
    Unanswered,
    /// A screen is open, and this needs none: a key, a mouse button, or
    /// opening the inventory (M8).
    ScreenOpen,
    /// A slot the screen doesn't have, a button or a trade it doesn't
    /// offer, or a screen of the wrong kind for what was asked.
    BadValue,
}

impl MenuRefusal {
    /// Every reason, in a fixed order.
    pub const ALL: [Self; 8] = [
        Self::NoMenu,
        Self::ScreenChanged,
        Self::ContentChanged,
        Self::SlotChanged,
        Self::ClickPending,
        Self::Unanswered,
        Self::ScreenOpen,
        Self::BadValue,
    ];

    /// The reason as a script reads it in `result.reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoMenu => "no_menu",
            Self::ScreenChanged => "screen_changed",
            Self::ContentChanged => "content_changed",
            Self::SlotChanged => "slot_changed",
            Self::ClickPending => "click_pending",
            Self::Unanswered => "unanswered",
            Self::ScreenOpen => "screen_open",
            Self::BadValue => "bad_value",
        }
    }
}

impl fmt::Display for MenuRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a hook saw of the screen it acts on: which screen, its state and
/// its cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    screen: ScreenId,
    state: StateId,
    cursor: Option<Item>,
}

impl Seen {
    fn of(screen: &Screen) -> Self {
        Self {
            screen: screen.id(),
            state: screen.state(),
            cursor: screen.cursor().cloned(),
        }
    }

    /// The guard's first three checks: a screen is open, it's the same
    /// screen, and its content is as it was.
    fn check<'a>(&self, now: Option<&'a Screen>) -> Result<&'a Screen, MenuRefusal> {
        let now = now.ok_or(MenuRefusal::NoMenu)?;
        if now.id() != self.screen {
            return Err(MenuRefusal::ScreenChanged);
        }
        if now.state() != self.state || now.cursor() != self.cursor.as_ref() {
            return Err(MenuRefusal::ContentChanged);
        }
        Ok(now)
    }
}

/// What a click touching `slot` of `screen` saw there; `None` when the
/// screen has no such slot.
fn seen_in(screen: &Screen, slot: u16) -> Option<(u16, Option<Item>)> {
    let seen = screen.slot(slot)?;
    Some((slot, seen.item().cloned()))
}

/// A click on a slot, with what the hook saw (ADR-0036, M3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Click {
    seen: Seen,
    kind: ClickKind,
    slot: u16,
    /// Each slot the click touches, with what the hook saw in it.
    touched: Vec<(u16, Option<Item>)>,
}

impl Click {
    /// A click of kind `kind` on slot `slot` of `screen`, as the hook sees
    /// it.
    ///
    /// # Errors
    /// [`MenuRefusal::BadValue`] if the screen has no such slot.
    pub fn on(screen: &Screen, kind: ClickKind, slot: u16) -> Result<Self, MenuRefusal> {
        let mut touched = vec![seen_in(screen, slot).ok_or(MenuRefusal::BadValue)?];
        if let ClickKind::NumberKey(key) = kind {
            let hotbar = screen.hotbar(key);
            // A number key over the hotbar slot it names swaps nothing
            // more: that slot is touched once.
            if hotbar != slot {
                touched.push(seen_in(screen, hotbar).ok_or(MenuRefusal::BadValue)?);
            }
        }
        Ok(Self {
            seen: Seen::of(screen),
            kind,
            slot,
            touched,
        })
    }

    /// How the slot is clicked.
    #[must_use]
    pub const fn kind(&self) -> ClickKind {
        self.kind
    }

    /// The slot clicked.
    #[must_use]
    pub const fn slot(&self) -> u16 {
        self.slot
    }

    /// The slots the click touches: the one clicked, and for a number key
    /// also the hotbar slot it swaps with.
    pub fn touched(&self) -> impl Iterator<Item = u16> {
        self.touched.iter().map(|(slot, _)| *slot)
    }
}

/// A screen's own button: an enchantment on an enchanting table, a recipe
/// on a stonecutter, a pattern on a loom (ADR-0036, M7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonPress {
    seen: Seen,
    button: u8,
}

impl ButtonPress {
    /// The highest button of an enchanting table: its third enchantment.
    pub const MAX_ENCHANTMENT: u8 = 2;

    /// Button `button` of `screen`, as the hook sees it.
    ///
    /// # Errors
    /// [`MenuRefusal::BadValue`] if the screen isn't an enchanting table,
    /// a stonecutter or a loom, or for an enchanting table, past
    /// [`ButtonPress::MAX_ENCHANTMENT`].
    pub fn on(screen: &Screen, button: u8) -> Result<Self, MenuRefusal> {
        let has = match screen.kind() {
            ScreenKind::Enchantment => button <= Self::MAX_ENCHANTMENT,
            ScreenKind::Stonecutter | ScreenKind::Loom => true,
            _ => false,
        };
        if !has {
            return Err(MenuRefusal::BadValue);
        }
        Ok(Self {
            seen: Seen::of(screen),
            button,
        })
    }

    /// The button's number.
    #[must_use]
    pub const fn button(&self) -> u8 {
        self.button
    }
}

/// A merchant's trade selected (ADR-0036, M7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeChoice {
    seen: Seen,
    index: u8,
    /// Boxed: three whole items, where every other action holds one or two.
    offer: Box<TradeOffer>,
}

impl TradeChoice {
    /// Trade `index` of `screen`, counted from 0, as the hook sees it.
    ///
    /// # Errors
    /// [`MenuRefusal::BadValue`] if the screen isn't a merchant's or has no
    /// such trade.
    pub fn on(screen: &Screen, index: u8) -> Result<Self, MenuRefusal> {
        // Only a merchant's screen has trades.
        let offer = screen
            .offers()
            .get(usize::from(index))
            .cloned()
            .map(Box::new)
            .ok_or(MenuRefusal::BadValue)?;
        Ok(Self {
            seen: Seen::of(screen),
            index,
            offer,
        })
    }

    /// The trade's place in the merchant's list.
    #[must_use]
    pub const fn index(&self) -> u8 {
        self.index
    }
}

/// Why a declared message's text isn't an [`AnvilName`]. It never holds
/// the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the name is {units} UTF-16 code units long; an anvil takes {max}", max = AnvilName::MAX_LEN)]
pub struct AnvilNameError {
    /// Its length in UTF-16 code units.
    pub units: usize,
}

/// The name an anvil gives an item, from a declared message (ADR-0036, M7;
/// D8): never text of the script's own.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnvilName(String);

impl AnvilName {
    /// The longest name an anvil takes, in UTF-16 code units, as a server
    /// measures it.
    pub const MAX_LEN: usize = 50;

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&ChatMessage> for AnvilName {
    type Error = AnvilNameError;

    /// A declared message's finished text as an anvil's name.
    ///
    /// # Errors
    /// [`AnvilNameError`] if it's longer than [`AnvilName::MAX_LEN`].
    fn try_from(message: &ChatMessage) -> Result<Self, AnvilNameError> {
        // A chat message holds nothing that hides or reorders text; only
        // its length is the anvil's own.
        let units = text::utf16_len(message.as_str());
        if units > Self::MAX_LEN {
            return Err(AnvilNameError { units });
        }
        Ok(Self(message.as_str().to_owned()))
    }
}

/// The anvil's name field set (ADR-0036, M7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    seen: Seen,
    name: AnvilName,
}

impl Rename {
    /// `name` in `screen`'s name field, as the hook sees it.
    ///
    /// # Errors
    /// [`MenuRefusal::BadValue`] if the screen isn't an anvil.
    pub fn on(screen: &Screen, name: AnvilName) -> Result<Self, MenuRefusal> {
        if screen.kind() != ScreenKind::Anvil {
            return Err(MenuRefusal::BadValue);
        }
        Ok(Self {
            seen: Seen::of(screen),
            name,
        })
    }

    /// The name.
    #[must_use]
    pub const fn name(&self) -> &AnvilName {
        &self.name
    }
}

/// Something a script does in a screen that the server answers: a click,
/// a button, a trade or a name (ADR-0036, M3, M7).
///
/// Each carries what the hook saw. [`guard`](Self::guard) is the pure
/// decision whether it may go out now, and [`ClickGate`](super::ClickGate)
/// sends one at a time. Closing a screen isn't one: it's never answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// A slot clicked.
    Click(Click),
    /// A screen's button pressed.
    Button(ButtonPress),
    /// A trade selected.
    Trade(TradeChoice),
    /// The anvil's name set.
    Rename(Rename),
}

impl MenuAction {
    /// The screen the hook saw.
    #[must_use]
    pub const fn screen(&self) -> ScreenId {
        self.seen().screen
    }

    /// The slot it clicks, if it's a click.
    #[must_use]
    pub const fn slot(&self) -> Option<u16> {
        match self {
            Self::Click(click) => Some(click.slot),
            Self::Button(_) | Self::Trade(_) | Self::Rename(_) => None,
        }
    }

    const fn seen(&self) -> &Seen {
        match self {
            Self::Click(Click { seen, .. })
            | Self::Button(ButtonPress { seen, .. })
            | Self::Trade(TradeChoice { seen, .. })
            | Self::Rename(Rename { seen, .. }) => seen,
        }
    }

    /// Whether it may go out onto `now`, the screen open at the moment of
    /// sending: the click guard (ADR-0036, M3; P16.6).
    ///
    /// The checks, in this order, each a reason when it fails:
    /// 1. a screen is open ([`MenuRefusal::NoMenu`]);
    /// 2. it's the screen the hook saw, the same opening
    ///    ([`MenuRefusal::ScreenChanged`]);
    /// 3. its content is as the hook saw it: the same state, the same
    ///    cursor, and for a trade the same trade at its place
    ///    ([`MenuRefusal::ContentChanged`]);
    /// 4. every slot a click touches holds the whole item the hook saw, or
    ///    is empty as it was ([`MenuRefusal::SlotChanged`]).
    ///
    /// # Errors
    /// The first check that fails, as its [`MenuRefusal`].
    pub fn guard(&self, now: Option<&Screen>) -> Result<(), MenuRefusal> {
        let now = self.seen().check(now)?;
        match self {
            Self::Click(click) => {
                // The whole item: `Item`'s equality is every field of it,
                // its data components included.
                let as_seen = click.touched.iter().all(|(slot, seen)| {
                    now.slot(*slot).map(|now| now.item()) == Some(seen.as_ref())
                });
                if !as_seen {
                    return Err(MenuRefusal::SlotChanged);
                }
            }
            Self::Trade(choice) => {
                if now.offers().get(usize::from(choice.index)) != Some(choice.offer.as_ref()) {
                    return Err(MenuRefusal::ContentChanged);
                }
            }
            Self::Button(_) | Self::Rename(_) => {}
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::script::menu::item::tests::item;
    use crate::script::menu::screen::tests::screen_id;
    use crate::script::menu::{ItemData, Slot};
    use core::num::NonZeroU32;
    use proptest::prelude::*;
    use rstest::rstest;

    fn hotbar(n: u8) -> HotbarSlot {
        HotbarSlot::try_from(n).unwrap()
    }

    /// A chest of one row whose slots `items` fill, the bot's 36 empty.
    pub(crate) fn chest(
        opening: u64,
        state: u32,
        items: &[Option<Item>],
        cursor: Option<Item>,
    ) -> Screen {
        Screen::new(
            screen_id(1, opening),
            StateId::new(state),
            ScreenKind::Generic9x1,
            "Chest",
            items.iter().cloned(),
            cursor,
        )
    }

    fn listing(price: &str) -> Item {
        item("diamond", 1)
            .named("Diamond")
            .with_lore([format!("Price: {price}")])
    }

    /// The auction house as the hook saw it: two diamond listings that
    /// differ only in their price.
    fn seen() -> Screen {
        chest(1, 5, &[Some(listing("500")), Some(listing("5,000"))], None)
    }

    fn left(slot: u16) -> MenuAction {
        MenuAction::Click(Click::on(&seen(), ClickKind::Left, slot).unwrap())
    }

    #[test]
    fn a_click_goes_out_onto_the_screen_the_hook_saw() {
        assert_eq!(left(0).guard(Some(&seen())), Ok(()));
        assert_eq!(left(0).screen(), screen_id(1, 1));
        assert_eq!(left(0).slot(), Some(0));
    }

    #[test]
    fn no_screen_is_no_menu() {
        assert_eq!(left(0).guard(None), Err(MenuRefusal::NoMenu));
    }

    #[rstest]
    #[case::another_opening(chest(2, 5, &[Some(listing("500")), Some(listing("5,000"))], None))]
    #[case::another_window(Screen::new(
        screen_id(2, 1),
        StateId::new(5),
        ScreenKind::Generic9x1,
        "Chest",
        [Some(listing("500")), Some(listing("5,000"))],
        None,
    ))]
    fn another_screen_even_the_same_in_every_slot_is_screen_changed(#[case] now: Screen) {
        assert_eq!(left(0).guard(Some(&now)), Err(MenuRefusal::ScreenChanged));
    }

    #[rstest]
    #[case::a_newer_state(chest(1, 6, &[Some(listing("500")), Some(listing("5,000"))], None))]
    #[case::an_older_state(chest(1, 4, &[Some(listing("500")), Some(listing("5,000"))], None))]
    #[case::the_cursor(chest(
        1,
        5,
        &[Some(listing("500")), Some(listing("5,000"))],
        Some(item("stone", 1)),
    ))]
    fn a_screen_whose_content_moved_on_is_content_changed(#[case] now: Screen) {
        assert_eq!(left(0).guard(Some(&now)), Err(MenuRefusal::ContentChanged));
    }

    /// The reviewer's case: two listings of one kind of item that differ
    /// only in their lore. The bargain was bought; the dear one moved into
    /// its slot, in the same state.
    #[test]
    fn another_listing_of_the_same_item_in_the_slot_is_slot_changed() {
        let now = chest(1, 5, &[Some(listing("5,000")), None], None);

        assert_eq!(left(0).guard(Some(&now)), Err(MenuRefusal::SlotChanged));
    }

    #[rstest]
    #[case::the_count(item("diamond", 2).named("Diamond").with_lore(["Price: 500"]))]
    #[case::the_name(item("diamond", 1).named("Diamonds").with_lore(["Price: 500"]))]
    #[case::the_lore(item("diamond", 1).named("Diamond").with_lore(["Price: 501"]))]
    #[case::another_kind(item("emerald", 1).named("Diamond").with_lore(["Price: 500"]))]
    #[case::a_component_a_script_reads(
        listing("500").with_enchantments([("minecraft:mending", 1)])
    )]
    #[case::a_component_no_script_reads(
        Item::new(item("diamond", 1).id().clone(), NonZeroU32::MIN, ItemData::new([7]))
            .named("Diamond")
            .with_lore(["Price: 500"])
    )]
    fn any_difference_in_the_whole_item_is_slot_changed(#[case] other: Item) {
        let now = chest(1, 5, &[Some(other), Some(listing("5,000"))], None);

        assert_eq!(left(0).guard(Some(&now)), Err(MenuRefusal::SlotChanged));
    }

    #[test]
    fn an_empty_slot_that_was_filled_is_slot_changed_and_the_other_way_round() {
        let empty = chest(1, 5, &[None, Some(listing("5,000"))], None);
        let filled_where_empty = chest(
            1,
            5,
            &[
                Some(listing("500")),
                Some(listing("5,000")),
                Some(item("stone", 1)),
            ],
            None,
        );

        assert_eq!(left(0).guard(Some(&empty)), Err(MenuRefusal::SlotChanged));
        assert_eq!(
            left(2).guard(Some(&filled_where_empty)),
            Err(MenuRefusal::SlotChanged)
        );
        // A slot the click doesn't touch isn't judged.
        assert_eq!(left(0).guard(Some(&filled_where_empty)), Ok(()));
    }

    #[test]
    fn a_number_key_also_needs_its_hotbar_slot_as_it_was() {
        let screen = seen();
        let click = Click::on(&screen, ClickKind::NumberKey(hotbar(0)), 0).unwrap();
        let swap = MenuAction::Click(click.clone());
        let mut items: Vec<Option<Item>> = vec![Some(listing("500")), Some(listing("5,000"))];
        items.resize(9 + 27, None);
        items.push(Some(item("stone", 1)));
        let hotbar_filled = chest(1, 5, &items, None);

        assert_eq!(swap.guard(Some(&screen)), Ok(()));
        assert_eq!(
            swap.guard(Some(&hotbar_filled)),
            Err(MenuRefusal::SlotChanged)
        );
        assert_eq!(click.touched().collect::<Vec<u16>>(), [0, 36]);
    }

    #[rstest]
    #[case::left(ClickKind::Left, "left")]
    #[case::right(ClickKind::Right, "right")]
    #[case::shift(ClickKind::Shift, "shift")]
    #[case::number_key(ClickKind::NumberKey(HotbarSlot::LAST), "number_key")]
    #[case::drop(ClickKind::Drop, "drop")]
    #[case::drop_stack(ClickKind::DropStack, "drop_stack")]
    fn every_kind_of_click_touches_its_slot(#[case] kind: ClickKind, #[case] name: &str) {
        let click = Click::on(&seen(), kind, 1).unwrap();

        assert_eq!(kind.as_str(), name);
        assert_eq!(click.kind(), kind);
        assert_eq!(click.slot(), 1);
        assert_eq!(click.touched().next(), Some(1));
    }

    #[rstest]
    #[case::past_the_last(45)]
    #[case::far_past(u16::MAX)]
    fn a_slot_the_screen_doesnt_have_is_a_bad_value(#[case] slot: u16) {
        assert_eq!(
            Click::on(&seen(), ClickKind::Left, slot),
            Err(MenuRefusal::BadValue)
        );
    }

    fn screen_of(kind: ScreenKind) -> Screen {
        Screen::new(screen_id(1, 1), StateId::new(1), kind, "", [], None)
    }

    #[rstest]
    #[case::an_enchantment(ScreenKind::Enchantment, 0, true)]
    #[case::the_third_enchantment(ScreenKind::Enchantment, 2, true)]
    #[case::no_fourth_enchantment(ScreenKind::Enchantment, 3, false)]
    #[case::a_stonecutters_recipe(ScreenKind::Stonecutter, 30, true)]
    #[case::a_looms_pattern(ScreenKind::Loom, 255, true)]
    #[case::a_chest_has_none(ScreenKind::Generic9x3, 0, false)]
    #[case::an_anvil_has_none(ScreenKind::Anvil, 0, false)]
    fn a_button_is_one_the_screen_has(
        #[case] kind: ScreenKind,
        #[case] button: u8,
        #[case] has: bool,
    ) {
        let screen = screen_of(kind);

        let press = ButtonPress::on(&screen, button);

        assert_eq!(press.is_ok(), has);
        if let Ok(press) = press {
            assert_eq!(press.button(), button);
            assert_eq!(MenuAction::Button(press).guard(Some(&screen)), Ok(()));
        } else {
            assert_eq!(press, Err(MenuRefusal::BadValue));
        }
    }

    #[test]
    fn a_button_is_guarded_by_its_screen_and_state() {
        let screen = screen_of(ScreenKind::Enchantment);
        let press = MenuAction::Button(ButtonPress::on(&screen, 1).unwrap());
        let newer = Screen::new(
            screen_id(1, 1),
            StateId::new(2),
            ScreenKind::Enchantment,
            "",
            [],
            None,
        );

        assert_eq!(press.guard(Some(&newer)), Err(MenuRefusal::ContentChanged));
        assert_eq!(press.guard(None), Err(MenuRefusal::NoMenu));
        assert_eq!(press.slot(), None);
    }

    fn merchant(offers: &[(u32, &str)]) -> Screen {
        screen_of(ScreenKind::Merchant).with_offers(offers.iter().map(|(cost, result)| {
            TradeOffer::new(item("emerald", *cost), None, item(result, 1), false)
        }))
    }

    #[test]
    fn a_trade_is_one_the_merchant_offers_and_stays_as_seen() {
        let screen = merchant(&[(1, "bread"), (3, "diamond_sword")]);
        let choice = TradeChoice::on(&screen, 1).unwrap();
        let trade = MenuAction::Trade(choice.clone());

        assert_eq!(choice.index(), 1);
        assert_eq!(trade.guard(Some(&screen)), Ok(()));
        // The merchant's trades changed in the same state.
        assert_eq!(
            trade.guard(Some(&merchant(&[(1, "bread"), (9, "diamond_sword")]))),
            Err(MenuRefusal::ContentChanged)
        );
        assert_eq!(
            trade.guard(Some(&merchant(&[(1, "bread")]))),
            Err(MenuRefusal::ContentChanged)
        );
        assert_eq!(TradeChoice::on(&screen, 2), Err(MenuRefusal::BadValue));
        assert_eq!(
            TradeChoice::on(&screen_of(ScreenKind::Generic9x3), 0),
            Err(MenuRefusal::BadValue)
        );
    }

    fn message(text: &str) -> ChatMessage {
        ChatMessage::try_from(text).unwrap()
    }

    #[test]
    fn an_anvils_name_is_a_declared_message_of_at_most_50_units() {
        let fifty = "x".repeat(50);
        let name = AnvilName::try_from(&message(&fifty)).unwrap();

        assert_eq!(name.as_str(), fifty);
        assert_eq!(
            AnvilName::try_from(&message(&"x".repeat(51))),
            Err(AnvilNameError { units: 51 })
        );
        // An emoji is two units, as a server counts it.
        assert_eq!(
            AnvilName::try_from(&message(&"\u{1f600}".repeat(26))),
            Err(AnvilNameError { units: 52 })
        );
        assert_eq!(
            AnvilNameError { units: 51 }.to_string(),
            "the name is 51 UTF-16 code units long; an anvil takes 50"
        );
    }

    #[test]
    fn a_name_goes_only_into_an_anvil() {
        let name = AnvilName::try_from(&message("Farm Sword")).unwrap();
        let anvil = screen_of(ScreenKind::Anvil);

        let rename = Rename::on(&anvil, name.clone()).unwrap();

        assert_eq!(rename.name(), &name);
        assert_eq!(MenuAction::Rename(rename).guard(Some(&anvil)), Ok(()));
        assert_eq!(
            Rename::on(&screen_of(ScreenKind::Generic9x3), name),
            Err(MenuRefusal::BadValue)
        );
    }

    #[rstest]
    #[case::no_menu(MenuRefusal::NoMenu, "no_menu")]
    #[case::screen_changed(MenuRefusal::ScreenChanged, "screen_changed")]
    #[case::content_changed(MenuRefusal::ContentChanged, "content_changed")]
    #[case::slot_changed(MenuRefusal::SlotChanged, "slot_changed")]
    #[case::click_pending(MenuRefusal::ClickPending, "click_pending")]
    #[case::unanswered(MenuRefusal::Unanswered, "unanswered")]
    #[case::screen_open(MenuRefusal::ScreenOpen, "screen_open")]
    #[case::bad_value(MenuRefusal::BadValue, "bad_value")]
    fn each_reason_reads_as_the_reference_names_it(
        #[case] reason: MenuRefusal,
        #[case] text: &str,
    ) {
        assert_eq!(reason.as_str(), text);
        assert_eq!(reason.to_string(), text);
        assert!(MenuRefusal::ALL.contains(&reason));
    }

    /// A small world of items, so two screens are often alike.
    pub(crate) fn any_item() -> impl Strategy<Value = Option<Item>> {
        prop_oneof![
            Just(None),
            (0_u8..2, 1_u32..3, 0_u8..2, 0_u8..2).prop_map(|(kind, count, name, data)| {
                let kind = if kind == 0 { "diamond" } else { "stone" };
                Some(
                    Item::new(
                        item(kind, 1).id().clone(),
                        NonZeroU32::new(count).unwrap(),
                        ItemData::new(vec![data]),
                    )
                    .named(if name == 0 { "" } else { "A" }),
                )
            }),
        ]
    }

    /// A one-row chest drawn from a small world: two openings of one
    /// window, two states, and the items of [`any_item`].
    pub(crate) fn any_chest() -> impl Strategy<Value = Screen> {
        (
            1_u64..3,
            0_u32..2,
            proptest::collection::vec(any_item(), 45),
            any_item(),
        )
            .prop_map(|(opening, state, items, cursor)| chest(opening, state, &items, cursor))
    }

    pub(crate) fn any_click_kind() -> impl Strategy<Value = ClickKind> {
        prop_oneof![
            Just(ClickKind::Left),
            Just(ClickKind::Right),
            Just(ClickKind::Shift),
            (0_u8..9).prop_map(|n| ClickKind::NumberKey(HotbarSlot::try_from(n).unwrap())),
            Just(ClickKind::Drop),
            Just(ClickKind::DropStack),
        ]
    }

    /// Whether a click seen on `seen` would land on what the hook saw if
    /// it went out onto `now`: the model the guard is held to.
    pub(crate) fn lands_as_seen(click: &Click, seen: &Screen, now: &Screen) -> bool {
        seen.id() == now.id()
            && seen.state() == now.state()
            && seen.cursor() == now.cursor()
            && click.touched().all(|slot| {
                seen.slot(slot).map(|slot| slot.item()) == now.slot(slot).map(|slot| slot.item())
            })
    }

    proptest! {
        /// Whatever the screen the hook saw and the screen open now, a
        /// click goes out exactly when the screen, its state, its cursor
        /// and every slot it touches are as the hook saw them.
        #[test]
        fn the_guard_lets_a_click_through_only_onto_what_the_hook_saw(
            seen in any_chest(),
            now in any_chest(),
            kind in any_click_kind(),
            slot in 0_u16..45,
        ) {
            let click = Click::on(&seen, kind, slot).unwrap();
            let action = MenuAction::Click(click.clone());

            let verdict = action.guard(Some(&now));

            prop_assert_eq!(verdict.is_ok(), lands_as_seen(&click, &seen, &now));
        }

        /// The same with `now` made from what the hook saw by a few
        /// changes, so it's often alike in all but one thing.
        #[test]
        fn the_guard_finds_any_one_difference(
            seen in any_chest(),
            kind in any_click_kind(),
            slot in 0_u16..45,
            changes in proptest::collection::vec((0_u8..4, 0_u16..45, any_item()), 0..3),
        ) {
            let mut items: Vec<Option<Item>> =
                (0..45).map(|index| seen.slot(index).and_then(Slot::item).cloned()).collect();
            let mut opening = 1;
            let mut state = seen.state().get();
            let mut cursor = seen.cursor().cloned();
            for (what, at, new) in changes {
                match what {
                    0 => opening += 1,
                    1 => state += 1,
                    2 => cursor = new,
                    _ => {
                        if let Some(item) = items.get_mut(usize::from(at)) {
                            *item = new;
                        }
                    }
                }
            }
            // The hook saw opening 1 or 2; a change of opening is another.
            let opening = if opening == 1 { seen_opening(&seen) } else { 9 };
            let now = chest(opening, state, &items, cursor);
            let click = Click::on(&seen, kind, slot).unwrap();

            let verdict = MenuAction::Click(click.clone()).guard(Some(&now));

            prop_assert_eq!(verdict.is_ok(), lands_as_seen(&click, &seen, &now));
        }
    }

    /// Which opening of window 1 `screen` is: 1 or 2 in these tests.
    fn seen_opening(screen: &Screen) -> u64 {
        if screen.id() == screen_id(1, 1) { 1 } else { 2 }
    }
}
