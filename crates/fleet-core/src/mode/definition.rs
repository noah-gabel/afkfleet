//! [`ModeDefinition`]: a validated mode, and [`ModeDraft`], its unvalidated
//! form.

use core::fmt;
use core::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Action, HotbarSlot};
use crate::chat::ChatMessage;

/// When a [`Step`] runs.
///
/// In JSON a schedule is tagged with its `"type"`, and durations are whole
/// milliseconds: `{"type":"every","interval_ms":45000,"jitter_ms":75000}`
/// (ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Schedule {
    /// Once, right after the bot joins.
    AtStart,
    /// Again and again while the bot is online. Each gap is uniform in
    /// [`interval`, `interval + jitter`], and the first run comes one gap after
    /// the bot joins.
    Every {
        /// The shortest gap between two runs.
        #[serde(rename = "interval_ms", with = "super::millis")]
        interval: Duration,
        /// How much longer than `interval` a gap may be.
        #[serde(rename = "jitter_ms", with = "super::millis")]
        jitter: Duration,
    },
}

/// One entry of a mode: what the bot does, when, and how likely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// What the bot does.
    pub action: Action,
    /// When.
    pub schedule: Schedule,
    /// The chance in percent, 1 to 100, that the step runs each time it's
    /// due.
    pub probability: u8,
}

/// A mode as it's stored or entered, before [`ModeDraft::validate`] has
/// checked it.
///
/// Its JSON is `{"steps":[…]}`. Every field is required; unknown fields are
/// ignored, so a server that was rolled back can still read newer rows that
/// only add fields (ADR-0010).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModeDraft {
    /// The steps, in the order they run when several are due at once.
    pub steps: Vec<Step>,
}

/// A validated mode: what a bot does while it's online.
///
/// The only ways to get one are [`ModeDraft::validate`] and the presets
/// ([`ModeDefinition::afk`], [`ModeDefinition::farm`]), so every definition
/// keeps the limits. It has the same JSON as [`ModeDraft`], and reading it
/// validates again, so stored modes can't skip the checks either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ModeDraft", into = "ModeDraft")]
pub struct ModeDefinition {
    steps: Vec<Step>,
}

/// Why a [`ModeDraft`] isn't a valid mode.
///
/// `step` is the position of the offending step in [`ModeDraft::steps`],
/// counting from 0. Validation stops at the first error, in step order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ModeError {
    /// The mode has more than [`ModeDefinition::MAX_STEPS`] steps.
    #[error("the mode has {count} steps; the limit is {max}", max = ModeDefinition::MAX_STEPS)]
    TooManySteps {
        /// How many steps it has.
        count: usize,
    },
    /// A repeating step's interval is shorter than its action allows.
    #[error("step {step}: the interval must be at least {min:?}")]
    IntervalTooShort {
        /// The step.
        step: usize,
        /// The shortest interval for the step's action.
        min: Duration,
    },
    /// A repeating step's interval is longer than
    /// [`ModeDefinition::MAX_INTERVAL`].
    #[error("step {step}: the interval must be at most 24 h")]
    IntervalTooLong {
        /// The step.
        step: usize,
    },
    /// A repeating step's jitter is longer than [`ModeDefinition::MAX_JITTER`].
    #[error("step {step}: the jitter must be at most 24 h")]
    JitterTooLong {
        /// The step.
        step: usize,
    },
    /// The probability isn't 1 to 100 percent.
    #[error("step {step}: the probability must be 1 to 100 percent")]
    InvalidProbability {
        /// The step.
        step: usize,
    },
    /// An angle is outside its range, or isn't a finite number.
    #[error("step {step}: {angle} is out of range")]
    AngleOutOfRange {
        /// The step.
        step: usize,
        /// Which angle.
        angle: Angle,
    },
    /// A random rotation whose maxima are both zero, so it never turns.
    #[error("step {step}: a random rotation needs a maximum yaw or pitch above zero")]
    NoRotation {
        /// The step.
        step: usize,
    },
    /// A second step of a kind a mode may have only once.
    #[error("step {step}: a mode can have only one {kind} step")]
    DuplicateStep {
        /// The second step of that kind.
        step: usize,
        /// The kind.
        kind: LimitedStep,
    },
}

/// The angle a [`ModeError::AngleOutOfRange`] is about, named like its JSON
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Angle {
    /// The yaw of [`Action::Look`], −180 to 180.
    Yaw,
    /// The pitch of [`Action::Look`], −90 to 90.
    Pitch,
    /// The maximum yaw of [`Action::RotateRandom`], 0 to 180.
    MaxYaw,
    /// The maximum pitch of [`Action::RotateRandom`], 0 to 90.
    MaxPitch,
}

impl Angle {
    /// Returns the JSON field name: `yaw`, `pitch`, `max_yaw` or `max_pitch`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Yaw => "yaw",
            Self::Pitch => "pitch",
            Self::MaxYaw => "max_yaw",
            Self::MaxPitch => "max_pitch",
        }
    }
}

impl fmt::Display for Angle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A kind of step a mode may have at most once (ADR-0010). Two of them would
/// get around the attack, chat or hold-use interval, or fire together on
/// every join. [`Action::HoldUse`] counts holding and letting go apart, so a
/// mode can let go and draw again on a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitedStep {
    /// [`Action::SendChat`] at start.
    StartChat,
    /// [`Action::SendChat`] repeating.
    RepeatingChat,
    /// [`Action::AttackFacingEntity`] at start.
    StartAttack,
    /// [`Action::AttackFacingEntity`] repeating.
    RepeatingAttack,
    /// [`Action::HoldUse`] holding (`on: true`) at start.
    StartHold,
    /// [`Action::HoldUse`] holding (`on: true`) repeating.
    RepeatingHold,
    /// [`Action::HoldUse`] letting go (`on: false`) at start.
    StartRelease,
    /// [`Action::HoldUse`] letting go (`on: false`) repeating.
    RepeatingRelease,
}

