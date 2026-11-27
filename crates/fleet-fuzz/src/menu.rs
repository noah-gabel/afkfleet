//! Menus (Plan.md P16.14; ADR-0036, M1 to M6, R1; ADR-0050, ADR-0052): a
//! screen and an inventory made from anything a server could send, the
//! number helpers that read a server's amounts, and the click guard with
//! one action at a time.
//!
//! Each target reads its bytes through `Bytes`, which never runs out: a
//! read past the end is zero, so every input makes some screen or some
//! sequence, and a short one a small one.

use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Utc};
use fleet_core::mc::MenuView;
use fleet_core::script::amount::find_amount;
use fleet_core::script::menu::{
    BotInventory, Click, ClickFate, ClickGate, ClickKind, Item, ItemData, Lore, MenuAction, Screen,
    ScreenKind, StateId, TradeOffer, WindowId,
};
use fleet_core::script::{
    ActionLimiter, BlockPosition, Blocks, BotFacts, DecimalMark, HookEvent, Identifier, Position,
    Script, ScriptDraft, Snapshot, Stopwatch,
};
use fleet_core::time;
use fleet_script::{Instance, Moment, Outcome, Presence, Program};

/// A reader of a target's bytes that never runs out.
pub(crate) struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    /// The next byte; 0 past the end.
    pub(crate) fn byte(&mut self) -> u8 {
        let byte = self.data.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        byte
    }

    /// The next two bytes as a number.
    fn wide(&mut self) -> u16 {
        u16::from_le_bytes([self.byte(), self.byte()])
    }

    /// The next four bytes as a number.
    fn int(&mut self) -> i32 {
        i32::from_le_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }

    /// A text: a length of up to `max` bytes, then that many, as UTF-8 with
    /// every bad sequence replaced.
    fn text(&mut self, max: usize) -> String {
        let len = usize::from(self.wide()) % (max + 1);
        let start = self.at.min(self.data.len());
        let end = start.saturating_add(len).min(self.data.len());
        self.at = self.at.saturating_add(len);
        String::from_utf8_lossy(self.data.get(start..end).unwrap_or_default()).into_owned()
    }

    /// Whether anything is left to read.
    fn left(&self) -> bool {
        self.at < self.data.len()
    }
}

/// What a seed writes for [`Bytes`] to read back: the same order, the
/// same lengths.
#[derive(Default)]
pub(crate) struct Writer(pub(crate) Vec<u8>);

impl Writer {
    pub(crate) fn byte(&mut self, byte: u8) -> &mut Self {
        self.0.push(byte);
        self
    }

    fn wide(&mut self, wide: u16) -> &mut Self {
        self.0.extend(wide.to_le_bytes());
        self
    }

    fn int(&mut self, int: i32) -> &mut Self {
        self.0.extend(int.to_le_bytes());
        self
    }

    pub(crate) fn text(&mut self, text: &str) -> &mut Self {
        let len = u16::try_from(text.len()).unwrap_or(u16::MAX);
        self.wide(len);
        self.0.extend(text.as_bytes());
        self
    }

    /// An item of the known ID at `known` (see [`kind_of`]), with a name,
    /// lore lines and a potion.
    fn item(&mut self, known: u8, name: &str, lore: &[&str], potion: &str) -> &mut Self {
        self.byte(1).byte(known).byte(1).text("");
        self.text(name);
        self.byte(u8::try_from(lore.len()).unwrap_or(0));
        for line in lore {
            self.text(line);
        }
        self.text(potion);
        // No enchantments of either kind; a durability.
        self.byte(0).byte(0).byte(0).wide(250)
    }

    fn empty(&mut self) -> &mut Self {
        self.byte(0)
    }
}

