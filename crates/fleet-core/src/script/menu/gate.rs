//! [`ClickGate`]: one click at a time, and each click's fate from what the
//! server sent before the barrier's reply (ADR-0036, M4, R1; P16.6).

use core::fmt;
use core::time::Duration;

use chrono::{DateTime, Utc};

use super::action::{MenuAction, MenuRefusal};
use super::screen::{Screen, ScreenId, WindowId};
use crate::time;

/// What became of a click, decided at the barrier's reply (ADR-0036, R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClickFate {
    /// Nothing came for its window before the reply: the server did what
    /// was predicted.
    Ok,
    /// A correction came: the window's whole content, a slot it predicted
    /// or the cursor. The server put things back or did otherwise; a
    /// plugin's button whose click it cancels always ends here.
    Rejected,
    /// Its screen closed, or another opened, before the reply.
    Replaced,
    /// No reply came within [`ClickGate::UNANSWERED_AFTER`]. Its screen
    /// takes no further click until it's opened again.
    Unanswered,
    /// A correction came for a click on the screen's result slot
    /// ([`ScreenKind::result_slot`](super::ScreenKind::result_slot)): a
    /// trade, an anvil's result, a craft; or for a button, a trade or a
    /// name, which predict no slot, so what the server sends after one is
    /// its own picture.
    /// Taking a result is a transaction the server decides itself (it
    /// repeats a trade while the payment lasts, an anvil asks levels), so
    /// what it did may still be what was asked, once or more. Never read
    /// as [`Rejected`](Self::Rejected) (ADR-0052).
    Unclear,
}

impl ClickFate {
    /// Every fate, in a fixed order.
    pub const ALL: [Self; 5] = [
        Self::Ok,
        Self::Rejected,
        Self::Replaced,
        Self::Unanswered,
        Self::Unclear,
    ];

    /// The fate as a script reads it: `ok`, or `result.reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Rejected => "rejected",
            Self::Replaced => "replaced",
            Self::Unanswered => "unanswered",
            Self::Unclear => "unclear",
        }
    }
}

impl fmt::Display for ClickFate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A click's fate, with what it was: `on_click(result)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClickResult {
    /// The screen it went out onto.
    pub screen: ScreenId,
    /// The slot it clicked; `None` for a button, a trade or a name.
    pub slot: Option<u16>,
    /// What became of it.
    pub fate: ClickFate,
}

/// The screen that's open, which clicks the bot has sent onto it, and what
/// became of them: one click at a time a bot, and each click exactly one
/// fate (ADR-0036, M4, R1; ADR-0050).
///
/// It's pure: fleet-mc feeds it what the server sends and asks it before
/// every [`MenuAction`] it sends, on the host thread, and `now` is passed
/// in (ADR-0010).
///
/// **The barrier.** fleet-mc sends a request behind every action that the
/// server answers only after everything the action brought: R1's
/// statistics request today. Its reply is
/// [`barrier_answered`](Self::barrier_answered), whatever the request is,
/// so the barrier can change without changing this. The server answers barriers in the order
/// they were sent, so the gate counts them: a late reply to a click that
/// was already unanswered is that click's, and never answers the next.
///
/// **What it guarantees:**
/// - **Never two actions pending:** a second one before the first's fate
///   is [`MenuRefusal::ClickPending`], on any screen.
/// - **Every action sent gets exactly one fate:** at its barrier's reply,
///   or [`ClickFate::Unanswered`] at [`expire`](Self::expire) after
///   [`ClickGate::UNANSWERED_AFTER`], never both.
/// - **After `unanswered`, its screen takes nothing** until a screen is
///   opened again: [`MenuRefusal::Unanswered`].
/// - **An action goes out only onto what the hook saw:** the click guard
///   ([`MenuAction::guard`]) decides it at the moment of sending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClickGate {
    /// How many screens the session has opened.
    openings: u64,
    /// The screen that's open.
    open: Option<ScreenId>,
    /// The open screen, if its last action stayed unanswered.
    stuck: Option<ScreenId>,
    /// The action whose fate isn't known yet.
    pending: Option<Pending>,
    /// How many barriers went out.
    barriers_sent: u64,
    /// How many barriers the server answered.
    barriers_answered: u64,
}

/// What came for a pending action's window before its barrier's reply,
/// the worst first: a replaced screen outweighs a correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Came {
    Nothing,
    Correction,
    Replaced,
}

