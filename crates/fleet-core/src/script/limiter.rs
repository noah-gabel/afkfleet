//! [`ActionLimiter`]: the limits that sit below a script (ADR-0036, D7).

use core::time::Duration;

use chrono::{DateTime, Utc};

use super::input::{Input, Key, MouseButton};
use super::limits;
use crate::mode::ModeDefinition;
use crate::time;

/// How an input is pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Press {
    /// Pressed and let go at once.
    Tap,
    /// Pressed and kept down, with or without a time.
    Hold,
}

/// A kind of action with a minimum time between two of them.
///
/// **There is no kind for a release:** letting go is never limited, and
/// doesn't count as a press. And there is none for a declared command,
/// which the limiter passes and the bot's chat queue paces at the bot's own
/// chat rate (the user's answer 4, ADR-0037, C8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Limited {
    /// A press of this key, tapped or held.
    Key(Key),
    /// A press of the left button, tapped or held: an attack on an entity.
    LeftButton,
    /// A tap of the right button: one use.
    RightTap,
    /// A hold of the right button.
    RightHold,
    /// A look in a direction, at a point or at an entity.
    LookAt,
    /// A turn by an amount.
    LookTurn,
    /// A hotbar slot selected.
    Hotbar,
    /// A vehicle mounted.
    Mount,
    /// A vehicle left.
    Dismount,
    /// A declared message that isn't a command.
    PlainChat,
    /// Anything a screen answers: a click, a screen's button, a trade, an
    /// anvil's name, a sign's answer (ADR-0036, R2). Closing a screen isn't
    /// limited, as a release isn't.
    Click,
}

impl Limited {
    /// The kind a press of `input` falls under.
    #[must_use]
    pub const fn of_press(input: Input, press: Press) -> Self {
        match (input, press) {
            (Input::Key(key), Press::Tap | Press::Hold) => Self::Key(key),
            (Input::Mouse(MouseButton::Left), Press::Tap | Press::Hold) => Self::LeftButton,
            (Input::Mouse(MouseButton::Right), Press::Tap) => Self::RightTap,
            (Input::Mouse(MouseButton::Right), Press::Hold) => Self::RightHold,
        }
    }

    /// The shortest time between two actions of this kind: today's
    /// shortest interval of the step that does the same (ADR-0010), so a
    /// script never acts faster than a step mode could; and for a click,
    /// which no step does, [`limits::CLICK_INTERVAL`] (ADR-0036, R2).
    #[must_use]
    pub const fn minimum(self) -> Duration {
        match self {
            Self::LeftButton => ModeDefinition::MIN_ATTACK_INTERVAL,
            Self::RightTap => ModeDefinition::MIN_USE_ITEM_INTERVAL,
            Self::RightHold => ModeDefinition::MIN_HOLD_USE_INTERVAL,
            Self::PlainChat => ModeDefinition::MIN_CHAT_INTERVAL,
            Self::Key(_)
            | Self::LookAt
            | Self::LookTurn
            | Self::Hotbar
            | Self::Mount
            | Self::Dismount => ModeDefinition::MIN_INTERVAL,
            Self::Click => limits::CLICK_INTERVAL,
        }
    }
}

/// An action came sooner than its kind allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the action came too soon; it's allowed again in {wait:?}")]
pub struct TooSoon {
    /// How long until an action of that kind is allowed.
    pub wait: Duration,
}

/// The limits below a script: when each kind of action last happened, and
/// whether one may happen now (ADR-0036, D7).
///
/// It sits between what a script asks for and the session, so no script
/// can go around it. It belongs to the bot, not to a hook or a session:
/// it outlives a reconnect and a mode change, as the 30 s memory of a step
/// mode's on-respawn chat does (ADR-0020, S24), so no sequence of events
/// resets a minimum.
///
/// The limiter holds no clock: every call takes `now` (ADR-0010). For a
/// clock that went backwards, nothing is allowed early.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionLimiter {
    /// When each kind last happened; at most one entry a kind.
    last: Vec<(Limited, DateTime<Utc>)>,
}