/// The harness's auction house as a seed: 45 potions listed with their
/// price, seller and time left, its buttons, and an inventory of potions
/// and one enchanted sword (ADR-0049).
pub(crate) fn auction_page() -> Vec<u8> {
    let mut writer = Writer::default();
    // Generic 9x6, its title, its window, its 54 slots.
    writer.byte(6).text("Auction House").byte(3).byte(54);
    for n in 0..45_u32 {
        let price = format!("Price: ${},250.00", n + 1);
        let seller = format!("Seller: Seller{n}");
        writer.item(
            0,
            "Potion of the Turtle Master",
            &[&price, &seller, "Expires in: 59m 12s"],
            "minecraft:turtle_master",
        );
    }
    for slot in 45..54_u32 {
        if slot == 47 {
            writer.item(2, "Create", &["Start a new listing"], "");
        } else {
            writer.item(4, " ", &[], "");
        }
    }
    // No cursor, no trades, the state.
    writer.empty().byte(0).wide(48);
    for slot in 0..46_u32 {
        if slot == 36 {
            writer
                .byte(1)
                .byte(5)
                .byte(1)
                .text("")
                .text("Blade")
                .byte(0)
                .text("");
            writer
                .byte(2)
                .text("minecraft:sharpness")
                .int(5)
                .text("minecraft:unbreaking")
                .int(3);
            writer.byte(0).byte(1);
        } else if (9..20).contains(&slot) {
            writer.item(0, "", &[], "minecraft:turtle_master");
        } else {
            writer.empty();
        }
    }
    writer.0
}

/// A registry ID from `bytes`: one of a few real ones, or one of the
/// bytes' own that may be no identifier at all.
fn kind_of(bytes: &mut Bytes<'_>) -> Option<Identifier> {
    const KNOWN: [&str; 6] = [
        "minecraft:potion",
        "minecraft:diamond",
        "minecraft:emerald",
        "minecraft:enchanted_book",
        "minecraft:lime_stained_glass_pane",
        "minecraft:diamond_sword",
    ];
    let pick = bytes.byte();
    match KNOWN.get(usize::from(pick % 8)) {
        Some(known) => Identifier::try_from(*known).ok(),
        None => Identifier::try_from(bytes.text(64).as_str()).ok(),
    }
}

/// An item as a server could send it: any name, any lore, any potion, any
/// enchantments and levels, any components; `None` for an empty slot or
/// an ID that is none.
fn item(bytes: &mut Bytes<'_>) -> Option<Item> {
    if bytes.byte().is_multiple_of(4) {
        return None;
    }
    let id = kind_of(bytes)?;
    let count = NonZeroU32::new(u32::from(bytes.byte()).max(1))?;
    let data = ItemData::new(bytes.text(64).into_bytes());
    let name = bytes.text(400);
    let lines: Vec<String> = (0..bytes.byte() % 24).map(|_| bytes.text(400)).collect();
    let potion = bytes.text(80);
    let enchantments: Vec<(String, i32)> = (0..bytes.byte() % 24)
        .map(|_| (bytes.text(40), bytes.int()))
        .collect();
    let stored: Vec<(String, i32)> = (0..bytes.byte() % 24)
        .map(|_| (bytes.text(40), bytes.int()))
        .collect();
    let durability = (bytes.byte().is_multiple_of(2)).then(|| u32::from(bytes.wide()));
    Some(
        Item::new(id, count, data)
            .with_durability(durability)
            .named(&name)
            .with_lore(lines)
            .with_potion(Some(potion.as_str()))
            .with_enchantments(enchantments.iter().map(|(id, level)| (id.as_str(), *level)))
            .with_stored_enchantments(stored.iter().map(|(id, level)| (id.as_str(), *level))),
    )
}

