//! The conductor's book (ADR-0019, S65): which bots the last hello listed
//! and the stream gave, the specs the fleet couldn't apply yet, and the
//! removals waiting for their `Removed`.

use std::collections::{BTreeMap, BTreeSet};

use fleet_core::bot::{BotSpec, StickyState};
use fleet_core::id::BotId;

/// What a pending apply waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wait {
    /// The next retry tick, or any `Removed`.
    Retry,
    /// Any `Removed`: the fleet is at capacity, or the account is in use.
    Removed,
}

/// A spec the fleet couldn't apply yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pending {
    /// The spec.
    pub(crate) spec: BotSpec,
    /// Its restore, kept through an `Update`.
    pub(crate) restore: Option<StickyState>,
    /// What it waits for.
    pub(crate) wait: Wait,
}

/// The book; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct Book {
    /// The most pending applies: `max_bots`.
    cap: usize,
    /// The bots the last hello listed, until its stream's reconcile.
    listed: BTreeSet<BotId>,
    /// The bots the current stream gave.
    told: BTreeSet<BotId>,
    /// The specs waiting to be applied, by bot.
    pending: BTreeMap<BotId, Pending>,
    /// The bots whose removal the fleet accepted, until their `Removed`.
    removing: BTreeSet<BotId>,
    /// The bots whose removal the fleet was too busy for: tried again on
    /// the tick.
    unremoved: BTreeSet<BotId>,
}

impl Book {
    /// An empty book, at most `cap` pending applies.
    pub(crate) const fn new(cap: usize) -> Self {
        Self {
            cap,
            listed: BTreeSet::new(),
            told: BTreeSet::new(),
            pending: BTreeMap::new(),
            removing: BTreeSet::new(),
            unremoved: BTreeSet::new(),
        }
    }

    /// A hello listed `listed`: the stream starts over, and what was
    /// pending, told or busy to remove goes, since its reconcile resends what
    /// still holds and takes away what doesn't: a busy removal kept past it
    /// could stop a bot the reconcile gives again.
    pub(crate) fn barrier(&mut self, listed: BTreeSet<BotId>) {
        self.listed = listed;
        self.told.clear();
        self.pending.clear();
        self.unremoved.clear();
    }

    /// The bots a reconcile giving `given` takes away, in ID order: those the
    /// hello listed or the stream told, that it leaves out. The stream has
    /// told `given` from here on, and a busy removal of a bot it gives goes.
    pub(crate) fn reconcile(&mut self, given: &BTreeSet<BotId>) -> Vec<BotId> {
        let leaving = self
            .listed
            .union(&self.told)
            .filter(|bot| !given.contains(bot))
            .copied()
            .collect();
        self.listed.clear();
        self.told.clone_from(given);
        self.pending.retain(|bot, _| given.contains(bot));
        self.unremoved.retain(|bot| !given.contains(bot));
        leaving
    }

    /// The stream gave `bot`: a busy removal of it goes.
    pub(crate) fn tell(&mut self, bot: BotId) {
        self.told.insert(bot);
        self.unremoved.remove(&bot);
    }

    /// The stream took `bot` away: it's no longer told, and its pending
    /// apply goes.
    pub(crate) fn untell(&mut self, bot: BotId) {
        self.told.remove(&bot);
        self.listed.remove(&bot);
        self.pending.remove(&bot);
    }

    /// Whether the stream told `bot`.
    pub(crate) fn told(&self, bot: BotId) -> bool {
        self.told.contains(&bot)
    }

    /// Keeps `spec` pending; false when the book is full of other bots.
    pub(crate) fn hold(&mut self, spec: BotSpec, restore: Option<StickyState>, wait: Wait) -> bool {
        let bot = spec.id;
        if !self.pending.contains_key(&bot) && self.pending.len() >= self.cap {
            return false;
        }
        self.pending.insert(
            bot,
            Pending {
                spec,
                restore,
                wait,
            },
        );
        true
    }