impl ActionLimiter {
    /// A limiter that has seen nothing, as at a bot's start: everything is
    /// allowed once.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an action of kind `what` may happen at `now`. Nothing is
    /// recorded.
    ///
    /// # Errors
    /// [`TooSoon`], with how long to wait.
    pub fn check(&self, what: Limited, now: DateTime<Utc>) -> Result<(), TooSoon> {
        let Some((_, last)) = self.last.iter().find(|(kind, _)| *kind == what) else {
            return Ok(());
        };
        // `elapsed` is zero for a clock that went backwards: the whole
        // minimum is still to wait.
        match what.minimum().checked_sub(time::elapsed(*last, now)) {
            Some(wait) if !wait.is_zero() => Err(TooSoon { wait }),
            _ => Ok(()),
        }
    }

    /// Takes an action of kind `what` at `now`: allowed, it's recorded and
    /// the next one must wait its minimum from `now`.
    ///
    /// # Errors
    /// [`TooSoon`], with how long to wait; nothing is recorded, so being
    /// refused never pushes the next allowed time on.
    pub fn try_act(&mut self, what: Limited, now: DateTime<Utc>) -> Result<(), TooSoon> {
        self.check(what, now)?;
        match self.last.iter_mut().find(|(kind, _)| *kind == what) {
            Some((_, last)) => *last = now,
            None => self.last.push((what, now)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

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

    const JUMP: Limited = Limited::Key(Key::Jump);

    #[rstest]
    #[case::a_key(Limited::Key(Key::Forward), 250)]
    #[case::a_tap_only_key(Limited::Key(Key::Drop), 250)]
    #[case::the_left_button(Limited::LeftButton, 500)]
    #[case::a_right_tap(Limited::RightTap, 200)]
    #[case::a_right_hold(Limited::RightHold, 500)]
    #[case::a_look(Limited::LookAt, 250)]
    #[case::a_turn(Limited::LookTurn, 250)]
    #[case::the_hotbar(Limited::Hotbar, 250)]
    #[case::mounting(Limited::Mount, 250)]
    #[case::dismounting(Limited::Dismount, 250)]
    #[case::plain_chat(Limited::PlainChat, 30_000)]
    #[case::a_click(Limited::Click, 50)]
    fn each_kind_has_its_minimum(#[case] what: Limited, #[case] millis: u64) {
        assert_eq!(what.minimum(), ms(millis));
    }

    /// The reference's table (docs/scripting.md, "Limits") is a step
    /// mode's: a change to a step's shortest interval changes a script's.
    #[test]
    fn the_minimums_are_a_step_modes() {
        assert_eq!(JUMP.minimum(), ModeDefinition::MIN_INTERVAL);
        assert_eq!(Limited::LookAt.minimum(), ModeDefinition::MIN_INTERVAL);
        assert_eq!(
            Limited::LeftButton.minimum(),
            ModeDefinition::MIN_ATTACK_INTERVAL
        );
        assert_eq!(
            Limited::RightTap.minimum(),
            ModeDefinition::MIN_USE_ITEM_INTERVAL
        );
        assert_eq!(
            Limited::RightHold.minimum(),
            ModeDefinition::MIN_HOLD_USE_INTERVAL
        );
        assert_eq!(
            Limited::PlainChat.minimum(),
            ModeDefinition::MIN_CHAT_INTERVAL
        );
    }

    #[rstest]
    #[case::a_key_tapped(Input::Key(Key::Jump), Press::Tap, Limited::Key(Key::Jump))]
    #[case::a_key_held(Input::Key(Key::Jump), Press::Hold, Limited::Key(Key::Jump))]
    #[case::another_key(Input::Key(Key::Drop), Press::Tap, Limited::Key(Key::Drop))]
    #[case::left_tapped(Input::Mouse(MouseButton::Left), Press::Tap, Limited::LeftButton)]
    #[case::left_held(Input::Mouse(MouseButton::Left), Press::Hold, Limited::LeftButton)]
    #[case::right_tapped(Input::Mouse(MouseButton::Right), Press::Tap, Limited::RightTap)]
    #[case::right_held(Input::Mouse(MouseButton::Right), Press::Hold, Limited::RightHold)]
    fn a_press_falls_under_its_kind(
        #[case] input: Input,
        #[case] press: Press,
        #[case] what: Limited,
    ) {
        assert_eq!(Limited::of_press(input, press), what);
    }

    /// R2: one click a game tick, no burst: the second click of a tick
    /// waits, whatever else the bot does.
    #[test]
    fn a_click_comes_at_most_once_a_game_tick() {
        let mut limiter = ActionLimiter::new();

        assert_eq!(limiter.try_act(Limited::Click, at(0)), Ok(()));
        assert_eq!(
            limiter.try_act(Limited::Click, at(0)),
            Err(TooSoon { wait: ms(50) })
        );
        assert_eq!(
            limiter.try_act(Limited::Click, at(49)),
            Err(TooSoon { wait: ms(1) })
        );
        assert_eq!(limiter.try_act(Limited::Hotbar, at(49)), Ok(()));
        assert_eq!(limiter.try_act(Limited::Click, at(50)), Ok(()));
        assert_eq!(limiter.try_act(Limited::Click, at(100)), Ok(()));
        assert_eq!(Limited::Click.minimum(), limits::CLICK_INTERVAL);
        assert_eq!(limits::CLICK_INTERVAL, limits::MIN_HOLD_FOR);
    }

    #[test]
    fn the_first_action_of_a_kind_is_allowed() {
        let mut limiter = ActionLimiter::new();

        assert_eq!(limiter.check(JUMP, at(0)), Ok(()));
        assert_eq!(limiter.try_act(JUMP, at(0)), Ok(()));
    }

    #[rstest]
    #[case::at_once(0, Some(250))]
    #[case::a_millisecond_early(249, Some(1))]
    #[case::at_the_minimum(250, None)]
    #[case::later(10_000, None)]
    fn the_next_one_waits_the_kinds_minimum(#[case] later: u64, #[case] wait: Option<u64>) {
        let mut limiter = ActionLimiter::new();
        limiter.try_act(JUMP, at(1_000)).unwrap();

        let answer = wait.map_or(Ok(()), |wait| Err(TooSoon { wait: ms(wait) }));

        assert_eq!(limiter.check(JUMP, at(1_000 + later)), answer);
        assert_eq!(limiter.try_act(JUMP, at(1_000 + later)), answer);
    }

    #[test]
    fn checking_records_nothing() {
        let mut limiter = ActionLimiter::new();

        for now in [0, 10, 20] {
            assert_eq!(limiter.check(JUMP, at(now)), Ok(()));
        }
        assert_eq!(limiter.try_act(JUMP, at(30)), Ok(()));
        assert_eq!(limiter.check(JUMP, at(40)), Err(TooSoon { wait: ms(240) }));
    }

    /// A script that keeps asking is refused each time, and the time it's
    /// allowed again doesn't move.
    #[test]
    fn being_refused_never_pushes_the_next_allowed_time_on() {
        let mut limiter = ActionLimiter::new();
        limiter.try_act(Limited::LeftButton, at(0)).unwrap();

        for now in [100, 200, 300, 400, 499] {
            assert!(
                limiter.try_act(Limited::LeftButton, at(now)).is_err(),
                "{now}"
            );
        }

        assert_eq!(limiter.try_act(Limited::LeftButton, at(500)), Ok(()));
        assert_eq!(
            limiter.try_act(Limited::LeftButton, at(999)),
            Err(TooSoon { wait: ms(1) })
        );
    }

    #[test]
    fn each_kind_keeps_its_own_time() {
        let mut limiter = ActionLimiter::new();
        limiter.try_act(JUMP, at(0)).unwrap();

        // Another key, the buttons and a look are free at the same moment.
        for what in [
            Limited::Key(Key::Forward),
            Limited::LeftButton,
            Limited::RightTap,
            Limited::RightHold,
            Limited::LookAt,
            Limited::LookTurn,
            Limited::Hotbar,
            Limited::Mount,
            Limited::Dismount,
            Limited::PlainChat,
        ] {
            assert_eq!(limiter.try_act(what, at(0)), Ok(()), "{what:?}");
        }
        // And each is then held to its own minimum.
        assert_eq!(limiter.try_act(Limited::RightTap, at(200)), Ok(()));
        assert_eq!(
            limiter.try_act(Limited::RightHold, at(200)),
            Err(TooSoon { wait: ms(300) })
        );
        assert_eq!(
            limiter.try_act(Limited::PlainChat, at(29_999)),
            Err(TooSoon { wait: ms(1) })
        );
        assert_eq!(limiter.try_act(Limited::PlainChat, at(30_000)), Ok(()));
    }

    /// A tap and a hold of one key are one kind: a script can't alternate
    /// them to press twice as often.
    #[test]
    fn a_tap_and_a_hold_of_one_key_share_its_minimum() {
        let mut limiter = ActionLimiter::new();
        let jump = Input::Key(Key::Jump);
        limiter
            .try_act(Limited::of_press(jump, Press::Tap), at(0))
            .unwrap();

        assert_eq!(
            limiter.try_act(Limited::of_press(jump, Press::Hold), at(100)),
            Err(TooSoon { wait: ms(150) })
        );
    }

    #[test]
    fn a_clock_that_goes_backwards_allows_nothing_early() {
        let mut limiter = ActionLimiter::new();
        limiter.try_act(JUMP, at(10_000)).unwrap();

        assert_eq!(limiter.try_act(JUMP, at(0)), Err(TooSoon { wait: ms(250) }));
        assert_eq!(
            limiter.try_act(JUMP, at(10_249)),
            Err(TooSoon { wait: ms(1) })
        );
        assert_eq!(limiter.try_act(JUMP, at(10_250)), Ok(()));
    }

    #[test]
    fn the_error_says_how_long_to_wait() {
        assert_eq!(
            TooSoon { wait: ms(150) }.to_string(),
            "the action came too soon; it's allowed again in 150ms"
        );
    }

    fn any_limited() -> impl Strategy<Value = Limited> {
        prop_oneof![
            proptest::sample::select(Key::ALL.to_vec()).prop_map(Limited::Key),
            Just(Limited::LeftButton),
            Just(Limited::RightTap),
            Just(Limited::RightHold),
            Just(Limited::LookAt),
            Just(Limited::LookTurn),
            Just(Limited::Hotbar),
            Just(Limited::Mount),
            Just(Limited::Dismount),
            Just(Limited::PlainChat),
        ]
    }

    proptest! {
        /// Whatever a script asks for, at whatever times, with a clock that
        /// may jump either way: two actions of one kind that were allowed
        /// are never closer than that kind's minimum.
        #[test]
        fn no_sequence_makes_two_actions_of_a_kind_closer_than_its_minimum(
            asks in proptest::collection::vec((any_limited(), 0_u64..40_000), 0..200),
            backwards in proptest::collection::vec(any::<bool>(), 200),
        ) {
            let mut limiter = ActionLimiter::new();
            let mut allowed: Vec<(Limited, DateTime<Utc>)> = Vec::new();
            let mut now = 100_000_u64;
            for ((what, step), back) in asks.into_iter().zip(backwards) {
                now = if back { now.saturating_sub(step) } else { now + step / 20 };
                let checked = limiter.check(what, at(now));
                let taken = limiter.try_act(what, at(now));
                prop_assert_eq!(checked, taken);
                if taken.is_ok() {
                    if let Some((_, last)) = allowed.iter().rev().find(|(kind, _)| *kind == what) {
                        prop_assert!(
                            time::elapsed(*last, at(now)) >= what.minimum(),
                            "{what:?}: {last} then {}", at(now)
                        );
                    }
                    allowed.push((what, at(now)));
                }
            }
            // One entry a kind at most, however long the bot runs.
            prop_assert!(limiter.last.len() <= Key::ALL.len() + 9);
        }
    }
}
