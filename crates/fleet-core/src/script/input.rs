//! [`Input`]: the keys and mouse buttons of a vanilla client, and
//! [`HoldTracker`], which of them a script holds down.

use core::time::Duration;

use chrono::{DateTime, Utc};

use super::limits;
use crate::time;

/// A key of a vanilla keyboard, by what it does (ADR-0036, D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// W: walk forward.
    Forward,
    /// S: walk back.
    Back,
    /// A: strafe left.
    Left,
    /// D: strafe right.
    Right,
    /// Space.
    Jump,
    /// Shift.
    Sneak,
    /// Ctrl.
    Sprint,
    /// Q: drop one of the held item.
    Drop,
    /// Ctrl+Q: drop the held stack.
    DropStack,
    /// F: swap the held item with the offhand's.
    SwapOffhand,
}

impl Key {
    /// Every key, in a fixed order.
    pub const ALL: [Self; 10] = [
        Self::Forward,
        Self::Back,
        Self::Left,
        Self::Right,
        Self::Jump,
        Self::Sneak,
        Self::Sprint,
        Self::Drop,
        Self::DropStack,
        Self::SwapOffhand,
    ];

    /// The key's name in a script, after `keys.`: `forward`, `drop_stack`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Back => "back",
            Self::Left => "left",
            Self::Right => "right",
            Self::Jump => "jump",
            Self::Sneak => "sneak",
            Self::Sprint => "sprint",
            Self::Drop => "drop",
            Self::DropStack => "drop_stack",
            Self::SwapOffhand => "swap_offhand",
        }
    }

    /// The key with this name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.name() == name)
    }

    /// Whether the key can be held down. The three that act once (drop,
    /// drop a stack, swap) only tap.
    #[must_use]
    pub const fn can_be_held(self) -> bool {
        match self {
            Self::Forward
            | Self::Back
            | Self::Left
            | Self::Right
            | Self::Jump
            | Self::Sneak
            | Self::Sprint => true,
            Self::Drop | Self::DropStack | Self::SwapOffhand => false,
        }
    }

    /// Whether the key moves the bot: the four directions, jump and sprint.
    /// These are what the leash lets go of. Sneaking alone moves nothing.
    #[must_use]
    pub const fn is_movement(self) -> bool {
        match self {
            Self::Forward | Self::Back | Self::Left | Self::Right | Self::Jump | Self::Sprint => {
                true
            }
            Self::Sneak | Self::Drop | Self::DropStack | Self::SwapOffhand => false,
        }
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Attack, and held on a block, mine.
    Left,
    /// Use.
    Right,
}

impl MouseButton {
    /// Both buttons, in a fixed order.
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];

    /// The button's name in a script, after `mouse.`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// Something a virtual player presses: a key or a mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Input {
    /// A key.
    Key(Key),
    /// A mouse button.
    Mouse(MouseButton),
}

impl Input {
    /// Whether the input can be held down: both buttons, and the keys that
    /// can.
    #[must_use]
    pub const fn can_be_held(self) -> bool {
        match self {
            Self::Key(key) => key.can_be_held(),
            Self::Mouse(_) => true,
        }
    }

    /// Whether the input is a key that moves the bot.
    #[must_use]
    pub const fn is_movement(self) -> bool {
        match self {
            Self::Key(key) => key.is_movement(),
            Self::Mouse(_) => false,
        }
    }
}

/// Why an input can't be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HoldError {
    /// The key only taps: drop, drop a stack, swap.
    #[error("this key can only be tapped")]
    TapOnly,
    /// A `hold_for` outside [`limits::MIN_HOLD_FOR`] to
    /// [`limits::MAX_HOLD_FOR`].
    #[error("an input is held for 50 ms to 10 minutes")]
    Length,
}

/// Whether a hold pressed the input or found it down already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressed {
    /// The input was up, and is down now.
    Now,
    /// The input was down already. Its release time is the new hold's.
    Already,
}