/// A screen, an inventory and a cursor as a server could send them.
fn menu_view(bytes: &mut Bytes<'_>) -> MenuView {
    let kinds = ScreenKind::ALL;
    let kind = kinds
        .get(usize::from(bytes.byte()) % kinds.len())
        .copied()
        .unwrap_or(ScreenKind::Generic9x6);
    let title = bytes.text(2000);
    let mut gate = ClickGate::new();
    let id = gate.opened(WindowId::new(i32::from(bytes.byte()) + 1));
    // More slots than any screen has: what's past its last is never read.
    let items: Vec<Option<Item>> = (0..usize::from(bytes.byte()) % 100)
        .map(|_| item(bytes))
        .collect();
    let cursor = item(bytes);
    let offers: Vec<TradeOffer> = (0..usize::from(bytes.byte()) % 80)
        .filter_map(|_| {
            let cost = item(bytes)?;
            let second = item(bytes);
            let result = item(bytes)?;
            Some(TradeOffer::new(
                cost,
                second,
                result,
                bytes.byte().is_multiple_of(2),
            ))
        })
        .collect();
    let screen = Screen::new(
        id,
        StateId::new(u32::from(bytes.wide())),
        kind,
        &title,
        items,
        cursor,
    )
    .with_offers(offers);
    let inventory = BotInventory::new((0..46).map(|_| item(bytes)));
    MenuView {
        inventory,
        screen: Some(screen),
        sign: None,
    }
}

/// A script that reads everything a screen and the inventory hold, through
/// every reader of level 2.
const READER: &str = r#"fn on_menu_open() {
    let n = 0;
    for i in menu.slots {
        let slot = menu.slot(i);
        n += slot.lore.len() + slot.enchantments.len() + slot.stored_enchantments.len();
        if menu.lore_has(i, "Seller: ") { n += 1; }
        if menu.lore_number(slot, "Price") != () { n += 1; }
        if menu.lore_number(i, "Price", ",") != () { n += 1; }
        if slot.potion != () && slot.name != "" { n += 1; }
    }
    for i in menu.player {
        let slot = menu.slot(i);
        if !slot.empty { n += slot.count; }
    }
    for i in 0..menu.trades {
        let trade = menu.trade(i);
        n += trade.result_stored_enchantments.len() + trade.cost_count;
    }
    for i in inventory.slots {
        if inventory.lore_has(i, "x") { n += 1; }
    }
    if menu.number(menu.title) != () { n += 1; }
    if menu.cursor != () { n += 1; }
    state.read = n;
}"#;

/// The reader, compiled once at level 2; `None` if it didn't compile,
/// which [`menu_snapshot`] asserts never happens.
static PROGRAM: LazyLock<Option<Arc<Program>>> = LazyLock::new(|| {
    let script = Script::try_from(ScriptDraft {
        api: 2,
        source: READER.to_owned(),
        leash: 8,
        tool_guard: 2,
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
        signs: Vec::new(),
    })
    .ok()?;
    fleet_script::compile(&script).ok().map(Arc::new)
});

/// The moment every target's hook runs at.
fn moment() -> DateTime<Utc> {
    time::add(DateTime::<Utc>::UNIX_EPOCH, Duration::from_hours(500_000))
}

/// A stopwatch that stands still: the reader's time isn't what's judged.
#[derive(Debug)]
struct Still;

impl Stopwatch for Still {
    fn elapsed(&self) -> Duration {
        Duration::ZERO
    }
}

fn facts() -> Option<Snapshot> {
    let bot = BotFacts {
        health: 20.0,
        food: 20,
        position: Position::try_new(0.5, 64.0, 0.5).ok()?,
        yaw: 0.0,
        pitch: 0.0,
        dimension: Identifier::try_from("overworld").ok()?,
        riding: None,
        held: None,
        selected: fleet_core::mode::HotbarSlot::FIRST,
        looking_at: None,
    };
    Some(Snapshot::new(
        bot,
        Vec::new(),
        Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |_| None),
    ))
}