/// An action sent whose fate isn't known yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    screen: ScreenId,
    slot: Option<u16>,
    /// The slots azalea predicted it to change.
    predicted: Vec<u16>,
    /// Whether it clicked the screen's result slot, or predicts no slot.
    on_result: bool,
    /// Which barrier is behind it, counted from 1.
    barrier: u64,
    sent: DateTime<Utc>,
    came: Came,
}

impl ClickGate {
    /// How long an action waits for its barrier's reply before it's
    /// [`ClickFate::Unanswered`] (ADR-0036, R1).
    pub const UNANSWERED_AFTER: Duration = Duration::from_secs(5);

    /// A gate for a new session: no screen open, nothing sent.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The server opened a screen in `window`: it's open now, a new screen
    /// whatever was open before. An action pending on the screen before is
    /// [`ClickFate::Replaced`] at its barrier's reply.
    pub fn opened(&mut self, window: WindowId) -> ScreenId {
        self.gone();
        // A session opens far fewer than 2^64 screens; saturating keeps
        // the count from ever going round to an opening handed out before.
        self.openings = self.openings.saturating_add(1);
        let id = ScreenId::new(window, self.openings);
        self.open = Some(id);
        id
    }

    /// The bot opens its own inventory, as E does: a screen in window 0.
    ///
    /// # Errors
    /// [`MenuRefusal::ScreenOpen`] if a screen is open.
    pub fn open_inventory(&mut self) -> Result<ScreenId, MenuRefusal> {
        if self.open.is_some() {
            return Err(MenuRefusal::ScreenOpen);
        }
        Ok(self.opened(WindowId::INVENTORY))
    }

    /// The open screen is gone: the server closed it, a sign editor took
    /// its place, or the bot died or changed worlds. An action pending on
    /// it is [`ClickFate::Replaced`] at its barrier's reply.
    pub fn closed(&mut self) {
        self.gone();
        self.open = None;
    }