impl fmt::Display for LimitedStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StartChat => "chat-at-start",
            Self::RepeatingChat => "repeating chat",
            Self::StartAttack => "attack-at-start",
            Self::RepeatingAttack => "repeating attack",
            Self::StartHold => "hold-at-start",
            Self::RepeatingHold => "repeating hold",
            Self::StartRelease => "release-at-start",
            Self::RepeatingRelease => "repeating release",
        })
    }
}

impl ModeDefinition {
    /// The most steps a mode may have.
    pub const MAX_STEPS: usize = 32;
    /// The shortest interval of a repeating step.
    pub const MIN_INTERVAL: Duration = Duration::from_millis(250);
    /// The shortest interval of a repeating [`Action::AttackFacingEntity`].
    pub const MIN_ATTACK_INTERVAL: Duration = Duration::from_millis(500);
    /// The shortest interval of a repeating [`Action::HoldUse`], holding or
    /// letting go (ADR-0010).
    pub const MIN_HOLD_USE_INTERVAL: Duration = Duration::from_millis(500);
    /// The shortest interval of a repeating [`Action::SendChat`] (Plan.md
    /// §7.3).
    pub const MIN_CHAT_INTERVAL: Duration = Duration::from_secs(30);
    /// The longest interval of a repeating step: 24 h.
    pub const MAX_INTERVAL: Duration = Duration::from_hours(24);
    /// The longest jitter of a repeating step: 24 h.
    pub const MAX_JITTER: Duration = Duration::from_hours(24);
    /// The largest yaw of [`Action::Look`] in either direction, and the
    /// largest maximum yaw of [`Action::RotateRandom`].
    pub const MAX_YAW: f32 = 180.0;
    /// The largest pitch of [`Action::Look`] in either direction, and the
    /// largest maximum pitch of [`Action::RotateRandom`].
    pub const MAX_PITCH: f32 = 90.0;

    /// Returns the steps.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Returns the [`Action::SendChat`] messages that are `/commands`, in step
    /// order, for the allowlist check (P2.9).
    pub fn commands(&self) -> impl Iterator<Item = &ChatMessage> {
        // Every action is listed: one that sends text must show up here, or
        // it would get around the allowlist.
        self.steps.iter().filter_map(|step| match &step.action {
            Action::SendChat { message } => message.is_command().then_some(message),
            Action::Look { .. }
            | Action::RotateRandom { .. }
            | Action::Jump
            | Action::Sneak { .. }
            | Action::SwingArm
            | Action::UseItem
            | Action::HoldUse { .. }
            | Action::AttackFacingEntity
            | Action::SelectHotbarSlot { .. } => None,
        })
    }

    /// The built-in `afk` mode: keeps a bot from being kicked for idling.
    ///
    /// It turns by up to 30° left or right and 10° up or down every 45–120 s,
    /// and swings its arm every 20–40 s. Turning alone doesn't reset the
    /// server's idle timer, but a swing does, and 40 s is below the shortest
    /// non-zero `player-idle-timeout` of 60 s (ADR-0008 §8, ADR-0010).
    #[must_use]
    pub fn afk() -> Self {
        Self {
            steps: vec![
                Step {
                    action: Action::RotateRandom {
                        max_yaw: 30.0,
                        max_pitch: 10.0,
                    },
                    schedule: Schedule::Every {
                        interval: Duration::from_secs(45),
                        jitter: Duration::from_secs(75),
                    },
                    probability: 100,
                },
                Step {
                    action: Action::SwingArm,
                    schedule: Schedule::Every {
                        interval: Duration::from_secs(20),
                        jitter: Duration::from_secs(20),
                    },
                    probability: 100,
                },
            ],
        }
    }

    /// The built-in `farm` mode: attacks whatever the bot looks at.
    ///
    /// It selects the first hotbar slot when it joins and attacks every
    /// 650–800 ms. It doesn't turn: the server restores the account's saved
    /// direction at join. Any other slot or direction is a custom mode
    /// (ADR-0010).
    #[must_use]
    pub fn farm() -> Self {
        Self {
            steps: vec![
                Step {
                    action: Action::SelectHotbarSlot {
                        slot: HotbarSlot::FIRST,
                    },
                    schedule: Schedule::AtStart,
                    probability: 100,
                },
                Step {
                    action: Action::AttackFacingEntity,
                    schedule: Schedule::Every {
                        interval: Duration::from_millis(650),
                        jitter: Duration::from_millis(150),
                    },
                    probability: 100,
                },
            ],
        }
    }
}

impl ModeDraft {
    /// Checks the mode against the limits and returns it as a
    /// [`ModeDefinition`].
    ///
    /// The limits (Plan.md P2.7; ADR-0010):
    /// - at most [`ModeDefinition::MAX_STEPS`] steps; an empty mode is valid
    ///   and does nothing
    /// - a repeating step's interval is at least 250 ms, 500 ms for an attack
    ///   or a hold-use and 30 s for chat, and at most 24 h; its jitter is at
    ///   most 24 h
    /// - durations count in whole milliseconds, as they're stored; anything
    ///   finer is dropped before the check
    /// - the probability is 1 to 100 percent
    /// - [`Action::Look`]: yaw −180 to 180, pitch −90 to 90
    /// - [`Action::RotateRandom`]: maximum yaw 0 to 180, maximum pitch 0 to
    ///   90, not both zero
    /// - at most one step of each [`LimitedStep`] kind
    ///
    /// # Errors
    /// The first [`ModeError`] in step order. [`ModeError::TooManySteps`] is
    /// checked before any step; within a step the action comes first, then
    /// the probability, the schedule, and whether the step is a second one of
    /// a limited kind.
    pub fn validate(self) -> Result<ModeDefinition, ModeError> {
        let count = self.steps.len();
        if count > ModeDefinition::MAX_STEPS {
            return Err(ModeError::TooManySteps { count });
        }
        let mut seen = Vec::new();
        let steps = self
            .steps
            .into_iter()
            .enumerate()
            .map(|(index, step)| check_step(index, step, &mut seen))
            .collect::<Result<_, _>>()?;
        Ok(ModeDefinition { steps })
    }
}