/// The `menu_snapshot` target: a screen, its trades and the bot's
/// inventory made from anything a server could send. **What's kept stays
/// within its bounds** (exactly the kind's slots, a title of 128
/// characters, a name of 64, 16 lore lines of 128, 16 enchantments of each
/// kind at levels 0 to 255, 64 trades), and **a script reads every bit of
/// it and runs to its end:** no text, list or map a server sends is past
/// what one of the script's values may hold (ADR-0052).
///
/// # Panics
/// When a property breaks.
pub fn menu_snapshot(data: &[u8]) {
    let mut bytes = Bytes::new(data);
    let view = menu_view(&mut bytes);
    let Some(screen) = &view.screen else { return };

    let slots = screen.kind().slots();
    assert!(
        (0..slots).all(|slot| screen.slot(slot).is_some()),
        "a slot missing"
    );
    assert!(screen.slot(slots).is_none(), "a slot past the kind's");
    assert!(
        screen.title().as_str().chars().count() <= Screen::MAX_TITLE,
        "a long title"
    );
    assert!(
        screen.offers().len() <= Screen::MAX_OFFERS,
        "too many trades"
    );
    let items = screen
        .own()
        .chain(screen.player())
        .chain(view.inventory.slots())
        .filter_map(fleet_core::script::menu::Slot::item);
    for item in items {
        assert!(
            item.name().as_str().chars().count() <= Item::MAX_NAME,
            "a long name"
        );
        assert!(
            item.lore().lines().len() <= Lore::MAX_LINES,
            "too much lore"
        );
        for line in item.lore().lines() {
            assert!(
                line.as_str().chars().count() <= Lore::MAX_LINE,
                "a long lore line"
            );
            assert!(
                !line.as_str().contains(['\n', '\r', '\u{a7}']),
                "a line holds formatting"
            );
        }
        assert!(item.enchantments().len() <= 16 && item.stored_enchantments().len() <= 16);
    }

    let program = PROGRAM.as_ref();
    assert!(program.is_some(), "the reader doesn't compile");
    let (Some(program), Some(facts)) = (program, facts()) else {
        return;
    };
    let mut instance = Instance::new(Arc::clone(program), 7, Arc::new(Still));
    let moment = Moment {
        facts: &facts,
        now: moment(),
        presence: Presence::InWorld,
        leash: None,
        menu: &view,
        name: "AfkBot1",
    };
    let ran = instance.handle(&HookEvent::MenuOpen, &moment, &mut ActionLimiter::new());
    assert!(
        matches!(ran.outcome, Outcome::Ran { .. }),
        "a script couldn't read what the server sent: {:?}",
        ran.outcome
    );
}

/// The `number_helpers` target: the readers of a server's amounts (P16.7,
/// B8). Whatever the text, the first number read is a finite one or none,
/// the same each time, and a lore's number after a label is the first
/// number after that label on the first line that starts with it.
///
/// # Panics
/// When a property breaks.
pub fn number_helpers(data: &[u8]) {
    let mut bytes = Bytes::new(data);
    let mark = if bytes.byte().is_multiple_of(2) {
        DecimalMark::Point
    } else {
        DecimalMark::Comma
    };
    let label = bytes.text(16);
    let lines: Vec<String> = (0..bytes.byte() % 20).map(|_| bytes.text(200)).collect();
    let text = bytes.text(2000);

    let found = find_amount(&text, mark);
    assert_eq!(find_amount(&text, mark), found, "two reads differ");
    assert!(
        found.is_none_or(f64::is_finite),
        "a number that isn't finite"
    );

    let lore = Lore::read(&lines);
    let read = lore.number(&label, mark);
    assert!(read.is_none_or(f64::is_finite));
    // Leading spaces aside, case counted: the label's own first line.
    if !label.is_empty() && label.trim_start() == label {
        let expected = lore
            .lines()
            .iter()
            .find_map(|line| line.as_str().trim_start().strip_prefix(label.as_str()))
            .and_then(|rest| find_amount(rest, mark));
        assert_eq!(read, expected, "the lore's number isn't its label's line's");
    }
}

/// The `click_guard` target's session: a gate, the screen the server has
/// open, what the hook saw, and what was sent and answered.
struct Guarded {
    gate: ClickGate,
    now: DateTime<Utc>,
    screen: Option<Screen>,
    seen: Option<Screen>,
    sent: u32,
    fates: u32,
    pending: bool,
}

/// The window every screen of the target opens in, as plugins reuse one.
const WINDOW: WindowId = WindowId::new(1);