    /// `bot`'s pending apply, taken out.
    pub(crate) fn take_pending(&mut self, bot: BotId) -> Option<Pending> {
        self.pending.remove(&bot)
    }

    /// What `bot`'s pending apply waits for, if it has one.
    pub(crate) fn pending_wait(&self, bot: BotId) -> Option<Wait> {
        self.pending.get(&bot).map(|pending| pending.wait)
    }

    /// `bot`'s pending restore, if it has a pending apply.
    pub(crate) fn pending_restore(&self, bot: BotId) -> Option<StickyState> {
        self.pending.get(&bot).and_then(|pending| pending.restore)
    }

    /// The pending applies to try now, taken out in ID order: those waiting
    /// for the tick, and with `removed` those waiting for a `Removed` too.
    pub(crate) fn due(&mut self, removed: bool) -> Vec<Pending> {
        let due: Vec<BotId> = self
            .pending
            .iter()
            .filter(|(_, pending)| removed || pending.wait == Wait::Retry)
            .map(|(bot, _)| *bot)
            .collect();
        due.iter()
            .filter_map(|bot| self.pending.remove(bot))
            .collect()
    }

    /// The fleet accepted `bot`'s removal.
    pub(crate) fn removing(&mut self, bot: BotId) {
        self.removing.insert(bot);
    }

    /// `bot`'s `Removed` came: whether its removal was waited for.
    pub(crate) fn removed(&mut self, bot: BotId) -> bool {
        self.unremoved.remove(&bot);
        self.removing.remove(&bot)
    }

    /// The bots being removed that `present` no longer lists: removed
    /// meanwhile, their `Removed` lost in a lag; taken out.
    pub(crate) fn vanished(&mut self, present: &BTreeSet<BotId>) -> Vec<BotId> {
        let vanished: Vec<BotId> = self.removing.difference(present).copied().collect();
        for bot in &vanished {
            self.removing.remove(bot);
        }
        vanished
    }

    /// The fleet was too busy to remove `bot`: tried again on the tick.
    pub(crate) fn unremoved(&mut self, bot: BotId) {
        self.unremoved.insert(bot);
    }