/// Checks one step and returns it with its durations in whole milliseconds.
/// `seen` collects the limited kinds of the steps before it.
fn check_step(index: usize, step: Step, seen: &mut Vec<LimitedStep>) -> Result<Step, ModeError> {
    check_action(index, &step.action)?;
    if !(1..=100).contains(&step.probability) {
        return Err(ModeError::InvalidProbability { step: index });
    }
    let schedule = check_schedule(index, &step.action, step.schedule)?;
    if let Some(kind) = limited_kind(&step.action, schedule) {
        if seen.contains(&kind) {
            return Err(ModeError::DuplicateStep { step: index, kind });
        }
        seen.push(kind);
    }
    Ok(Step { schedule, ..step })
}

/// Checks the angles of a look or a random rotation.
fn check_action(index: usize, action: &Action) -> Result<(), ModeError> {
    const YAW: f32 = ModeDefinition::MAX_YAW;
    const PITCH: f32 = ModeDefinition::MAX_PITCH;
    match *action {
        Action::Look { yaw, pitch } => {
            check_angle(index, yaw, -YAW, YAW, Angle::Yaw)?;
            check_angle(index, pitch, -PITCH, PITCH, Angle::Pitch)
        }
        Action::RotateRandom { max_yaw, max_pitch } => {
            check_angle(index, max_yaw, 0.0, YAW, Angle::MaxYaw)?;
            check_angle(index, max_pitch, 0.0, PITCH, Angle::MaxPitch)?;
            if max_yaw == 0.0 && max_pitch == 0.0 {
                Err(ModeError::NoRotation { step: index })
            } else {
                Ok(())
            }
        }
        Action::Jump
        | Action::Sneak { .. }
        | Action::SwingArm
        | Action::UseItem
        | Action::HoldUse { .. }
        | Action::AttackFacingEntity
        | Action::SelectHotbarSlot { .. }
        | Action::SendChat { .. } => Ok(()),
    }
}

/// Checks that `value` lies in `min..=max`, which also rules out NaN and the
/// infinities.
fn check_angle(
    index: usize,
    value: f32,
    min: f32,
    max: f32,
    angle: Angle,
) -> Result<(), ModeError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ModeError::AngleOutOfRange { step: index, angle })
    }
}

/// Checks a repeating step's interval and jitter, in whole milliseconds, and
/// returns the schedule with them.
fn check_schedule(
    index: usize,
    action: &Action,
    schedule: Schedule,
) -> Result<Schedule, ModeError> {
    let Schedule::Every { interval, jitter } = schedule else {
        return Ok(schedule);
    };
    let (interval, jitter) = (whole_millis(interval), whole_millis(jitter));
    let min = min_interval(action);
    if interval < min {
        return Err(ModeError::IntervalTooShort { step: index, min });
    }
    if interval > ModeDefinition::MAX_INTERVAL {
        return Err(ModeError::IntervalTooLong { step: index });
    }
    if jitter > ModeDefinition::MAX_JITTER {
        return Err(ModeError::JitterTooLong { step: index });
    }
    Ok(Schedule::Every { interval, jitter })
}

/// The shortest interval for a repeating `action`. Every action is listed, so
/// a new one needs a decision about its limit.
const fn min_interval(action: &Action) -> Duration {
    match action {
        Action::AttackFacingEntity => ModeDefinition::MIN_ATTACK_INTERVAL,
        Action::SendChat { .. } => ModeDefinition::MIN_CHAT_INTERVAL,
        Action::HoldUse { .. } => ModeDefinition::MIN_HOLD_USE_INTERVAL,
        Action::Look { .. }
        | Action::RotateRandom { .. }
        | Action::Jump
        | Action::Sneak { .. }
        | Action::SwingArm
        | Action::UseItem
        | Action::SelectHotbarSlot { .. } => ModeDefinition::MIN_INTERVAL,
    }
}

/// Which kind of step a mode may have only once this step is, if any. Every
/// action is listed, so a new one needs a decision.
const fn limited_kind(action: &Action, schedule: Schedule) -> Option<LimitedStep> {
    let repeating = matches!(schedule, Schedule::Every { .. });
    match action {
        Action::SendChat { .. } if repeating => Some(LimitedStep::RepeatingChat),
        Action::SendChat { .. } => Some(LimitedStep::StartChat),
        Action::AttackFacingEntity if repeating => Some(LimitedStep::RepeatingAttack),
        Action::AttackFacingEntity => Some(LimitedStep::StartAttack),
        Action::HoldUse { on: true } if repeating => Some(LimitedStep::RepeatingHold),
        Action::HoldUse { on: true } => Some(LimitedStep::StartHold),
        Action::HoldUse { on: false } if repeating => Some(LimitedStep::RepeatingRelease),
        Action::HoldUse { on: false } => Some(LimitedStep::StartRelease),
        Action::Look { .. }
        | Action::RotateRandom { .. }
        | Action::Jump
        | Action::Sneak { .. }
        | Action::SwingArm
        | Action::UseItem
        | Action::SelectHotbarSlot { .. } => None,
    }
}

/// Drops everything finer than a millisecond, as the stored JSON does.
const fn whole_millis(duration: Duration) -> Duration {
    // The nanoseconds stay below one second, so `new` never carries over.
    Duration::new(duration.as_secs(), duration.subsec_millis() * 1_000_000)
}

impl TryFrom<ModeDraft> for ModeDefinition {
    type Error = ModeError;

    fn try_from(draft: ModeDraft) -> Result<Self, ModeError> {
        draft.validate()
    }
}

