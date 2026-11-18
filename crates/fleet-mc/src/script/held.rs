//! [`Held`]: which keys and buttons a scripted mode holds down, and what
//! that means for azalea's key state (Plan.md P15.10; ADR-0036, D9).

use azalea::WalkDirection;
use azalea::inventory::ItemStack;
// The `Component` derive names its crate as `bevy_ecs`, which fleet-mc only
// has through azalea.
use azalea::ecs as bevy_ecs;
use azalea::ecs::component::Component;
use fleet_core::script::Key;

/// The seven keys that can be down: the four directions, jump, sneak and
/// sprint, a bit each. The other three only tap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct KeySet(u8);

impl KeySet {
    /// The key's bit; none for a key that only taps.
    const fn bit(key: Key) -> u8 {
        match key {
            Key::Forward => 1,
            Key::Back => 1 << 1,
            Key::Left => 1 << 2,
            Key::Right => 1 << 3,
            Key::Jump => 1 << 4,
            Key::Sneak => 1 << 5,
            Key::Sprint => 1 << 6,
            Key::Drop | Key::DropStack | Key::SwapOffhand => 0,
        }
    }

    /// Puts `key` down or up. Returns whether that changed anything; a key
    /// that only taps is never in the set.
    pub(crate) const fn set(&mut self, key: Key, down: bool) -> bool {
        let before = self.0;
        if down {
            self.0 |= Self::bit(key);
        } else {
            self.0 &= !Self::bit(key);
        }
        self.0 != before
    }

    /// Whether `key` is down.
    #[must_use]
    pub(crate) const fn has(self, key: Key) -> bool {
        self.0 & Self::bit(key) != 0
    }

    /// Whether no key is down.
    #[must_use]
    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether a key that moves the bot is down: what the leash lets go of.
    #[must_use]
    pub(crate) fn moves(self) -> bool {
        Key::ALL
            .into_iter()
            .any(|key| key.is_movement() && self.has(key))
    }

    /// Lets go of every key that moves the bot. Returns whether one was
    /// down.
    pub(crate) fn release_movement(&mut self) -> bool {
        let moved = self.moves();
        for key in Key::ALL {
            if key.is_movement() {
                self.set(key, false);
            }
        }
        moved
    }

    /// The keys of both sets.
    #[must_use]
    pub(crate) const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// What azalea's own key state is for a set of keys: what its `walk`,
/// `sprint`, `set_jumping` and `set_crouching` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyState {
    /// Where the direction keys lead; opposite keys cancel.
    pub(crate) walk: WalkDirection,
    /// Whether the sprint key is down. azalea sprints only while the bot
    /// also walks forward, as a vanilla client does.
    pub(crate) sprint: bool,
    /// Whether the jump key is down.
    pub(crate) jump: bool,
    /// Whether the sneak key is down.
    pub(crate) crouch: bool,
}

impl From<KeySet> for KeyState {
    fn from(keys: KeySet) -> Self {
        // −1, 0 or 1 along each axis; a key and its opposite give 0.
        let ahead = i8::from(keys.has(Key::Forward)) - i8::from(keys.has(Key::Back));
        let aside = i8::from(keys.has(Key::Right)) - i8::from(keys.has(Key::Left));
        let walk = match (ahead, aside) {
            (1, 0) => WalkDirection::Forward,
            (1, 1) => WalkDirection::ForwardRight,
            (1, -1) => WalkDirection::ForwardLeft,
            (-1, 0) => WalkDirection::Backward,
            (-1, 1) => WalkDirection::BackwardRight,
            (-1, -1) => WalkDirection::BackwardLeft,
            (0, 1) => WalkDirection::Right,
            (0, -1) => WalkDirection::Left,
            _ => WalkDirection::None,
        };
        Self {
            walk,
            sprint: keys.has(Key::Sprint),
            jump: keys.has(Key::Jump),
            crouch: keys.has(Key::Sneak),
        }
    }
}

/// The right button, held down.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RightHold {
    /// Whether the hold ends by itself once the held item is used up once.
    pub(crate) until_used: bool,
    /// How many game ticks ago the last use went out.
    pub(crate) since_use: u32,
    /// What the bot held when that use went out.
    pub(crate) used: ItemStack,
}