    /// The open screen is no more: whatever was pending on it is replaced,
    /// and it's stuck no longer.
    fn gone(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.came = Came::Replaced;
        }
        self.stuck = None;
    }

    /// The bot closes `seen`, the screen the hook saw, as Esc does. A
    /// close is never answered and never pending; an action pending on the
    /// screen is [`ClickFate::Replaced`] at its barrier's reply.
    ///
    /// # Errors
    /// [`MenuRefusal::NoMenu`] if no screen is open, and
    /// [`MenuRefusal::ScreenChanged`] if another one is.
    pub fn close(&mut self, seen: ScreenId) -> Result<(), MenuRefusal> {
        self.is_open(seen)?;
        self.closed();
        Ok(())
    }

    /// Whether `seen` is the screen that's open.
    fn is_open(&self, seen: ScreenId) -> Result<(), MenuRefusal> {
        match self.open {
            None => Err(MenuRefusal::NoMenu),
            Some(open) if open != seen => Err(MenuRefusal::ScreenChanged),
            Some(_) => Ok(()),
        }
    }

    /// The screen that's open.
    #[must_use]
    pub const fn open(&self) -> Option<ScreenId> {
        self.open
    }

    /// Whether an action's fate isn't known yet: `menu.pending`.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Asks to send `action` onto `now`, the screen open at this moment,
    /// with the slots azalea predicts it to change; `at` is now. When it's
    /// allowed, the action is pending, and fleet-mc sends it and its
    /// barrier behind it.
    ///
    /// # Errors
    /// The first of: [`MenuRefusal::NoMenu`] or
    /// [`MenuRefusal::ScreenChanged`] when the gate's open screen isn't
    /// the one the hook saw; [`MenuRefusal::ClickPending`];
    /// [`MenuRefusal::Unanswered`]; then the click guard's
    /// ([`MenuAction::guard`]).
    pub fn send(
        &mut self,
        action: &MenuAction,
        now: Option<&Screen>,
        predicted: &[u16],
        at: DateTime<Utc>,
    ) -> Result<(), MenuRefusal> {
        let seen = action.screen();
        self.is_open(seen)?;
        if self.pending.is_some() {
            return Err(MenuRefusal::ClickPending);
        }
        if self.stuck == Some(seen) {
            return Err(MenuRefusal::Unanswered);
        }
        action.guard(now)?;
        // A predicted slot the screen doesn't have can't be corrected; the
        // rest is bounded by the screen's own size.
        let mut predicted: Vec<u16> = predicted
            .iter()
            .copied()
            .filter(|slot| now.is_some_and(|now| now.slot(*slot).is_some()))
            .collect();
        predicted.sort_unstable();
        predicted.dedup();
        self.barriers_sent = self.barriers_sent.saturating_add(1);
        // A click on the screen's result slot, or an action that predicts no
        // slot (a button, a trade, a name): a correction of it doesn't say
        // the trade, the name or the craft didn't happen (ADR-0052).
        let on_result = action
            .slot()
            .is_none_or(|slot| now.and_then(|now| now.kind().result_slot()) == Some(slot));
        self.pending = Some(Pending {
            screen: seen,
            slot: action.slot(),
            predicted,
            on_result,
            barrier: self.barriers_sent,
            sent: at,
            came: Came::Nothing,
        });
        Ok(())
    }

    /// The server sent the whole content of `window`.
    pub fn content(&mut self, window: WindowId) {
        self.corrected(|pending| pending.screen.window() == window);
    }

    /// The server sent slot `slot` of `window`.
    pub fn slot(&mut self, window: WindowId, slot: u16) {
        self.corrected(|pending| {
            pending.screen.window() == window && pending.predicted.binary_search(&slot).is_ok()
        });
    }

    /// The server sent the cursor's item.
    pub fn cursor(&mut self) {
        self.corrected(|_| true);
    }

    /// A correction came for the pending action, if `concerns` it.
    fn corrected(&mut self, concerns: impl FnOnce(&Pending) -> bool) {
        if let Some(pending) = &mut self.pending
            && concerns(pending)
        {
            pending.came = pending.came.max(Came::Correction);
        }
    }

    /// The server answered a barrier: the oldest one not yet answered.
    /// If it's the pending action's, its fate is known. A reply when no
    /// barrier is out answers nothing.
    pub fn barrier_answered(&mut self) -> Option<ClickResult> {
        if self.barriers_answered >= self.barriers_sent {
            return None;
        }
        self.barriers_answered += 1;
        let answered = self.barriers_answered;
        let pending = self
            .pending
            .take_if(|pending| pending.barrier == answered)?;
        let fate = match pending.came {
            Came::Nothing => ClickFate::Ok,
            Came::Correction if pending.on_result => ClickFate::Unclear,
            Came::Correction => ClickFate::Rejected,
            Came::Replaced => ClickFate::Replaced,
        };
        Some(ClickResult {
            screen: pending.screen,
            slot: pending.slot,
            fate,
        })
    }

    /// At `at`, the pending action is [`ClickFate::Unanswered`] if its
    /// barrier has had no reply for [`ClickGate::UNANSWERED_AFTER`]; its
    /// screen, if it's still open, takes nothing more. A clock that went
    /// back never makes it unanswered early.
    pub fn expire(&mut self, at: DateTime<Utc>) -> Option<ClickResult> {
        let pending = self
            .pending
            .take_if(|pending| time::elapsed(pending.sent, at) >= Self::UNANSWERED_AFTER)?;
        if self.open == Some(pending.screen) {
            self.stuck = Some(pending.screen);
        }
        Some(ClickResult {
            screen: pending.screen,
            slot: pending.slot,
            fate: ClickFate::Unanswered,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::menu::action::tests::{any_click_kind, any_item, lands_as_seen};
    use crate::script::menu::action::{ButtonPress, Click, ClickKind};
    use crate::script::menu::item::tests::item;
    use crate::script::menu::{Item, ScreenKind, StateId};
    use crate::time;
    use proptest::prelude::*;
    use rstest::rstest;

    fn at(ms: u64) -> DateTime<Utc> {
        time::add(
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            Duration::from_millis(ms),
        )
    }

    const CHEST: WindowId = WindowId::new(1);

    /// A gate with a chest open, and the chest as the hook sees it: a
    /// diamond in slot 0.
    fn open_chest() -> (ClickGate, Screen) {
        let mut gate = ClickGate::new();
        let id = gate.opened(CHEST);
        let screen = Screen::new(
            id,
            StateId::new(1),
            ScreenKind::Generic9x1,
            "Chest",
            [Some(item("diamond", 1))],
            None,
        );
        (gate, screen)
    }

    fn left(screen: &Screen, slot: u16) -> MenuAction {
        MenuAction::Click(Click::on(screen, ClickKind::Left, slot).unwrap())
    }

    /// Sends a left click on slot 0 at `ms`, predicted to change slot 0.
    fn click(gate: &mut ClickGate, screen: &Screen, ms: u64) -> Result<(), MenuRefusal> {
        gate.send(&left(screen, 0), Some(screen), &[0], at(ms))
    }

    fn fate(result: Option<ClickResult>) -> Option<ClickFate> {
        result.map(|result| result.fate)
    }

    #[test]
    fn a_click_with_nothing_before_its_reply_is_ok() {
        let (mut gate, screen) = open_chest();

        click(&mut gate, &screen, 0).unwrap();
        assert!(gate.is_pending());
        let result = gate.barrier_answered();

        assert_eq!(
            result,
            Some(ClickResult {
                screen: screen.id(),
                slot: Some(0),
                fate: ClickFate::Ok,
            })
        );
        assert!(!gate.is_pending());
    }

    #[rstest]
    #[case::the_whole_content(|gate: &mut ClickGate| gate.content(CHEST))]
    #[case::a_slot_it_predicted(|gate: &mut ClickGate| gate.slot(CHEST, 0))]
    #[case::the_cursor(|gate: &mut ClickGate| gate.cursor())]
    fn a_correction_before_the_reply_is_rejected(#[case] correction: fn(&mut ClickGate)) {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();

        correction(&mut gate);

        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Rejected));
    }

    #[rstest]
    #[case::a_slot_it_didnt_predict(|gate: &mut ClickGate| gate.slot(CHEST, 5))]
    #[case::another_windows_slot(|gate: &mut ClickGate| gate.slot(WindowId::INVENTORY, 0))]
    #[case::another_windows_content(|gate: &mut ClickGate| gate.content(WindowId::INVENTORY))]
    fn what_isnt_a_correction_of_the_click_leaves_it_ok(#[case] other: fn(&mut ClickGate)) {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();

        other(&mut gate);

        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Ok));
    }

    #[test]
    fn a_correction_after_the_reply_belongs_to_no_click() {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();
        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Ok));

        gate.content(CHEST);
        click(&mut gate, &screen, 10).unwrap();

        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Ok));
    }

    #[rstest]
    #[case::the_server_closed_it(|gate: &mut ClickGate| gate.closed())]
    #[case::another_screen_opened(|gate: &mut ClickGate| { gate.opened(WindowId::new(2)); })]
    #[case::the_same_window_opened_again(|gate: &mut ClickGate| { gate.opened(CHEST); })]
    #[case::closed_after_a_correction(|gate: &mut ClickGate| { gate.content(CHEST); gate.closed(); })]
    #[case::a_correction_after_the_close(|gate: &mut ClickGate| { gate.closed(); gate.cursor(); })]
    fn its_screen_gone_before_the_reply_is_replaced(#[case] gone: fn(&mut ClickGate)) {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();

        gone(&mut gate);

        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Replaced));
    }

    /// A gate with `kind` open, and the screen as the hook sees it: a
    /// diamond in every slot of its own.
    fn open(kind: ScreenKind) -> (ClickGate, Screen) {
        let mut gate = ClickGate::new();
        let id = gate.opened(CHEST);
        let items = (0..kind.own_slots()).map(|_| Some(item("diamond", 1)));
        let screen = Screen::new(id, StateId::new(1), kind, "Screen", items, None);
        (gate, screen)
    }

    #[rstest]
    #[case::a_villagers_trade(ScreenKind::Merchant, 2)]
    #[case::an_anvils_result(ScreenKind::Anvil, 2)]
    #[case::a_crafting_tables_result(ScreenKind::Crafting, 0)]
    #[case::the_inventorys_crafting_result(ScreenKind::Inventory, 0)]
    #[case::a_grindstones_result(ScreenKind::Grindstone, 2)]
    #[case::a_smithing_tables_result(ScreenKind::Smithing, 3)]
    #[case::a_stonecutters_result(ScreenKind::Stonecutter, 1)]
    #[case::a_looms_result(ScreenKind::Loom, 3)]
    #[case::a_cartography_tables_result(ScreenKind::CartographyTable, 2)]
    fn a_correction_of_a_click_on_a_result_slot_is_unclear_never_rejected(
        #[case] kind: ScreenKind,
        #[case] result: u16,
    ) {
        for click in [ClickKind::Left, ClickKind::Shift] {
            let (mut gate, screen) = open(kind);
            let action = MenuAction::Click(Click::on(&screen, click, result).unwrap());
            gate.send(&action, Some(&screen), &[result], at(0)).unwrap();

            gate.content(CHEST);

            assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Unclear));
        }
        assert_eq!(kind.result_slot(), Some(result));
    }

    #[rstest]
    #[case::a_merchants_payment(ScreenKind::Merchant, 0)]
    #[case::an_anvils_input(ScreenKind::Anvil, 0)]
    #[case::a_furnaces_output(ScreenKind::Furnace, 2)]
    #[case::a_chests_slot(ScreenKind::Generic9x3, 2)]
    fn a_correction_of_any_other_slot_is_rejected(#[case] kind: ScreenKind, #[case] slot: u16) {
        let (mut gate, screen) = open(kind);
        let action = MenuAction::Click(Click::on(&screen, ClickKind::Left, slot).unwrap());
        gate.send(&action, Some(&screen), &[slot], at(0)).unwrap();

        gate.content(CHEST);

        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Rejected));
    }

    #[test]
    fn a_click_on_a_result_slot_that_nothing_corrects_is_ok_and_a_replaced_one_replaced() {
        let (mut gate, screen) = open(ScreenKind::Merchant);
        let action = MenuAction::Click(Click::on(&screen, ClickKind::Shift, 2).unwrap());
        gate.send(&action, Some(&screen), &[2], at(0)).unwrap();
        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Ok));

        gate.send(&action, Some(&screen), &[2], at(10)).unwrap();
        gate.content(CHEST);
        gate.closed();
        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Replaced));
    }

    #[test]
    fn only_the_screens_that_make_something_have_a_result_slot() {
        let with: Vec<ScreenKind> = ScreenKind::ALL
            .into_iter()
            .filter(|kind| kind.result_slot().is_some())
            .collect();

        assert_eq!(
            with,
            [
                ScreenKind::Inventory,
                ScreenKind::Anvil,
                ScreenKind::Crafting,
                ScreenKind::Grindstone,
                ScreenKind::Loom,
                ScreenKind::Merchant,
                ScreenKind::Smithing,
                ScreenKind::CartographyTable,
                ScreenKind::Stonecutter,
            ]
        );
        for kind in with {
            assert!(kind.result_slot().unwrap() < kind.own_slots(), "{kind:?}");
        }
    }

    #[test]
    fn the_bots_own_close_replaces_a_pending_click() {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();

        gate.close(screen.id()).unwrap();

        assert_eq!(gate.open(), None);
        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Replaced));
    }

    #[test]
    fn a_second_click_before_the_first_ones_fate_is_click_pending() {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();

        assert_eq!(
            click(&mut gate, &screen, 50),
            Err(MenuRefusal::ClickPending)
        );
        // Not on another screen either.
        let other = gate.opened(WindowId::new(2));
        let other = Screen::new(other, StateId::new(1), ScreenKind::Generic9x1, "", [], None);
        assert_eq!(click(&mut gate, &other, 60), Err(MenuRefusal::ClickPending));
        // The first one's fate, then the next may go.
        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Replaced));
        assert_eq!(click(&mut gate, &other, 70), Ok(()));
    }

    #[test]
    fn no_reply_within_five_seconds_is_unanswered() {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 1000).unwrap();

        assert_eq!(gate.expire(at(5999)), None);
        let result = gate.expire(at(6000));

        assert_eq!(fate(result), Some(ClickFate::Unanswered));
        assert!(!gate.is_pending());
        assert_eq!(gate.expire(at(60_000)), None);
    }

    #[test]
    fn after_unanswered_the_screen_takes_nothing_until_it_opens_again() {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();
        gate.expire(at(5000)).unwrap();

        assert_eq!(
            click(&mut gate, &screen, 5100),
            Err(MenuRefusal::Unanswered)
        );
        let button = Screen::new(
            screen.id(),
            StateId::new(1),
            ScreenKind::Enchantment,
            "",
            [],
            None,
        );
        let press = MenuAction::Button(ButtonPress::on(&button, 0).unwrap());
        assert_eq!(
            gate.send(&press, Some(&button), &[], at(5200)),
            Err(MenuRefusal::Unanswered)
        );
        // Its late reply changes nothing.
        assert_eq!(gate.barrier_answered(), None);
        assert_eq!(
            click(&mut gate, &screen, 5300),
            Err(MenuRefusal::Unanswered)
        );
        // It can be closed, and the next opening takes clicks.
        gate.close(screen.id()).unwrap();
        let reopened = gate.opened(CHEST);
        let reopened = Screen::new(
            reopened,
            StateId::new(1),
            ScreenKind::Generic9x1,
            "Chest",
            [Some(item("diamond", 1))],
            None,
        );
        assert_eq!(click(&mut gate, &reopened, 5400), Ok(()));
    }

    #[test]
    fn a_late_reply_never_answers_the_next_click() {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 0).unwrap();
        gate.expire(at(5000)).unwrap();
        gate.closed();
        let reopened = gate.opened(CHEST);
        let reopened = Screen::new(
            reopened,
            StateId::new(1),
            ScreenKind::Generic9x1,
            "Chest",
            [Some(item("diamond", 1))],
            None,
        );
        click(&mut gate, &reopened, 6000).unwrap();

        // The first click's reply comes late: it's the first's, not this one's.
        assert_eq!(gate.barrier_answered(), None);
        assert!(gate.is_pending());
        // This one's own reply decides it.
        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Ok));
    }

    #[test]
    fn a_reply_nobody_asked_for_answers_nothing() {
        let (mut gate, screen) = open_chest();

        assert_eq!(gate.barrier_answered(), None);
        click(&mut gate, &screen, 0).unwrap();

        // The unasked one before didn't count for this click.
        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Ok));
    }

    #[test]
    fn a_clock_that_went_back_never_makes_a_click_unanswered_early() {
        let (mut gate, screen) = open_chest();
        click(&mut gate, &screen, 10_000).unwrap();

        assert_eq!(gate.expire(at(0)), None);
        assert!(gate.is_pending());
    }

    #[test]
    fn the_gate_sends_only_onto_the_screen_it_has_open() {
        let (mut gate, screen) = open_chest();
        gate.closed();

        assert_eq!(click(&mut gate, &screen, 0), Err(MenuRefusal::NoMenu));
        gate.opened(WindowId::new(2));
        // The hook's screen is offered as `now`, but the gate has another open.
        assert_eq!(
            click(&mut gate, &screen, 0),
            Err(MenuRefusal::ScreenChanged)
        );
        assert!(!gate.is_pending());
    }

    #[test]
    fn the_gate_applies_the_click_guard() {
        let (mut gate, screen) = open_chest();
        let moved_on = Screen::new(
            screen.id(),
            StateId::new(2),
            ScreenKind::Generic9x1,
            "Chest",
            [Some(item("diamond", 1))],
            None,
        );

        assert_eq!(
            gate.send(&left(&screen, 0), Some(&moved_on), &[0], at(0)),
            Err(MenuRefusal::ContentChanged)
        );
        assert_eq!(
            gate.send(&left(&screen, 0), None, &[0], at(0)),
            Err(MenuRefusal::NoMenu)
        );
        assert!(!gate.is_pending());
    }

    #[test]
    fn each_opening_is_a_new_screen_even_in_the_same_window() {
        let mut gate = ClickGate::new();

        let first = gate.opened(CHEST);
        let second = gate.opened(CHEST);

        assert_ne!(first, second);
        assert_eq!(first.window(), second.window());
        assert_eq!(gate.open(), Some(second));
        assert_eq!((first.opening(), second.opening()), (1, 2));
    }

    #[test]
    fn closing_names_the_screen_the_hook_saw() {
        let mut gate = ClickGate::new();
        assert_eq!(
            gate.close(ScreenId::new(CHEST, 1)),
            Err(MenuRefusal::NoMenu)
        );
        let first = gate.opened(CHEST);
        let second = gate.opened(CHEST);

        assert_eq!(gate.close(first), Err(MenuRefusal::ScreenChanged));
        assert_eq!(gate.open(), Some(second));
        assert_eq!(gate.close(second), Ok(()));
        assert_eq!(gate.open(), None);
    }

    #[test]
    fn the_inventory_opens_only_when_no_screen_is_open() {
        let mut gate = ClickGate::new();

        let inventory = gate.open_inventory().unwrap();

        assert_eq!(inventory.window(), WindowId::INVENTORY);
        assert_eq!(gate.open(), Some(inventory));
        assert_eq!(gate.open_inventory(), Err(MenuRefusal::ScreenOpen));
        gate.opened(CHEST);
        assert_eq!(gate.open_inventory(), Err(MenuRefusal::ScreenOpen));
    }

    #[test]
    fn a_button_has_no_slot_and_a_fate_like_a_click() {
        let mut gate = ClickGate::new();
        let id = gate.opened(CHEST);
        let table = Screen::new(id, StateId::new(1), ScreenKind::Enchantment, "", [], None);
        let press = MenuAction::Button(ButtonPress::on(&table, 2).unwrap());

        gate.send(&press, Some(&table), &[], at(0)).unwrap();
        // The server changes the item it enchants: not a slot it predicted.
        gate.slot(CHEST, 0);

        assert_eq!(
            gate.barrier_answered(),
            Some(ClickResult {
                screen: id,
                slot: None,
                fate: ClickFate::Ok,
            })
        );
    }

    #[rstest]
    #[case::the_whole_content(|gate: &mut ClickGate| gate.content(CHEST))]
    #[case::the_cursor(|gate: &mut ClickGate| gate.cursor())]
    fn a_correction_of_a_button_a_trade_or_a_name_is_unclear(
        #[case] correction: fn(&mut ClickGate),
    ) {
        // They predict no slot: what the server sends after one is its own
        // picture, which says nothing of a refusal (ADR-0052; on Paper, an
        // anvil's name a plugin took came back with one).
        let mut gate = ClickGate::new();
        let id = gate.opened(CHEST);
        let table = Screen::new(id, StateId::new(1), ScreenKind::Enchantment, "", [], None);
        let press = MenuAction::Button(ButtonPress::on(&table, 2).unwrap());
        gate.send(&press, Some(&table), &[], at(0)).unwrap();

        correction(&mut gate);

        assert_eq!(fate(gate.barrier_answered()), Some(ClickFate::Unclear));
    }

    #[rstest]
    #[case::ok(ClickFate::Ok, "ok")]
    #[case::unclear(ClickFate::Unclear, "unclear")]
    #[case::rejected(ClickFate::Rejected, "rejected")]
    #[case::replaced(ClickFate::Replaced, "replaced")]
    #[case::unanswered(ClickFate::Unanswered, "unanswered")]
    fn each_fate_reads_as_the_reference_names_it(#[case] fate: ClickFate, #[case] text: &str) {
        assert_eq!(fate.as_str(), text);
        assert_eq!(fate.to_string(), text);
        assert!(ClickFate::ALL.contains(&fate));
    }

    /// One thing that happens in a session, for the property below.
    #[derive(Debug, Clone)]
    enum Step {
        /// The server opens a screen in window 1 or 2.
        Open(i32),
        /// The server closes the screen.
        Close,
        /// The bot closes the screen it last looked at.
        CloseSeen,
        /// The server changes the open screen: a new state, and an item.
        Update {
            state: bool,
            slot: u16,
            item: Option<Item>,
            cursor: bool,
        },
        /// A hook looks at the open screen.
        Look,
        /// The hook that looked `back` looks ago clicks.
        Click {
            back: usize,
            kind: ClickKind,
            slot: u16,
        },
        /// The server sends the whole content, a slot or the cursor.
        Correction(u8, u16),
        /// The server answers a barrier.
        Reply,
        /// Time passes.
        Wait(u64),
    }

    fn any_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (1_i32..3).prop_map(Step::Open),
            Just(Step::Close),
            Just(Step::CloseSeen),
            (any::<bool>(), 0_u16..45, any_item(), any::<bool>()).prop_map(
                |(state, slot, item, cursor)| Step::Update {
                    state,
                    slot,
                    item,
                    cursor
                }
            ),
            Just(Step::Look),
            Just(Step::Look),
            (0_usize..4, any_click_kind(), 0_u16..45).prop_map(|(back, kind, slot)| Step::Click {
                back,
                kind,
                slot
            }),
            Just(Step::Click {
                back: 0,
                kind: ClickKind::Left,
                slot: 0
            }),
            (0_u8..3, 0_u16..45).prop_map(|(what, slot)| Step::Correction(what, slot)),
            Just(Step::Reply),
            Just(Step::Reply),
            (0_u64..3000).prop_map(Step::Wait),
        ]
    }

    /// A session the property drives: the gate, the screen the server has
    /// open, and the screens hooks looked at.
    struct Session {
        gate: ClickGate,
        now: Option<Screen>,
        looked: Vec<Screen>,
        ms: u64,
        sent: Vec<(MenuAction, u64)>,
        fates: Vec<ClickResult>,
        stuck: Vec<ScreenId>,
    }

    impl Session {
        fn new() -> Self {
            Self {
                gate: ClickGate::new(),
                now: None,
                looked: Vec::new(),
                ms: 0,
                sent: Vec::new(),
                fates: Vec::new(),
                stuck: Vec::new(),
            }
        }

        fn record(&mut self, result: Option<ClickResult>) {
            if let Some(result) = result {
                if result.fate == ClickFate::Unanswered && self.gate.open() == Some(result.screen) {
                    self.stuck.push(result.screen);
                }
                self.fates.push(result);
            }
        }

        fn step(&mut self, step: Step) -> Result<(), TestCaseError> {
            match step {
                Step::Open(window) => {
                    let id = self.gate.opened(WindowId::new(window));
                    self.now = Some(Screen::new(
                        id,
                        StateId::new(0),
                        ScreenKind::Generic9x1,
                        "",
                        [Some(item("diamond", 1)), Some(item("diamond", 1))],
                        None,
                    ));
                }
                Step::Close => {
                    self.gate.closed();
                    self.now = None;
                }
                Step::CloseSeen => {
                    if let Some(seen) = self.looked.last()
                        && self.gate.close(seen.id()).is_ok()
                    {
                        self.now = None;
                    }
                }
                Step::Update {
                    state,
                    slot,
                    item,
                    cursor,
                } => {
                    if let Some(now) = self.now.take() {
                        let mut items: Vec<Option<Item>> = (0..45)
                            .map(|index| now.slot(index).and_then(|slot| slot.item()).cloned())
                            .collect();
                        if let Some(at) = items.get_mut(usize::from(slot)) {
                            *at = item.clone();
                        }
                        let state = now.state().get() + u32::from(state);
                        let cursor = if cursor { item } else { now.cursor().cloned() };
                        self.now = Some(Screen::new(
                            now.id(),
                            StateId::new(state),
                            ScreenKind::Generic9x1,
                            "",
                            items,
                            cursor,
                        ));
                    }
                }
                Step::Look => {
                    if let Some(now) = &self.now {
                        self.looked.push(now.clone());
                    }
                }
                Step::Click { back, kind, slot } => {
                    let Some(seen) = self.looked.iter().rev().nth(back).cloned() else {
                        return Ok(());
                    };
                    let click = Click::on(&seen, kind, slot).unwrap();
                    let action = MenuAction::Click(click.clone());
                    let pending_before = self.gate.is_pending();
                    let verdict = self
                        .gate
                        .send(&action, self.now.as_ref(), &[slot], at(self.ms));
                    if verdict.is_ok() {
                        // Never two pending, never onto anything but what was seen,
                        // never onto a screen whose click stayed unanswered.
                        prop_assert!(!pending_before);
                        let now = self.now.as_ref().unwrap();
                        prop_assert!(lands_as_seen(&click, &seen, now));
                        prop_assert_eq!(self.gate.open(), Some(seen.id()));
                        prop_assert!(!self.stuck.contains(&seen.id()));
                        self.sent.push((action, self.ms));
                    }
                }
                Step::Correction(what, slot) => match what {
                    0 => self.gate.content(WindowId::new(1)),
                    1 => self.gate.slot(WindowId::new(1), slot),
                    _ => self.gate.cursor(),
                },
                Step::Reply => {
                    let result = self.gate.barrier_answered();
                    self.record(result);
                }
                Step::Wait(ms) => {
                    self.ms += ms;
                    let result = self.gate.expire(at(self.ms));
                    self.record(result);
                }
            }
            prop_assert!(self.fates.len() <= self.sent.len());
            Ok(())
        }
    }

    proptest! {
        /// Whatever the server sends and whenever, every action that goes
        /// out lands on what its hook saw, never while another is pending
        /// and never onto a screen whose click stayed unanswered; and each
        /// one gets exactly one fate, in the order they went out.
        #[test]
        fn every_click_lands_as_seen_and_gets_exactly_one_fate(
            steps in proptest::collection::vec(any_step(), 1..80),
        ) {
            let mut session = Session::new();
            for step in steps {
                session.step(step)?;
            }
            // Long enough for the last one to be unanswered at the latest.
            session.step(Step::Wait(5000))?;

            prop_assert_eq!(session.fates.len(), session.sent.len());
            prop_assert!(!session.gate.is_pending());
            for ((action, _), result) in session.sent.iter().zip(&session.fates) {
                prop_assert_eq!(result.screen, action.screen());
                prop_assert_eq!(result.slot, action.slot());
            }
        }
    }
}