impl Guarded {
    /// A screen opens: a chest or a merchant, four slots of anything.
    fn open(&mut self, bytes: &mut Bytes<'_>) {
        let id = self.gate.opened(WINDOW);
        let kind = if bytes.byte().is_multiple_of(2) {
            ScreenKind::Generic9x1
        } else {
            ScreenKind::Merchant
        };
        let items: Vec<Option<Item>> = (0..4).map(|_| item(bytes)).collect();
        self.screen = Some(Screen::new(id, StateId::new(1), kind, "", items, None));
    }

    /// The server's whole content: a newer state, an item changed.
    fn content(&mut self, bytes: &mut Bytes<'_>) {
        let Some(open) = &self.screen else { return };
        let mut items: Vec<Option<Item>> = open.own().map(|slot| slot.item().cloned()).collect();
        if let Some(slot) = items.get_mut(usize::from(bytes.byte() % 4)) {
            *slot = item(bytes);
        }
        let state = StateId::new(open.state().get().saturating_add(1));
        let cursor = open.cursor().cloned();
        self.screen = Some(Screen::new(
            open.id(),
            state,
            open.kind(),
            "",
            items,
            cursor,
        ));
        self.gate.content(WINDOW);
    }

    /// The server's cursor.
    fn cursor(&mut self, bytes: &mut Bytes<'_>) {
        let Some(open) = &self.screen else { return };
        let items: Vec<Option<Item>> = open.own().map(|slot| slot.item().cloned()).collect();
        let cursor = item(bytes);
        self.screen = Some(Screen::new(
            open.id(),
            open.state(),
            open.kind(),
            "",
            items,
            cursor,
        ));
        self.gate.cursor();
    }

    /// A click on what the hook saw, sent onto the screen as it is now.
    fn click(&mut self, bytes: &mut Bytes<'_>) {
        let Some(hook) = &self.seen else { return };
        let kinds = [
            ClickKind::Left,
            ClickKind::Right,
            ClickKind::Shift,
            ClickKind::Drop,
        ];
        let kind = kinds
            .get(usize::from(bytes.byte()) % kinds.len())
            .copied()
            .unwrap_or(ClickKind::Left);
        let Ok(click) = Click::on(hook, kind, u16::from(bytes.byte() % 8)) else {
            return;
        };
        let action = MenuAction::Click(click.clone());
        let predicted: Vec<u16> = click.touched().collect();
        if self
            .gate
            .send(&action, self.screen.as_ref(), &predicted, self.now)
            .is_err()
        {
            return;
        }
        assert!(self.screen.is_some(), "a click went out onto no screen");
        let Some(open) = &self.screen else { return };
        assert!(!self.pending, "a click went out while another waited");
        assert_eq!(open.id(), hook.id(), "onto another screen");
        assert_eq!(open.state(), hook.state(), "onto another state");
        assert_eq!(open.cursor(), hook.cursor(), "with another cursor");
        for slot in click.touched() {
            assert_eq!(
                open.slot(slot).and_then(|slot| slot.item()),
                hook.slot(slot).and_then(|slot| slot.item()),
                "onto another item"
            );
        }
        self.sent += 1;
        self.pending = true;
    }

    /// A fate came, at a barrier's reply or after a wait.
    fn fate(&mut self, fate: Option<ClickFate>) {
        if let Some(fate) = fate {
            assert!(ClickFate::ALL.contains(&fate));
            self.fates += 1;
            self.pending = false;
        }
    }
}