/// On the bot: what a scripted mode holds down. A new session's bot holds
/// nothing, and so does a bot the server just put into another world.
///
/// It's the one place that says which keys are down. A game action only
/// changes it, and marks it changed; the session's tick writes azalea's key
/// state from it before azalea's physics run, so a key and the leash never
/// disagree about what's held.
#[derive(Component, Debug, Clone, PartialEq, Default)]
pub(crate) struct Held {
    /// The keys held down.
    pub(crate) keys: KeySet,
    /// The keys tapped: down until the game tick that saw them is over.
    pub(crate) taps: KeySet,
    /// Whether the left button is down.
    pub(crate) left: bool,
    /// The right button, while it's down.
    pub(crate) right: Option<RightHold>,
    /// Whether the keys changed since azalea's key state was last written.
    pub(crate) changed: bool,
}

impl Held {
    /// What azalea's key state should be: the held keys and the tapped
    /// ones.
    #[must_use]
    pub(crate) fn key_state(&self) -> KeyState {
        KeyState::from(self.keys.with(self.taps))
    }

    /// Puts a key down, to stay.
    pub(crate) fn press(&mut self, key: Key) {
        self.changed |= self.keys.set(key, true);
    }

    /// Puts a key down for one game tick.
    pub(crate) fn tap(&mut self, key: Key) {
        self.changed |= self.taps.set(key, true);
    }

    /// Lets go of a key, held or tapped.
    pub(crate) fn release(&mut self, key: Key) {
        let held = self.keys.set(key, false);
        let tapped = self.taps.set(key, false);
        self.changed |= held || tapped;
    }

    /// Lets go of every key; the buttons stay. azalea's key state is
    /// written again whatever was held: a sneak a step mode left behind,
    /// set past this component, ends too.
    pub(crate) fn release_keys(&mut self) {
        self.changed = true;
        self.keys = KeySet::default();
        self.taps = KeySet::default();
    }

    /// Lets go of every key that moves the bot, held or tapped. Returns
    /// whether one was down.
    pub(crate) fn release_movement(&mut self) -> bool {
        let held = self.keys.release_movement();
        let tapped = self.taps.release_movement();
        self.changed |= held || tapped;
        held || tapped
    }

    /// The game tick that saw the taps is over: they go up. Returns
    /// whether one was down.
    pub(crate) fn end_taps(&mut self) -> bool {
        let tapped = !self.taps.is_empty();
        self.taps = KeySet::default();
        self.changed |= tapped;
        tapped
    }

    /// Lets go of everything: every key and both buttons.
    pub(crate) fn release_all(&mut self) {
        self.release_keys();
        self.left = false;
        self.right = None;
    }

    /// Whether anything is down.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_anything_down(&self) -> bool {
        !self.keys.is_empty() || !self.taps.is_empty() || self.left || self.right.is_some()
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn keys(down: &[Key]) -> KeySet {
        let mut set = KeySet::default();
        for key in down {
            assert!(set.set(*key, true), "{key:?}");
        }
        set
    }