/// Which inputs a script holds down, and until when (ADR-0036, D4, D9).
///
/// It's the script's side of the truth: what `held` answers and what the
/// runtime must let go of. A hold lasts until a release, or with
/// [`hold_for`](Self::hold_for) until its time is up, which
/// [`due`](Self::due) reports. Holding an input again replaces how long it
/// stays down and presses nothing twice.
///
/// **Everything is released whenever a script stops,** and before each hook
/// that puts the bot somewhere new: [`release_all`](Self::release_all) says
/// what was down, and afterwards nothing is.
///
/// The tracker holds no clock: `hold_for` and `due` take `now` (ADR-0010).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoldTracker {
    /// The inputs that are down, in the order they were pressed.
    held: Vec<Held>,
}

/// An input that's down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    input: Input,
    /// When it's let go by itself; `None` until a release.
    until: Option<DateTime<Utc>>,
}

impl HoldTracker {
    /// A tracker with nothing held, as at a bot's start.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Holds `input` down until it's released.
    ///
    /// # Errors
    /// [`HoldError::TapOnly`] for a key that only taps; nothing changes.
    pub fn hold(&mut self, input: Input) -> Result<Pressed, HoldError> {
        if !input.can_be_held() {
            return Err(HoldError::TapOnly);
        }
        Ok(self.press(input, None))
    }

    /// Holds `input` down for `length` from `now`, then lets go by itself.
    ///
    /// # Errors
    /// [`HoldError::TapOnly`] for a key that only taps, then
    /// [`HoldError::Length`] for a length outside 50 ms to 10 minutes;
    /// nothing changes.
    pub fn hold_for(
        &mut self,
        input: Input,
        length: Duration,
        now: DateTime<Utc>,
    ) -> Result<Pressed, HoldError> {
        if !input.can_be_held() {
            return Err(HoldError::TapOnly);
        }
        let until = deadline(length, now)?;
        Ok(self.press(input, Some(until)))
    }

    /// Puts `input` down until `until`, or sets that time if it's down.
    fn press(&mut self, input: Input, until: Option<DateTime<Utc>>) -> Pressed {
        if let Some(held) = self.held.iter_mut().find(|held| held.input == input) {
            held.until = until;
            Pressed::Already
        } else {
            self.held.push(Held { input, until });
            Pressed::Now
        }
    }

    /// Lets go of the inputs `goes` picks and returns them, in the order
    /// they were pressed.
    fn let_go(&mut self, mut goes: impl FnMut(&Held) -> bool) -> Vec<Input> {
        let (gone, kept): (Vec<Held>, Vec<Held>) = core::mem::take(&mut self.held)
            .into_iter()
            .partition(|held| goes(held));
        self.held = kept;
        gone.into_iter().map(|held| held.input).collect()
    }

    /// Lets go of `input`. Returns whether it was down. A release is never
    /// refused.
    pub fn release(&mut self, input: Input) -> bool {
        !self.let_go(|held| held.input == input).is_empty()
    }

    /// Lets go of everything, and returns what was down, in the order it
    /// was pressed.
    pub fn release_all(&mut self) -> Vec<Input> {
        self.let_go(|_| true)
    }

    /// Lets go of every key, not the mouse buttons, and returns what was
    /// down: a script's `keys.release_all()`.
    pub fn release_keys(&mut self) -> Vec<Input> {
        self.let_go(|held| matches!(held.input, Input::Key(_)))
    }

    /// Lets go of every key that moves the bot and returns what was down:
    /// what the leash does at its end (ADR-0036, D10).
    pub fn release_movement(&mut self) -> Vec<Input> {
        self.let_go(|held| held.input.is_movement())
    }

    /// Lets go of the inputs whose time is up at `now` and returns them, in
    /// the order they were pressed.
    pub fn due(&mut self, now: DateTime<Utc>) -> Vec<Input> {
        self.let_go(|held| held.until.is_some_and(|until| until <= now))
    }

    /// When the next input is let go by itself, for the runtime's timer.
    #[must_use]
    pub fn next_due(&self) -> Option<DateTime<Utc>> {
        self.held.iter().filter_map(|held| held.until).min()
    }

    /// Whether `input` is down.
    #[must_use]
    pub fn is_held(&self, input: Input) -> bool {
        self.held.iter().any(|held| held.input == input)
    }

    /// The inputs that are down, in the order they were pressed.
    pub fn held(&self) -> impl Iterator<Item = Input> {
        self.held.iter().map(|held| held.input)
    }