/// The `click_guard` target: any sequence of screens opening and closing,
/// a server's corrections, what a hook saw, clicks, barrier replies and
/// waits. **A click goes out only onto the screen, the state, the cursor
/// and the whole items the hook saw, never while another waits for its
/// fate, and every click that went out gets exactly one fate**
/// (ADR-0036, M3, M4, R1; ADR-0050, B5, B6).
///
/// # Panics
/// When a property breaks.
pub fn click_guard(data: &[u8]) {
    let mut bytes = Bytes::new(data);
    let mut session = Guarded {
        gate: ClickGate::new(),
        now: moment(),
        screen: None,
        seen: None,
        sent: 0,
        fates: 0,
        pending: false,
    };
    for _ in 0..400 {
        if !bytes.left() {
            break;
        }
        match bytes.byte() % 9 {
            0 => session.open(&mut bytes),
            1 => session.content(&mut bytes),
            2 => session.gate.slot(WINDOW, u16::from(bytes.byte() % 40)),
            3 => session.cursor(&mut bytes),
            4 => session.seen.clone_from(&session.screen),
            5 => session.click(&mut bytes),
            6 => {
                let fate = session.gate.barrier_answered().map(|result| result.fate);
                session.fate(fate);
            }
            7 => {
                let wait = Duration::from_millis(u64::from(bytes.byte()) * 50);
                session.now = time::add(session.now, wait);
                let fate = session.gate.expire(session.now).map(|result| result.fate);
                session.fate(fate);
            }
            _ => {
                session.gate.closed();
                session.screen = None;
            }
        }
        assert!(session.fates <= session.sent, "a fate without a click");
    }
    // Long enough later every click has had its fate.
    let later = time::add(session.now, Duration::from_mins(1));
    let fate = session.gate.expire(later).map(|result| result.fate);
    session.fate(fate);
    assert_eq!(session.fates, session.sent, "a click without its fate");
}

/// The seeds of the menus' targets; `None` for another target.
pub(crate) fn seeds(name: &str) -> Option<Vec<Vec<u8>>> {
    Some(match name {
        "menu_snapshot" => vec![
            auction_page(),
            // Every byte the same: a merchant's long run of trades.
            vec![20; 3000],
            vec![0xff; 4000],
        ],
        "number_helpers" => [
            (
                0,
                "Price",
                vec!["Price: 1,250.50", "Seller: AfkBot1"],
                "Price: $1,250.50",
            ),
            (1, "Preis", vec!["Preis: 1.234,5 Münzen"], "12,5k"),
            (
                0,
                "Price",
                vec!["Seller: AfkBot1", "Price: 2B"],
                "AfkBot1 sells 3M",
            ),
            (0, "Price", vec!["Price: 1000000000000000000 or 5"], "-250"),
        ]
        .iter()
        .map(|(mark, label, lines, text)| {
            let mut writer = Writer::default();
            writer
                .byte(*mark)
                .text(label)
                .byte(u8::try_from(lines.len()).unwrap_or(0));
            for line in lines {
                writer.text(line);
            }
            writer.text(text);
            writer.0
        })
        .collect(),
        "click_guard" => vec![
            // Open, see, click, reply; again with a change in between.
            vec![0, 0, 1, 0, 4, 5, 0, 0, 6, 1, 0, 1, 5, 0, 0, 4, 5, 1, 1, 6],
            // A click, then a wait past its patience, then another.
            vec![0, 1, 4, 5, 2, 1, 7, 120, 7, 0, 4, 5, 0, 2, 6, 6],
            // A close while a click waits.
            vec![0, 0, 4, 5, 0, 0, 8, 6, 0, 0, 4, 5, 0, 0, 6],
        ],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_reader_reads_a_full_page_of_the_harness() {
        // A seed's page: 45 listings with lore, as the harness draws them.
        for seed in crate::seeds::seeds("menu_snapshot") {
            menu_snapshot(&seed);
        }
    }

    proptest! {
        #[test]
        fn a_snapshot_of_any_bytes_is_bounded_and_read_whole(data in prop::collection::vec(any::<u8>(), 0..4096)) {
            menu_snapshot(&data);
        }

        #[test]
        fn the_number_helpers_hold_for_any_bytes(data in prop::collection::vec(any::<u8>(), 0..512)) {
            number_helpers(&data);
        }

        #[test]
        fn the_click_guard_holds_for_any_sequence(data in prop::collection::vec(any::<u8>(), 0..2048)) {
            click_guard(&data);
        }
    }
}