impl From<ModeDefinition> for ModeDraft {
    fn from(definition: ModeDefinition) -> Self {
        Self {
            steps: definition.steps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ModeError::{
        AngleOutOfRange, DuplicateStep, IntervalTooLong, IntervalTooShort, InvalidProbability,
        JitterTooLong, NoRotation, TooManySteps,
    };
    use proptest::collection::vec;
    use proptest::prelude::*;
    use rstest::rstest;

    const DAY: Duration = Duration::from_hours(24);
    const DAY_MS: u64 = 86_400_000;

    const fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    const fn every(interval: Duration, jitter: Duration) -> Schedule {
        Schedule::Every { interval, jitter }
    }

    fn step(action: Action, schedule: Schedule) -> Step {
        Step {
            action,
            schedule,
            probability: 100,
        }
    }

    fn at_start(action: Action) -> Step {
        step(action, Schedule::AtStart)
    }

    /// A repeating step with an interval every action allows.
    fn repeating(action: Action) -> Step {
        step(action, every(ms(60_000), Duration::ZERO))
    }

    fn with_probability(probability: u8, step: Step) -> Step {
        Step {
            probability,
            ..step
        }
    }

    fn chat(text: &str) -> Action {
        Action::SendChat {
            message: text.parse().unwrap(),
        }
    }

    const fn look(yaw: f32, pitch: f32) -> Action {
        Action::Look { yaw, pitch }
    }

    const fn rotate(max_yaw: f32, max_pitch: f32) -> Action {
        Action::RotateRandom { max_yaw, max_pitch }
    }

    const fn hold(on: bool) -> Action {
        Action::HoldUse { on }
    }

    fn draft(steps: Vec<Step>) -> ModeDraft {
        ModeDraft { steps }
    }

    fn validate(steps: Vec<Step>) -> Result<ModeDefinition, ModeError> {
        draft(steps).validate()
    }

    #[test]
    fn an_empty_mode_is_valid() {
        assert_eq!(validate(Vec::new()).unwrap().steps(), []);
    }

    #[rstest]
    #[case::shortest_interval(step(Action::Jump, every(ms(250), ms(0))))]
    #[case::shortest_attack_interval(step(Action::AttackFacingEntity, every(ms(500), ms(0))))]
    #[case::shortest_chat_interval(step(chat("hello"), every(ms(30_000), ms(0))))]
    #[case::shortest_hold_interval(step(hold(true), every(ms(500), ms(0))))]
    #[case::shortest_release_interval(step(hold(false), every(ms(500), ms(0))))]
    #[case::hold_at_start(at_start(hold(true)))]
    #[case::longest_interval_and_jitter(step(Action::UseItem, every(DAY, DAY)))]
    #[case::lowest_probability(with_probability(1, at_start(Action::SwingArm)))]
    #[case::look_at_the_lower_limits(at_start(look(-180.0, -90.0)))]
    #[case::look_at_the_upper_limits(at_start(look(180.0, 90.0)))]
    #[case::rotate_at_the_limits(repeating(rotate(180.0, 90.0)))]
    #[case::rotate_yaw_only(repeating(rotate(30.0, 0.0)))]
    #[case::rotate_pitch_only(repeating(rotate(0.0, 10.0)))]
    #[case::sneak(repeating(Action::Sneak { on: false }))]
    #[case::hotbar(at_start(Action::SelectHotbarSlot { slot: HotbarSlot::LAST }))]
    #[case::chat_at_start(at_start(chat("/spawn")))]
    fn accepts_a_valid_step_unchanged(#[case] step: Step) {
        let definition = validate(vec![step.clone()]).unwrap();
        assert_eq!(definition.steps(), [step]);
    }

    #[rstest]
    #[case::interval_below_250_ms(
        step(Action::Jump, every(ms(249), ms(0))),
        IntervalTooShort { step: 0, min: ms(250) }
    )]
    #[case::zero_interval(
        step(Action::SwingArm, every(ms(0), ms(1_000))),
        IntervalTooShort { step: 0, min: ms(250) }
    )]
    #[case::attack_below_500_ms(
        step(Action::AttackFacingEntity, every(ms(499), ms(1_000))),
        IntervalTooShort { step: 0, min: ms(500) }
    )]
    #[case::chat_below_30_s(
        step(chat("hi"), every(ms(29_999), ms(60_000))),
        IntervalTooShort { step: 0, min: ms(30_000) }
    )]
    #[case::hold_below_500_ms(
        step(hold(true), every(ms(499), ms(1_000))),
        IntervalTooShort { step: 0, min: ms(500) }
    )]
    #[case::release_below_500_ms(
        step(hold(false), every(ms(499), ms(1_000))),
        IntervalTooShort { step: 0, min: ms(500) }
    )]
    #[case::sub_millisecond_parts_dont_count(
        step(Action::Jump, every(Duration::from_micros(249_999), ms(0))),
        IntervalTooShort { step: 0, min: ms(250) }
    )]
    #[case::interval_above_24_h(
        step(Action::Jump, every(DAY + ms(1), ms(0))),
        IntervalTooLong { step: 0 }
    )]
    #[case::jitter_above_24_h(
        step(Action::Jump, every(ms(250), DAY + ms(1))),
        JitterTooLong { step: 0 }
    )]
    #[case::probability_0(with_probability(0, at_start(Action::Jump)), InvalidProbability { step: 0 })]
    #[case::probability_101(with_probability(101, repeating(Action::Jump)), InvalidProbability { step: 0 })]
    #[case::probability_255(with_probability(255, at_start(Action::Jump)), InvalidProbability { step: 0 })]
    #[case::yaw_above_180(
        at_start(look(180.0_f32.next_up(), 0.0)),
        AngleOutOfRange { step: 0, angle: Angle::Yaw }
    )]
    #[case::yaw_below_minus_180(
        at_start(look((-180.0_f32).next_down(), 0.0)),
        AngleOutOfRange { step: 0, angle: Angle::Yaw }
    )]
    #[case::yaw_nan(at_start(look(f32::NAN, 0.0)), AngleOutOfRange { step: 0, angle: Angle::Yaw })]
    #[case::yaw_infinite(
        at_start(look(f32::INFINITY, 0.0)),
        AngleOutOfRange { step: 0, angle: Angle::Yaw }
    )]
    #[case::pitch_above_90(
        at_start(look(0.0, 90.0_f32.next_up())),
        AngleOutOfRange { step: 0, angle: Angle::Pitch }
    )]
    #[case::pitch_below_minus_90(
        at_start(look(0.0, (-90.0_f32).next_down())),
        AngleOutOfRange { step: 0, angle: Angle::Pitch }
    )]
    #[case::pitch_nan(at_start(look(0.0, f32::NAN)), AngleOutOfRange { step: 0, angle: Angle::Pitch })]
    #[case::pitch_negative_infinity(
        at_start(look(0.0, f32::NEG_INFINITY)),
        AngleOutOfRange { step: 0, angle: Angle::Pitch }
    )]
    #[case::max_yaw_negative(
        repeating(rotate(-1.0, 10.0)),
        AngleOutOfRange { step: 0, angle: Angle::MaxYaw }
    )]
    #[case::max_yaw_barely_negative(
        repeating(rotate(0.0_f32.next_down(), 10.0)),
        AngleOutOfRange { step: 0, angle: Angle::MaxYaw }
    )]
    #[case::max_yaw_above_180(
        repeating(rotate(180.0_f32.next_up(), 10.0)),
        AngleOutOfRange { step: 0, angle: Angle::MaxYaw }
    )]
    #[case::max_yaw_nan(
        repeating(rotate(f32::NAN, 10.0)),
        AngleOutOfRange { step: 0, angle: Angle::MaxYaw }
    )]
    #[case::max_pitch_negative(
        repeating(rotate(30.0, -10.0)),
        AngleOutOfRange { step: 0, angle: Angle::MaxPitch }
    )]
    #[case::max_pitch_above_90(
        repeating(rotate(30.0, 90.0_f32.next_up())),
        AngleOutOfRange { step: 0, angle: Angle::MaxPitch }
    )]
    #[case::max_pitch_infinite(
        repeating(rotate(30.0, f32::INFINITY)),
        AngleOutOfRange { step: 0, angle: Angle::MaxPitch }
    )]
    #[case::no_rotation(repeating(rotate(0.0, 0.0)), NoRotation { step: 0 })]
    #[case::no_rotation_negative_zero(repeating(rotate(-0.0, 0.0)), NoRotation { step: 0 })]
    fn rejects_an_invalid_step(#[case] step: Step, #[case] error: ModeError) {
        assert_eq!(validate(vec![step]), Err(error));
    }

    #[test]
    fn reports_the_position_of_the_failing_step() {
        let steps = vec![
            at_start(Action::Jump),
            repeating(Action::SwingArm),
            step(Action::UseItem, every(ms(1), ms(0))),
        ];
        assert_eq!(
            validate(steps),
            Err(IntervalTooShort {
                step: 2,
                min: ms(250)
            })
        );
    }

    #[test]
    fn accepts_32_steps() {
        assert!(validate(vec![at_start(Action::Jump); 32]).is_ok());
    }

    #[test]
    fn rejects_33_steps_before_checking_any_step() {
        let mut steps = vec![at_start(Action::Jump); 33];
        steps[0].probability = 0;
        assert_eq!(validate(steps), Err(TooManySteps { count: 33 }));
    }

    #[rstest]
    #[case::earlier_step_first(
        vec![with_probability(0, at_start(Action::Jump)), at_start(look(f32::NAN, 0.0))],
        InvalidProbability { step: 0 }
    )]
    #[case::action_before_probability(
        vec![with_probability(0, step(look(f32::NAN, 0.0), every(ms(1), ms(0))))],
        AngleOutOfRange { step: 0, angle: Angle::Yaw }
    )]
    #[case::probability_before_schedule(
        vec![with_probability(0, step(Action::Jump, every(ms(1), ms(0))))],
        InvalidProbability { step: 0 }
    )]
    #[case::schedule_before_duplicate(
        vec![repeating(chat("a")), step(chat("b"), every(ms(1), ms(0)))],
        IntervalTooShort { step: 1, min: ms(30_000) }
    )]
    #[case::yaw_before_pitch(
        vec![at_start(look(f32::NAN, f32::NAN))],
        AngleOutOfRange { step: 0, angle: Angle::Yaw }
    )]
    #[case::max_yaw_before_max_pitch(
        vec![at_start(rotate(f32::NAN, f32::NAN))],
        AngleOutOfRange { step: 0, angle: Angle::MaxYaw }
    )]
    #[case::interval_before_jitter(
        vec![step(Action::Jump, every(DAY + ms(1), DAY + ms(1)))],
        IntervalTooLong { step: 0 }
    )]
    fn the_first_error_wins(#[case] steps: Vec<Step>, #[case] error: ModeError) {
        assert_eq!(validate(steps), Err(error));
    }

    #[rstest]
    #[case::chat_at_start(
        at_start(chat("/spawn")),
        at_start(chat("hello")),
        LimitedStep::StartChat
    )]
    #[case::repeating_chat(repeating(chat("a")), repeating(chat("b")), LimitedStep::RepeatingChat)]
    #[case::attack_at_start(
        at_start(Action::AttackFacingEntity),
        at_start(Action::AttackFacingEntity),
        LimitedStep::StartAttack
    )]
    #[case::repeating_attack(
        repeating(Action::AttackFacingEntity),
        repeating(Action::AttackFacingEntity),
        LimitedStep::RepeatingAttack
    )]
    #[case::hold_at_start(at_start(hold(true)), at_start(hold(true)), LimitedStep::StartHold)]
    #[case::repeating_hold(
        repeating(hold(true)),
        repeating(hold(true)),
        LimitedStep::RepeatingHold
    )]
    #[case::release_at_start(
        at_start(hold(false)),
        at_start(hold(false)),
        LimitedStep::StartRelease
    )]
    #[case::repeating_release(
        repeating(hold(false)),
        repeating(hold(false)),
        LimitedStep::RepeatingRelease
    )]
    fn rejects_a_second_limited_step(
        #[case] first: Step,
        #[case] second: Step,
        #[case] kind: LimitedStep,
    ) {
        let steps = vec![first, at_start(Action::Jump), second];
        assert_eq!(validate(steps), Err(DuplicateStep { step: 2, kind }));
    }

    #[test]
    fn accepts_one_limited_step_of_each_kind() {
        let steps = vec![
            at_start(chat("/spawn")),
            repeating(chat("/afk")),
            at_start(Action::AttackFacingEntity),
            repeating(Action::AttackFacingEntity),
            at_start(hold(true)),
            repeating(hold(true)),
            at_start(hold(false)),
            repeating(hold(false)),
        ];
        assert!(validate(steps).is_ok());
    }

    #[test]
    fn a_mode_can_shoot_a_bow_again_and_again() {
        // Draw at start; then, every 2 s, let go (the shot) and draw again.
        // Both repeating steps are due together and run in step order.
        let steps = vec![
            at_start(hold(true)),
            step(hold(false), every(ms(2_000), Duration::ZERO)),
            step(hold(true), every(ms(2_000), Duration::ZERO)),
        ];
        assert!(validate(steps).is_ok());
    }

    #[rstest]
    #[case::start_hold(LimitedStep::StartHold, "hold-at-start")]
    #[case::repeating_hold(LimitedStep::RepeatingHold, "repeating hold")]
    #[case::start_release(LimitedStep::StartRelease, "release-at-start")]
    #[case::repeating_release(LimitedStep::RepeatingRelease, "repeating release")]
    fn hold_use_kinds_are_named_in_errors(#[case] kind: LimitedStep, #[case] name: &str) {
        assert_eq!(kind.to_string(), name);
    }

    #[test]
    fn keeps_whole_milliseconds() {
        let definition = validate(vec![step(
            Action::Jump,
            every(
                Duration::from_micros(250_900),
                Duration::from_nanos(1_999_999),
            ),
        )])
        .unwrap();
        assert_eq!(definition.steps()[0].schedule, every(ms(250), ms(1)));
    }

    #[test]
    fn errors_name_the_step_and_never_echo_the_input() {
        assert_eq!(
            DuplicateStep {
                step: 3,
                kind: LimitedStep::StartChat
            }
            .to_string(),
            "step 3: a mode can have only one chat-at-start step"
        );
        assert_eq!(
            AngleOutOfRange {
                step: 1,
                angle: Angle::MaxPitch
            }
            .to_string(),
            "step 1: max_pitch is out of range"
        );
        assert_eq!(
            IntervalTooShort {
                step: 0,
                min: ms(30_000)
            }
            .to_string(),
            "step 0: the interval must be at least 30s"
        );
    }

    #[rstest]
    #[case::yaw(Angle::Yaw, "yaw")]
    #[case::pitch(Angle::Pitch, "pitch")]
    #[case::max_yaw(Angle::MaxYaw, "max_yaw")]
    #[case::max_pitch(Angle::MaxPitch, "max_pitch")]
    fn angles_are_named_like_their_json_fields(#[case] angle: Angle, #[case] name: &str) {
        assert_eq!(angle.as_str(), name);
        assert_eq!(angle.to_string(), name);
    }

    #[rstest]
    #[case::none(
        vec![at_start(chat("hello")), repeating(Action::Jump), at_start(hold(true))],
        &[]
    )]
    #[case::one(vec![at_start(chat("/spawn")), repeating(chat("hello"))], &["/spawn"])]
    #[case::in_step_order(
        vec![repeating(chat("/afk")), at_start(Action::Jump), at_start(chat("/home base"))],
        &["/afk", "/home base"]
    )]
    fn commands_lists_the_command_messages_in_step_order(
        #[case] steps: Vec<Step>,
        #[case] expected: &[&str],
    ) {
        let definition = validate(steps).unwrap();
        let commands: Vec<&str> = definition.commands().map(ChatMessage::as_str).collect();
        assert_eq!(commands, expected);
    }

    // --- Presets ---

    #[test]
    fn afk_preset_is_as_documented() {
        assert_eq!(
            ModeDefinition::afk().steps(),
            [
                step(rotate(30.0, 10.0), every(ms(45_000), ms(75_000))),
                step(Action::SwingArm, every(ms(20_000), ms(20_000))),
            ]
        );
    }

    #[test]
    fn farm_preset_is_as_documented() {
        assert_eq!(
            ModeDefinition::farm().steps(),
            [
                at_start(Action::SelectHotbarSlot {
                    slot: HotbarSlot::FIRST
                }),
                step(Action::AttackFacingEntity, every(ms(650), ms(150))),
            ]
        );
    }

    #[rstest]
    #[case::afk(ModeDefinition::afk())]
    #[case::farm(ModeDefinition::farm())]
    fn presets_pass_validation_unchanged(#[case] preset: ModeDefinition) {
        assert_eq!(ModeDraft::from(preset.clone()).validate(), Ok(preset));
    }

    #[test]
    fn afk_resets_the_idle_timer_more_often_than_every_60_s() {
        // Turning doesn't reset the server's idle timer; a swing, jump, sneak
        // or hotbar change does. 60 s is the shortest non-zero
        // `player-idle-timeout` (ADR-0008 §8).
        let resets_in_time = ModeDefinition::afk().steps().iter().any(|step| {
            let resets_timer = matches!(
                step.action,
                Action::SwingArm
                    | Action::Jump
                    | Action::Sneak { .. }
                    | Action::SelectHotbarSlot { .. }
            );
            let longest_gap = match step.schedule {
                Schedule::Every { interval, jitter } => Some(interval + jitter),
                Schedule::AtStart => None,
            };
            resets_timer
                && step.probability == 100
                && longest_gap.is_some_and(|gap| gap < Duration::from_secs(60))
        });
        assert!(resets_in_time);
    }

    // --- JSON ---

    const AFK_JSON: &str = concat!(
        r#"{"steps":["#,
        r#"{"action":{"type":"rotate_random","max_yaw":30.0,"max_pitch":10.0},"#,
        r#""schedule":{"type":"every","interval_ms":45000,"jitter_ms":75000},"probability":100},"#,
        r#"{"action":{"type":"swing_arm"},"#,
        r#""schedule":{"type":"every","interval_ms":20000,"jitter_ms":20000},"probability":100}"#,
        r#"]}"#
    );

    #[test]
    fn stores_the_documented_json() {
        assert_eq!(
            serde_json::to_string(&ModeDefinition::afk()).unwrap(),
            AFK_JSON
        );
        assert_eq!(
            serde_json::from_str::<ModeDefinition>(AFK_JSON).unwrap(),
            ModeDefinition::afk()
        );
    }

    #[test]
    fn afk_preset_json_is_stable() {
        let json = serde_json::to_string_pretty(&ModeDefinition::afk()).unwrap();
        insta::assert_snapshot!("afk_preset", json);
    }

    #[test]
    fn farm_preset_json_is_stable() {
        let json = serde_json::to_string_pretty(&ModeDefinition::farm()).unwrap();
        insta::assert_snapshot!("farm_preset", json);
    }

    #[test]
    fn every_tag_and_field_name_is_stable() {
        let draft = draft(vec![
            at_start(look(-90.5, 12.25)),
            step(rotate(30.0, 10.0), every(ms(45_000), ms(75_000))),
            at_start(Action::Jump),
            at_start(Action::Sneak { on: true }),
            at_start(Action::SwingArm),
            at_start(Action::UseItem),
            at_start(hold(true)),
            at_start(Action::AttackFacingEntity),
            at_start(Action::SelectHotbarSlot {
                slot: HotbarSlot::LAST,
            }),
            with_probability(25, repeating(chat("/spawn"))),
        ]);
        let json = serde_json::to_string_pretty(&draft).unwrap();
        insta::assert_snapshot!("all_variants", json);
    }

    #[test]
    fn reading_a_definition_validates_it() {
        let json = concat!(
            r#"{"steps":[{"action":{"type":"jump"},"#,
            r#""schedule":{"type":"every","interval_ms":100,"jitter_ms":0},"probability":100}]}"#
        );
        assert!(serde_json::from_str::<ModeDraft>(json).is_ok());
        assert!(serde_json::from_str::<ModeDefinition>(json).is_err());
    }

    const JUMP: &str = r#"{"type":"jump"}"#;
    const MINUTELY: &str = r#"{"type":"every","interval_ms":60000,"jitter_ms":0}"#;
    const CERTAIN: &str = r#","probability":100"#;

    fn mode_json(action: &str, schedule: &str, rest: &str) -> String {
        format!(r#"{{"steps":[{{"action":{action},"schedule":{schedule}{rest}}}]}}"#)
    }

    #[rstest]
    #[case::unknown_action(r#"{"type":"fly"}"#, MINUTELY, CERTAIN)]
    #[case::unknown_schedule(JUMP, r#"{"type":"hourly"}"#, CERTAIN)]
    #[case::missing_probability(JUMP, MINUTELY, "")]
    #[case::probability_above_255(JUMP, MINUTELY, r#","probability":256"#)]
    #[case::negative_probability(JUMP, MINUTELY, r#","probability":-1"#)]
    #[case::slot_above_8(r#"{"type":"select_hotbar_slot","slot":9}"#, MINUTELY, CERTAIN)]
    #[case::invalid_chat(r#"{"type":"send_chat","message":"§cred"}"#, MINUTELY, CERTAIN)]
    #[case::negative_interval(JUMP, r#"{"type":"every","interval_ms":-1,"jitter_ms":0}"#, CERTAIN)]
    #[case::fractional_interval(
        JUMP,
        r#"{"type":"every","interval_ms":1.5,"jitter_ms":0}"#,
        CERTAIN
    )]
    #[case::missing_jitter(JUMP, r#"{"type":"every","interval_ms":60000}"#, CERTAIN)]
    #[case::interval_without_unit(
        JUMP,
        r#"{"type":"every","interval":60000,"jitter_ms":0}"#,
        CERTAIN
    )]
    fn rejects_malformed_mode_json(
        #[case] action: &str,
        #[case] schedule: &str,
        #[case] rest: &str,
    ) {
        let json = mode_json(action, schedule, rest);
        assert!(serde_json::from_str::<ModeDraft>(&json).is_err(), "{json}");
    }

    #[test]
    fn a_mode_needs_its_steps() {
        assert!(serde_json::from_str::<ModeDraft>("{}").is_err());
    }

    #[test]
    fn ignores_unknown_fields_in_stored_modes() {
        let json = concat!(
            r#"{"name":"old","steps":["#,
            r#"{"action":{"type":"swing_arm","hand":"off"},"#,
            r#""schedule":{"type":"every","interval_ms":20000,"jitter_ms":500,"align":true},"#,
            r#""probability":100,"comment":"x"},"#,
            r#"{"action":{"type":"jump"},"schedule":{"type":"at_start","delay_ms":5},"probability":50}"#,
            r#"]}"#
        );
        assert_eq!(
            serde_json::from_str::<ModeDefinition>(json)
                .unwrap()
                .steps(),
            [
                step(Action::SwingArm, every(ms(20_000), ms(500))),
                with_probability(50, at_start(Action::Jump)),
            ]
        );
    }

    #[test]
    fn serializing_a_duration_beyond_u64_milliseconds_fails() {
        let draft = draft(vec![step(Action::Jump, every(Duration::MAX, ms(0)))]);
        assert!(serde_json::to_string(&draft).is_err());
    }

    #[test]
    fn serializes_the_longest_storable_duration() {
        let draft = draft(vec![step(Action::Jump, every(ms(u64::MAX), ms(0)))]);
        let json = serde_json::to_string(&draft).unwrap();
        assert_eq!(serde_json::from_str::<ModeDraft>(&json).unwrap(), draft);
    }

    #[test]
    fn try_from_validates_like_validate() {
        let valid = draft(vec![repeating(Action::Jump)]);
        assert_eq!(ModeDefinition::try_from(valid.clone()), valid.validate());
        assert_eq!(
            ModeDefinition::try_from(draft(vec![with_probability(0, repeating(Action::Jump))])),
            Err(InvalidProbability { step: 0 })
        );
    }

    #[test]
    fn a_definition_converts_back_to_its_draft() {
        let steps = vec![repeating(Action::Jump), at_start(chat("/spawn"))];
        assert_eq!(
            ModeDraft::from(validate(steps.clone()).unwrap()),
            draft(steps)
        );
    }

    // --- Properties ---

    /// Any `f32`, including NaN, the infinities and values just outside the
    /// ranges.
    fn any_angle() -> impl Strategy<Value = f32> {
        prop_oneof![
            any::<f32>(),
            Just(f32::NAN),
            Just(f32::INFINITY),
            Just(f32::NEG_INFINITY),
            -200.0_f32..200.0,
        ]
    }

    fn any_duration() -> impl Strategy<Value = Duration> {
        prop_oneof![
            (any::<u64>(), 0..1_000_000_000_u32)
                .prop_map(|(secs, nanos)| Duration::new(secs, nanos)),
            (0..=2 * DAY_MS).prop_map(ms),
        ]
    }

    fn chat_action() -> impl Strategy<Value = Action> {
        "/?[a-z ]{1,12}"
            .prop_filter_map("valid chat", |text| {
                ChatMessage::try_from(text.as_str()).ok()
            })
            .prop_map(|message| Action::SendChat { message })
    }

    fn slot_action() -> impl Strategy<Value = Action> {
        (0..=8_u8).prop_map(|n| Action::SelectHotbarSlot {
            slot: HotbarSlot::try_from(n).unwrap(),
        })
    }

    fn any_action() -> impl Strategy<Value = Action> {
        prop_oneof![
            (any_angle(), any_angle()).prop_map(|(yaw, pitch)| look(yaw, pitch)),
            (any_angle(), any_angle()).prop_map(|(max_yaw, max_pitch)| rotate(max_yaw, max_pitch)),
            Just(Action::Jump),
            any::<bool>().prop_map(|on| Action::Sneak { on }),
            Just(Action::SwingArm),
            Just(Action::UseItem),
            any::<bool>().prop_map(hold),
            Just(Action::AttackFacingEntity),
            slot_action(),
            chat_action(),
        ]
    }

    fn any_step() -> impl Strategy<Value = Step> {
        let schedule = prop_oneof![
            Just(Schedule::AtStart),
            (any_duration(), any_duration()).prop_map(|(interval, jitter)| every(interval, jitter)),
        ];
        (any_action(), schedule, any::<u8>()).prop_map(|(action, schedule, probability)| Step {
            action,
            schedule,
            probability,
        })
    }

    /// Whole milliseconds in `min..=max`, often exactly at the limits.
    fn millis_between(min: u64, max: u64) -> impl Strategy<Value = Duration> {
        prop_oneof![Just(min), Just(max), min..=max].prop_map(ms)
    }

    /// A valid angle in `-limit..=limit`, often exactly at the limits.
    fn valid_angle(limit: f32) -> impl Strategy<Value = f32> {
        prop_oneof![Just(-limit), Just(limit), Just(0.0), -limit..=limit]
    }

    /// A valid maximum in `0..=limit`, often exactly at the limits.
    fn valid_max(limit: f32) -> impl Strategy<Value = f32> {
        prop_oneof![Just(0.0), Just(limit), 0.0..=limit]
    }

    /// A valid action of a kind a mode may have any number of.
    fn valid_unlimited_action() -> impl Strategy<Value = Action> {
        prop_oneof![
            (valid_angle(180.0), valid_angle(90.0)).prop_map(|(yaw, pitch)| look(yaw, pitch)),
            (valid_max(180.0), valid_max(90.0))
                .prop_filter("turns", |(max_yaw, max_pitch)| *max_yaw > 0.0
                    || *max_pitch > 0.0)
                .prop_map(|(max_yaw, max_pitch)| rotate(max_yaw, max_pitch)),
            Just(Action::Jump),
            any::<bool>().prop_map(|on| Action::Sneak { on }),
            Just(Action::SwingArm),
            Just(Action::UseItem),
            slot_action(),
        ]
    }

    fn valid_probability() -> impl Strategy<Value = u8> {
        prop_oneof![Just(1_u8), Just(100_u8), 1..=100_u8]
    }

    fn valid_repeating(
        action: impl Strategy<Value = Action>,
        min_ms: u64,
    ) -> impl Strategy<Value = Step> {
        (
            action,
            millis_between(min_ms, DAY_MS),
            millis_between(0, DAY_MS),
            valid_probability(),
        )
            .prop_map(|(action, interval, jitter, probability)| Step {
                action,
                schedule: every(interval, jitter),
                probability,
            })
    }

    fn valid_at_start(action: impl Strategy<Value = Action>) -> impl Strategy<Value = Step> {
        (action, valid_probability()).prop_map(|(action, probability)| Step {
            action,
            schedule: Schedule::AtStart,
            probability,
        })
    }

    /// A valid draft: up to 24 unlimited steps plus at most one step of each
    /// of the 8 limited kinds, in any order.
    fn valid_draft() -> impl Strategy<Value = ModeDraft> {
        let unlimited = prop_oneof![
            valid_at_start(valid_unlimited_action()),
            valid_repeating(valid_unlimited_action(), 250),
        ];
        let limited = (
            proptest::option::of(valid_at_start(chat_action())),
            proptest::option::of(valid_repeating(chat_action(), 30_000)),
            proptest::option::of(valid_at_start(Just(Action::AttackFacingEntity))),
            proptest::option::of(valid_repeating(Just(Action::AttackFacingEntity), 500)),
            proptest::option::of(valid_at_start(Just(hold(true)))),
            proptest::option::of(valid_repeating(Just(hold(true)), 500)),
            proptest::option::of(valid_at_start(Just(hold(false)))),
            proptest::option::of(valid_repeating(Just(hold(false)), 500)),
        )
            .prop_map(|(a, b, c, d, e, f, g, h)| [a, b, c, d, e, f, g, h]);
        (vec(unlimited, 0..=24), limited)
            .prop_map(|(mut steps, limited)| {
                steps.extend(limited.into_iter().flatten());
                steps
            })
            .prop_shuffle()
            .prop_map(draft)
    }

    proptest! {
        #[test]
        fn validate_never_panics(steps in vec(any_step(), 0..=40)) {
            let _ = draft(steps).validate();
        }

        #[test]
        fn valid_drafts_pass_unchanged_and_round_trip_through_json(draft in valid_draft()) {
            let definition = draft.clone().validate().unwrap();
            prop_assert_eq!(definition.steps(), draft.steps.as_slice());

            let json = serde_json::to_string(&definition).unwrap();
            prop_assert_eq!(serde_json::from_str::<ModeDefinition>(&json).unwrap(), definition);
        }
    }
}