    /// The inputs that are down with when each is let go by itself (`None`
    /// until a release), in the order they were pressed: what a script held
    /// when a screen made the session let go of it (ADR-0052).
    pub fn holds(&self) -> impl Iterator<Item = (Input, Option<DateTime<Utc>>)> {
        self.held.iter().map(|held| (held.input, held.until))
    }
}

/// When a hold of `length` from `now` ends, if `length` is one a script may
/// ask for.
fn deadline(length: Duration, now: DateTime<Utc>) -> Result<DateTime<Utc>, HoldError> {
    if (limits::MIN_HOLD_FOR..=limits::MAX_HOLD_FOR).contains(&length) {
        Ok(time::add(now, length))
    } else {
        Err(HoldError::Length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    const FORWARD: Input = Input::Key(Key::Forward);
    const JUMP: Input = Input::Key(Key::Jump);
    const SNEAK: Input = Input::Key(Key::Sneak);
    const SPRINT: Input = Input::Key(Key::Sprint);
    const LEFT_BUTTON: Input = Input::Mouse(MouseButton::Left);
    const RIGHT_BUTTON: Input = Input::Mouse(MouseButton::Right);

    /// `ms` milliseconds after a fixed start.
    fn at(ms: u64) -> DateTime<Utc> {
        time::add(
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            Duration::from_millis(ms),
        )
    }

    const fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn the_holds_say_what_is_down_and_until_when_in_the_order_pressed() {
        let mut tracker = HoldTracker::new();
        tracker.hold(RIGHT_BUTTON).unwrap();
        tracker.hold_for(FORWARD, ms(3000), at(0)).unwrap();
        tracker.hold(JUMP).unwrap();
        tracker.release(JUMP);

        let holds: Vec<_> = tracker.holds().collect();

        assert_eq!(holds, [(RIGHT_BUTTON, None), (FORWARD, Some(at(3000)))]);
    }

    // --- The inputs ---

    #[rstest]
    #[case::forward(Key::Forward, "forward", true, true)]
    #[case::back(Key::Back, "back", true, true)]
    #[case::left(Key::Left, "left", true, true)]
    #[case::right(Key::Right, "right", true, true)]
    #[case::jump(Key::Jump, "jump", true, true)]
    #[case::sneak(Key::Sneak, "sneak", true, false)]
    #[case::sprint(Key::Sprint, "sprint", true, true)]
    #[case::drop(Key::Drop, "drop", false, false)]
    #[case::drop_stack(Key::DropStack, "drop_stack", false, false)]
    #[case::swap_offhand(Key::SwapOffhand, "swap_offhand", false, false)]
    fn each_key_has_its_name_and_its_kind(
        #[case] key: Key,
        #[case] name: &str,
        #[case] held: bool,
        #[case] movement: bool,
    ) {
        assert_eq!(key.name(), name);
        assert_eq!(Key::from_name(name), Some(key));
        assert_eq!(key.can_be_held(), held);
        assert_eq!(key.is_movement(), movement);
        assert_eq!(Input::Key(key).can_be_held(), held);
        assert_eq!(Input::Key(key).is_movement(), movement);
    }

    #[test]
    fn every_key_is_listed_once_and_named_apart() {
        let mut names: Vec<&str> = Key::ALL.iter().map(|key| key.name()).collect();
        assert_eq!(names.len(), 10);
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 10);
        for name in ["", "Forward", "w", "space", "attack", "forward "] {
            assert_eq!(Key::from_name(name), None, "{name}");
        }
    }

    #[test]
    fn both_buttons_can_be_held_and_move_nothing() {
        assert_eq!(MouseButton::Left.name(), "left");
        assert_eq!(MouseButton::Right.name(), "right");
        for button in MouseButton::ALL {
            assert!(Input::Mouse(button).can_be_held());
            assert!(!Input::Mouse(button).is_movement());
        }
    }

    // --- Holding ---

    #[test]
    fn nothing_is_held_at_the_start() {
        let tracker = HoldTracker::new();

        assert_eq!(tracker.held().count(), 0);
        assert!(!tracker.is_held(FORWARD));
        assert_eq!(tracker.next_due(), None);
    }

    #[test]
    fn a_held_input_stays_down_until_its_released() {
        let mut tracker = HoldTracker::new();

        assert_eq!(tracker.hold(FORWARD), Ok(Pressed::Now));

        assert!(tracker.is_held(FORWARD));
        assert!(!tracker.is_held(JUMP));
        assert_eq!(tracker.due(at(3_600_000)), []);
        assert!(tracker.is_held(FORWARD));
        assert!(tracker.release(FORWARD));
        assert!(!tracker.is_held(FORWARD));
    }

    #[test]
    fn a_release_of_something_that_isnt_held_does_nothing() {
        let mut tracker = HoldTracker::new();
        tracker.hold(FORWARD).unwrap();

        assert!(!tracker.release(JUMP));
        assert!(!tracker.release(Input::Key(Key::Drop)));

        assert_eq!(tracker.held().collect::<Vec<_>>(), [FORWARD]);
    }

    #[test]
    fn holding_again_presses_nothing_twice() {
        let mut tracker = HoldTracker::new();
        tracker.hold(FORWARD).unwrap();

        assert_eq!(tracker.hold(FORWARD), Ok(Pressed::Already));
        assert_eq!(
            tracker.hold_for(FORWARD, ms(1_000), at(0)),
            Ok(Pressed::Already)
        );

        assert_eq!(tracker.held().collect::<Vec<_>>(), [FORWARD]);
    }

    #[rstest]
    #[case::drop(Key::Drop)]
    #[case::drop_stack(Key::DropStack)]
    #[case::swap_offhand(Key::SwapOffhand)]
    fn a_key_that_only_taps_cant_be_held(#[case] key: Key) {
        let mut tracker = HoldTracker::new();

        assert_eq!(tracker.hold(Input::Key(key)), Err(HoldError::TapOnly));
        assert_eq!(
            tracker.hold_for(Input::Key(key), ms(1_000), at(0)),
            Err(HoldError::TapOnly)
        );
        // The key comes before the length.
        assert_eq!(
            tracker.hold_for(Input::Key(key), ms(1), at(0)),
            Err(HoldError::TapOnly)
        );
        assert_eq!(tracker.held().count(), 0);
    }

    #[test]
    fn every_input_that_can_be_held_is_held() {
        let mut tracker = HoldTracker::new();
        let holdable: Vec<Input> = Key::ALL
            .into_iter()
            .filter(|key| key.can_be_held())
            .map(Input::Key)
            .chain(MouseButton::ALL.into_iter().map(Input::Mouse))
            .collect();

        for input in &holdable {
            assert_eq!(tracker.hold(*input), Ok(Pressed::Now));
        }

        assert_eq!(holdable.len(), 9);
        assert_eq!(tracker.held().collect::<Vec<_>>(), holdable);
    }

    // --- hold_for ---

    #[rstest]
    #[case::just_before(2_999, false)]
    #[case::when_its_time_is_up(3_000, true)]
    #[case::later(60_000, true)]
    fn an_input_held_for_a_time_is_let_go_when_its_up(#[case] now: u64, #[case] let_go: bool) {
        let mut tracker = HoldTracker::new();
        assert_eq!(
            tracker.hold_for(FORWARD, ms(2_000), at(1_000)),
            Ok(Pressed::Now)
        );
        assert_eq!(tracker.next_due(), Some(at(3_000)));

        let due = tracker.due(at(now));

        assert_eq!(due, if let_go { vec![FORWARD] } else { Vec::new() });
        assert_eq!(tracker.is_held(FORWARD), !let_go);
        assert_eq!(tracker.due(at(now)), [], "let go only once");
    }

    #[rstest]
    #[case::the_shortest(50)]
    #[case::the_longest(600_000)]
    fn a_hold_lasts_50_ms_to_10_minutes(#[case] length: u64) {
        let mut tracker = HoldTracker::new();

        assert_eq!(tracker.hold_for(JUMP, ms(length), at(0)), Ok(Pressed::Now));
        assert_eq!(tracker.next_due(), Some(at(length)));
    }

    #[rstest]
    #[case::none(0)]
    #[case::too_short(49)]
    #[case::too_long(600_001)]
    #[case::a_day(86_400_000)]
    fn a_hold_of_another_length_is_refused(#[case] length: u64) {
        let mut tracker = HoldTracker::new();
        tracker.hold_for(FORWARD, ms(1_000), at(0)).unwrap();

        assert_eq!(
            tracker.hold_for(JUMP, ms(length), at(0)),
            Err(HoldError::Length)
        );
        assert_eq!(
            tracker.hold_for(FORWARD, ms(length), at(0)),
            Err(HoldError::Length)
        );

        // Nothing changed: the earlier hold keeps its time.
        assert!(!tracker.is_held(JUMP));
        assert_eq!(tracker.next_due(), Some(at(1_000)));
    }

    #[test]
    fn holding_again_replaces_how_long_it_stays_down() {
        let mut tracker = HoldTracker::new();

        // A longer hold_for.
        tracker.hold_for(FORWARD, ms(1_000), at(0)).unwrap();
        tracker.hold_for(FORWARD, ms(5_000), at(500)).unwrap();
        assert_eq!(tracker.due(at(1_000)), []);
        assert_eq!(tracker.next_due(), Some(at(5_500)));

        // A plain hold ends the time limit.
        tracker.hold(FORWARD).unwrap();
        assert_eq!(tracker.next_due(), None);
        assert_eq!(tracker.due(at(3_600_000)), []);

        // And a hold_for puts one back.
        tracker.hold_for(FORWARD, ms(50), at(0)).unwrap();
        assert_eq!(tracker.due(at(50)), [FORWARD]);
    }

    #[test]
    fn inputs_due_together_come_in_the_order_they_were_pressed() {
        let mut tracker = HoldTracker::new();
        tracker.hold_for(SPRINT, ms(3_000), at(0)).unwrap();
        tracker.hold(SNEAK).unwrap();
        tracker.hold_for(FORWARD, ms(3_000), at(0)).unwrap();
        tracker.hold_for(JUMP, ms(9_000), at(0)).unwrap();

        assert_eq!(tracker.next_due(), Some(at(3_000)));
        assert_eq!(tracker.due(at(3_000)), [SPRINT, FORWARD]);
        assert_eq!(tracker.held().collect::<Vec<_>>(), [SNEAK, JUMP]);
        assert_eq!(tracker.next_due(), Some(at(9_000)));
    }

    #[test]
    fn a_released_input_is_never_due() {
        let mut tracker = HoldTracker::new();
        tracker.hold_for(FORWARD, ms(1_000), at(0)).unwrap();

        assert!(tracker.release(FORWARD));

        assert_eq!(tracker.next_due(), None);
        assert_eq!(tracker.due(at(5_000)), []);
    }

    #[test]
    fn a_clock_that_goes_backwards_lets_go_of_nothing_early() {
        let mut tracker = HoldTracker::new();
        tracker.hold_for(FORWARD, ms(1_000), at(5_000)).unwrap();

        assert_eq!(tracker.due(at(0)), []);
        assert!(tracker.is_held(FORWARD));
        assert_eq!(tracker.due(at(6_000)), [FORWARD]);
    }

    // --- Releasing ---

    fn everything_held() -> HoldTracker {
        let mut tracker = HoldTracker::new();
        for input in [SNEAK, FORWARD, RIGHT_BUTTON, JUMP, LEFT_BUTTON, SPRINT] {
            tracker.hold(input).unwrap();
        }
        tracker
    }

    #[test]
    fn releasing_everything_leaves_nothing_held() {
        let mut tracker = everything_held();
        tracker
            .hold_for(Input::Key(Key::Back), ms(1_000), at(0))
            .unwrap();

        let released = tracker.release_all();

        assert_eq!(
            released,
            [
                SNEAK,
                FORWARD,
                RIGHT_BUTTON,
                JUMP,
                LEFT_BUTTON,
                SPRINT,
                Input::Key(Key::Back)
            ]
        );
        assert_eq!(tracker.held().count(), 0);
        assert_eq!(tracker.next_due(), None);
        assert_eq!(tracker.release_all(), [], "and again releases nothing");
    }

    #[test]
    fn releasing_the_keys_leaves_the_mouse_buttons_down() {
        let mut tracker = everything_held();

        assert_eq!(tracker.release_keys(), [SNEAK, FORWARD, JUMP, SPRINT]);

        assert_eq!(
            tracker.held().collect::<Vec<_>>(),
            [RIGHT_BUTTON, LEFT_BUTTON]
        );
    }

    #[test]
    fn the_leash_lets_go_of_what_moves_the_bot_and_nothing_else() {
        let mut tracker = everything_held();

        assert_eq!(tracker.release_movement(), [FORWARD, JUMP, SPRINT]);

        assert_eq!(
            tracker.held().collect::<Vec<_>>(),
            [SNEAK, RIGHT_BUTTON, LEFT_BUTTON]
        );
        assert_eq!(tracker.release_movement(), []);
    }

    #[test]
    fn the_errors_say_what_is_allowed() {
        assert_eq!(
            HoldError::TapOnly.to_string(),
            "this key can only be tapped"
        );
        assert_eq!(
            HoldError::Length.to_string(),
            "an input is held for 50 ms to 10 minutes"
        );
    }

    // --- Properties ---

    fn any_input() -> impl Strategy<Value = Input> {
        prop_oneof![
            proptest::sample::select(Key::ALL.to_vec()).prop_map(Input::Key),
            proptest::sample::select(MouseButton::ALL.to_vec()).prop_map(Input::Mouse),
        ]
    }

    /// One thing a script or the runtime does with the tracker.
    #[derive(Debug, Clone)]
    enum Step {
        Hold(Input),
        HoldFor(Input, u64),
        Release(Input),
        Due,
        ReleaseKeys,
        ReleaseMovement,
    }

    fn any_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            any_input().prop_map(Step::Hold),
            (any_input(), 0_u64..700_000).prop_map(|(input, length)| Step::HoldFor(input, length)),
            any_input().prop_map(Step::Release),
            Just(Step::Due),
            Just(Step::ReleaseKeys),
            Just(Step::ReleaseMovement),
        ]
    }

    proptest! {
        /// Whatever happens, in any order and at any times: an input is
        /// down at most once, only one that can be held, never past its
        /// time once `due` ran; and after `release_all`, nothing is held.
        #[test]
        fn after_any_sequence_releasing_everything_leaves_nothing_held(
            steps in proptest::collection::vec((any_step(), 0_u64..5_000), 0..60),
        ) {
            let mut tracker = HoldTracker::new();
            let mut now = 0_u64;
            for (step, pause) in steps {
                now += pause;
                match step {
                    Step::Hold(input) => {
                        prop_assert_eq!(tracker.hold(input).is_ok(), input.can_be_held());
                    }
                    Step::HoldFor(input, length) => {
                        let taken = tracker.hold_for(input, ms(length), at(now)).is_ok();
                        prop_assert_eq!(taken, input.can_be_held() && (50..=600_000).contains(&length));
                    }
                    Step::Release(input) => {
                        let was_held = tracker.is_held(input);
                        prop_assert_eq!(tracker.release(input), was_held);
                        prop_assert!(!tracker.is_held(input));
                    }
                    Step::Due => {
                        for input in tracker.due(at(now)) {
                            prop_assert!(!tracker.is_held(input));
                        }
                        prop_assert!(tracker.next_due().is_none_or(|due| due > at(now)));
                    }
                    Step::ReleaseKeys => {
                        tracker.release_keys();
                        prop_assert!(tracker.held().all(|input| matches!(input, Input::Mouse(_))));
                    }
                    Step::ReleaseMovement => {
                        tracker.release_movement();
                        prop_assert!(!tracker.held().any(Input::is_movement));
                    }
                }
                let held: Vec<Input> = tracker.held().collect();
                for (place, input) in held.iter().enumerate() {
                    prop_assert!(input.can_be_held());
                    prop_assert!(!held[..place].contains(input), "{input:?} is down twice");
                }
            }

            let released = tracker.release_all();

            prop_assert!(released.len() <= 9);
            prop_assert_eq!(tracker.held().count(), 0);
            prop_assert_eq!(tracker.next_due(), None);
            prop_assert_eq!(tracker.due(at(now + 1_000_000)), Vec::new());
        }
    }
}