    #[rstest]
    #[case::nothing(&[], WalkDirection::None)]
    #[case::forward(&[Key::Forward], WalkDirection::Forward)]
    #[case::back(&[Key::Back], WalkDirection::Backward)]
    #[case::left(&[Key::Left], WalkDirection::Left)]
    #[case::right(&[Key::Right], WalkDirection::Right)]
    #[case::forward_left(&[Key::Forward, Key::Left], WalkDirection::ForwardLeft)]
    #[case::forward_right(&[Key::Forward, Key::Right], WalkDirection::ForwardRight)]
    #[case::back_left(&[Key::Back, Key::Left], WalkDirection::BackwardLeft)]
    #[case::back_right(&[Key::Back, Key::Right], WalkDirection::BackwardRight)]
    #[case::forward_and_back_cancel(&[Key::Forward, Key::Back], WalkDirection::None)]
    #[case::left_and_right_cancel(&[Key::Left, Key::Right], WalkDirection::None)]
    #[case::three_keys_leave_the_third(
        &[Key::Forward, Key::Back, Key::Left],
        WalkDirection::Left
    )]
    #[case::all_four_cancel(
        &[Key::Forward, Key::Back, Key::Left, Key::Right],
        WalkDirection::None
    )]
    #[case::jump_sneak_and_sprint_walk_nowhere(
        &[Key::Jump, Key::Sneak, Key::Sprint],
        WalkDirection::None
    )]
    fn the_direction_keys_lead_where_a_keyboards_do(
        #[case] down: &[Key],
        #[case] walk: WalkDirection,
    ) {
        assert_eq!(KeyState::from(keys(down)).walk, walk);
    }

    #[test]
    fn jump_sneak_and_sprint_are_their_own_states() {
        let state = KeyState::from(keys(&[Key::Jump, Key::Sneak, Key::Sprint, Key::Forward]));

        assert_eq!(
            state,
            KeyState {
                walk: WalkDirection::Forward,
                sprint: true,
                jump: true,
                crouch: true,
            }
        );
        assert_eq!(
            KeyState::from(KeySet::default()),
            KeyState {
                walk: WalkDirection::None,
                sprint: false,
                jump: false,
                crouch: false,
            }
        );
    }

    #[test]
    fn a_key_that_only_taps_is_never_in_the_set() {
        let mut set = KeySet::default();

        for key in [Key::Drop, Key::DropStack, Key::SwapOffhand] {
            assert!(!set.set(key, true));
            assert!(!set.has(key));
        }
        assert!(set.is_empty());
    }

    #[test]
    fn every_key_that_can_be_held_is_one_of_the_set() {
        for key in Key::ALL {
            let mut set = KeySet::default();

            assert_eq!(set.set(key, true), key.can_be_held(), "{key:?}");
            assert_eq!(set.has(key), key.can_be_held(), "{key:?}");
        }
    }

    #[test]
    fn a_new_bot_holds_nothing() {
        let held = Held::default();

        assert!(!held.is_anything_down());
        assert!(!held.changed);
        assert_eq!(held.key_state(), KeyState::from(KeySet::default()));
    }

    #[test]
    fn a_press_stays_and_marks_the_keys_changed_once() {
        let mut held = Held::default();

        held.press(Key::Forward);
        assert!(held.changed);
        held.changed = false;
        // Down already: nothing changed.
        held.press(Key::Forward);

        assert!(!held.changed);
        assert_eq!(held.key_state().walk, WalkDirection::Forward);
    }

    #[test]
    fn a_tap_is_down_until_its_tick_is_over() {
        let mut held = Held::default();

        held.tap(Key::Sneak);
        let during = held.key_state();
        held.changed = false;
        let ended = held.end_taps();

        assert!(during.crouch);
        assert!(ended && held.changed);
        assert!(!held.key_state().crouch);
        assert!(!held.end_taps(), "nothing was tapped any more");
    }

    #[test]
    fn a_tap_of_a_held_key_leaves_it_held() {
        let mut held = Held::default();
        held.press(Key::Forward);

        held.tap(Key::Forward);
        held.end_taps();

        assert_eq!(held.key_state().walk, WalkDirection::Forward);
    }

    #[test]
    fn a_release_lets_go_of_a_key_held_or_tapped() {
        let mut held = Held::default();
        held.press(Key::Left);
        held.tap(Key::Jump);
        held.changed = false;

        held.release(Key::Left);
        held.release(Key::Jump);

        assert!(held.changed);
        assert!(!held.is_anything_down());
        held.changed = false;
        // Up already: letting go again changes nothing.
        held.release(Key::Left);
        assert!(!held.changed);
    }

    #[test]
    fn the_leash_lets_go_of_what_moves_the_bot_and_leaves_the_rest() {
        let mut held = Held::default();
        for key in [Key::Forward, Key::Sprint, Key::Jump, Key::Sneak] {
            held.press(key);
        }
        held.tap(Key::Left);
        held.left = true;
        held.changed = false;

        let let_go = held.release_movement();

        assert!(let_go && held.changed);
        assert_eq!(
            held.key_state(),
            KeyState {
                walk: WalkDirection::None,
                sprint: false,
                jump: false,
                crouch: true,
            }
        );
        assert!(held.left);
        assert!(!held.release_movement(), "nothing that moves is down");
    }

    #[test]
    fn releasing_the_keys_leaves_the_buttons_and_releasing_all_doesnt() {
        let mut held = Held::default();
        held.press(Key::Forward);
        held.tap(Key::Sneak);
        held.left = true;
        held.right = Some(RightHold {
            until_used: false,
            since_use: 0,
            used: ItemStack::Empty,
        });

        held.release_keys();
        let after_the_keys = held.clone();
        held.release_all();

        assert!(after_the_keys.keys.is_empty() && after_the_keys.taps.is_empty());
        assert!(after_the_keys.left && after_the_keys.right.is_some());
        assert!(!held.is_anything_down());
        // Doing it twice is as good as once.
        let once = held.clone();
        held.release_all();
        assert_eq!(held, once);
    }
}