    /// The removals to try again, taken out.
    pub(crate) fn take_unremoved(&mut self) -> Vec<BotId> {
        core::mem::take(&mut self.unremoved).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_core::bot::{BotAccount, DesiredRunState};
    use fleet_core::disconnect::ConflictTexts;
    use fleet_core::mode::ModePreset;

    use super::*;

    fn bot(n: u8) -> BotId {
        BotId::new_v7(DateTime::UNIX_EPOCH, [n; 10]).unwrap()
    }

    fn spec(n: u8) -> BotSpec {
        BotSpec {
            id: bot(n),
            account: BotAccount::Offline(format!("AfkBot{n}").parse().unwrap()),
            server: "localhost".try_into().unwrap(),
            mode: ModePreset::Afk.definition(),
            desired: DesiredRunState::Running,
            conflict_texts: ConflictTexts::default(),
            chat_rate: fleet_core::chat::ChatRate::default(),
        }
    }

    fn ids(pending: &[Pending]) -> Vec<BotId> {
        pending.iter().map(|pending| pending.spec.id).collect()
    }

    #[test]
    fn a_reconcile_takes_away_what_the_hello_listed_or_the_stream_told_and_it_omits() {
        let mut book = Book::new(8);
        book.barrier([bot(1), bot(2)].into());
        book.tell(bot(3));

        let leaving = book.reconcile(&[bot(2), bot(4)].into());

        assert_eq!(leaving, [bot(1), bot(3)]);
        assert!(book.told(bot(2)) && book.told(bot(4)));
        assert!(!book.told(bot(1)) && !book.told(bot(3)));
        assert_eq!(
            book.reconcile(&BTreeSet::new()),
            [bot(2), bot(4)],
            "told since"
        );
    }

    #[test]
    fn the_barrier_clears_what_was_pending_and_told() {
        let mut book = Book::new(8);
        book.tell(bot(1));
        assert!(book.hold(spec(1), None, Wait::Retry));

        book.barrier(BTreeSet::new());

        assert!(!book.told(bot(1)));
        assert_eq!(book.due(true), []);
    }

    #[test]
    fn a_pending_apply_keeps_its_restore_and_waits_for_its_kind() {
        let mut book = Book::new(8);
        let restore = Some(StickyState::Failed(fleet_core::bot::FailReason::Auth));
        assert!(book.hold(spec(1), restore, Wait::Retry));
        assert!(book.hold(spec(2), None, Wait::Removed));

        let restored = book.pending_restore(bot(1));
        let at_tick = book.due(false);
        let after_removed = book.due(true);

        assert_eq!(restored, restore);
        assert_eq!(ids(&at_tick), [bot(1)]);
        assert_eq!(ids(&after_removed), [bot(2)]);
    }

    #[test]
    fn a_full_book_refuses_a_new_bot_but_replaces_a_known_ones_spec() {
        let mut book = Book::new(2);
        assert!(book.hold(spec(1), None, Wait::Retry));
        assert!(book.hold(spec(2), None, Wait::Retry));

        let new = book.hold(spec(3), None, Wait::Retry);
        let known = book.hold(spec(2), None, Wait::Removed);

        assert!(!new);
        assert!(known);
        assert_eq!(ids(&book.due(true)), [bot(1), bot(2)]);
    }

    #[test]
    fn untelling_a_bot_drops_its_pending_apply() {
        let mut book = Book::new(8);
        book.tell(bot(1));
        assert!(book.hold(spec(1), None, Wait::Retry));

        book.untell(bot(1));

        assert!(!book.told(bot(1)));
        assert_eq!(book.take_pending(bot(1)), None);
    }

    #[test]
    fn a_removal_is_waited_for_until_its_removed_or_its_bot_vanishes() {
        let mut book = Book::new(8);
        book.removing(bot(1));
        book.removing(bot(2));
        book.removing(bot(3));

        let waited = book.removed(bot(1));
        let unknown = book.removed(bot(9));
        let vanished = book.vanished(&[bot(3)].into());

        assert!(waited);
        assert!(!unknown);
        assert_eq!(vanished, [bot(2)]);
        assert_eq!(book.vanished(&BTreeSet::new()), [bot(3)]);
    }

    #[test]
    fn a_busy_removal_is_kept_for_the_tick_once() {
        let mut book = Book::new(8);
        book.unremoved(bot(2));
        book.unremoved(bot(1));
        book.unremoved(bot(2));

        assert_eq!(book.take_unremoved(), [bot(1), bot(2)]);
        assert_eq!(book.take_unremoved(), []);
    }

    #[test]
    fn a_busy_removal_is_dropped_by_a_hello_and_by_the_stream_giving_its_bot() {
        let mut book = Book::new(8);
        book.unremoved(bot(1));
        book.barrier([bot(1)].into());
        assert_eq!(book.take_unremoved(), [], "the reconcile decides");

        book.unremoved(bot(2));
        book.unremoved(bot(3));
        let _ = book.reconcile(&[bot(2)].into());
        book.tell(bot(3));

        assert_eq!(book.take_unremoved(), [], "given again");
    }

    #[test]
    fn whatever_the_order_vanished_bots_are_exactly_the_waited_ones_not_present() {
        // A sweep over every subset of four bots waited for and present.
        for waited in 0..16_u8 {
            for present in 0..16_u8 {
                let mut book = Book::new(8);
                let set = |mask: u8| -> BTreeSet<BotId> {
                    (0..4)
                        .filter(|n| mask & (1 << n) != 0)
                        .map(|n| bot(n + 1))
                        .collect()
                };
                for waiting in set(waited) {
                    book.removing(waiting);
                }

                let vanished: BTreeSet<BotId> = book.vanished(&set(present)).into_iter().collect();

                let expected: BTreeSet<BotId> =
                    set(waited).difference(&set(present)).copied().collect();
                assert_eq!(vanished, expected, "{waited} {present}");
            }
        }
    }
}
