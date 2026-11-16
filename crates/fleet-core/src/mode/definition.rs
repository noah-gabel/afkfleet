//! [`ModeDefinition`]: a validated mode, and [`ModeDraft`], its unvalidated
//! form.

use core::fmt;
use core::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Action, HotbarSlot};
use crate::script::Script;

/// When a [`Step`] runs.
///
/// In JSON a schedule is tagged with its `"type"`, and durations are whole
/// milliseconds: `{"type":"every","interval_ms":45000,"jitter_ms":75000}`
/// (ADR-0010). A server or agent that doesn't know a type refuses the mode
/// whole; it never runs its steps at another time.
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
    /// Once each time the bot is alive again after a death, as soon as the
    /// server confirmed the respawn the bot asked for, after the runner let
    /// go of the use button and sneak; never at start and never again on its
    /// own (ADR-0020, S20, S24). An on-respawn chat is skipped when the last
    /// one was sent less than [`ModeDefinition::MIN_CHAT_INTERVAL`] before.
    OnRespawn,
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

/// What a mode is made of: a list of steps, or a script (ADR-0036, D13).
#[derive(Debug, Clone, PartialEq)]
pub enum ModeBody {
    /// Steps, each an action with its schedule and probability.
    Steps(Vec<Step>),
    /// A script with its settings and declarations.
    Script(Box<Script>),
}

/// A validated mode: what a bot does while it's online.
///
/// Its body is steps or a script ([`ModeBody`]). The only ways to get one
/// are [`ModeDraft::validate`], the presets ([`ModeDefinition::afk`],
/// [`ModeDefinition::farm`]) and [`ModeDefinition::scripted`] with a
/// checked [`Script`], so every definition keeps the limits.
///
/// **Its JSON has `steps` or `script`, never both and never neither.** A
/// step mode's is a [`ModeDraft`]'s, unchanged. A scripted mode has
/// `script` and no `steps`, so a server from before scripts can't read it
/// and fails to load it, as with any new step type (ADR-0010), instead of
/// running a mode that does nothing. Reading validates again, so stored
/// modes can't skip the checks either.
///
/// **Whoever runs or sends a mode reads [`body`](Self::body).**
/// [`steps`](Self::steps) is empty for a scripted mode: enough to show a
/// step mode's steps, never to decide what a bot does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StoredMode", into = "StoredMode")]
pub struct ModeDefinition {
    body: ModeBody,
}

/// A mode's JSON: `steps` or `script`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    steps: Option<Vec<Step>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    script: Option<Script>,
}

/// Why a stored mode's JSON isn't a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum StoredModeError {
    /// The steps break a limit.
    #[error(transparent)]
    Steps(#[from] ModeError),
    /// Neither `steps` nor `script`.
    #[error("a mode needs its steps or its script")]
    NoBody,
    /// Both `steps` and `script`.
    #[error("a mode has steps or a script, not both")]
    TwoBodies,
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
/// every join or respawn. [`Action::HoldUse`] counts holding and letting go
/// apart, so a mode can let go and draw again on a schedule. Each schedule
/// counts apart (ADR-0020, S20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitedStep {
    /// [`Action::SendChat`] at start.
    StartChat,
    /// [`Action::SendChat`] repeating.
    RepeatingChat,
    /// [`Action::SendChat`] on respawn.
    RespawnChat,
    /// [`Action::AttackFacingEntity`] at start.
    StartAttack,
    /// [`Action::AttackFacingEntity`] repeating.
    RepeatingAttack,
    /// [`Action::AttackFacingEntity`] on respawn.
    RespawnAttack,
    /// [`Action::HoldUse`] holding (`on: true`) at start.
    StartHold,
    /// [`Action::HoldUse`] holding (`on: true`) repeating.
    RepeatingHold,
    /// [`Action::HoldUse`] holding (`on: true`) on respawn.
    RespawnHold,
    /// [`Action::HoldUse`] letting go (`on: false`) at start.
    StartRelease,
    /// [`Action::HoldUse`] letting go (`on: false`) repeating.
    RepeatingRelease,
    /// [`Action::HoldUse`] letting go (`on: false`) on respawn.
    RespawnRelease,
}

impl fmt::Display for LimitedStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StartChat => "chat-at-start",
            Self::RepeatingChat => "repeating chat",
            Self::RespawnChat => "chat-on-respawn",
            Self::StartAttack => "attack-at-start",
            Self::RepeatingAttack => "repeating attack",
            Self::RespawnAttack => "attack-on-respawn",
            Self::StartHold => "hold-at-start",
            Self::RepeatingHold => "repeating hold",
            Self::RespawnHold => "hold-on-respawn",
            Self::StartRelease => "release-at-start",
            Self::RepeatingRelease => "repeating release",
            Self::RespawnRelease => "release-on-respawn",
        })
    }
}

impl ModeDefinition {
    /// The most steps a mode may have.
    pub const MAX_STEPS: usize = 32;
    /// The shortest interval of a repeating step.
    pub const MIN_INTERVAL: Duration = Duration::from_millis(250);
    /// The shortest interval of a repeating [`Action::UseItem`]: a click
    /// every 4 game ticks, as often as a vanilla client clicks for a player
    /// who holds the use button down (ADR-0034).
    pub const MIN_USE_ITEM_INTERVAL: Duration = Duration::from_millis(200);
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

    /// A scripted mode: `script` is checked already, and whether its source
    /// is a valid program is `fleet-script`'s to say when the mode is saved.
    #[must_use]
    pub fn scripted(script: Script) -> Self {
        Self {
            body: ModeBody::Script(Box::new(script)),
        }
    }

    /// What the mode is made of.
    #[must_use]
    pub const fn body(&self) -> &ModeBody {
        &self.body
    }

    /// Returns a step mode's steps; none for a scripted mode.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        match &self.body {
            ModeBody::Steps(steps) => steps,
            ModeBody::Script(_) => &[],
        }
    }

    /// Returns a scripted mode's script; `None` for a step mode.
    #[must_use]
    pub fn script(&self) -> Option<&Script> {
        match &self.body {
            ModeBody::Steps(_) => None,
            ModeBody::Script(script) => Some(script),
        }
    }

    /// Returns the names of the `/commands` the mode can send, for the
    /// allowlist check (P2.9): a step mode's [`Action::SendChat`] messages
    /// that are commands, in step order, and a scripted mode's declared
    /// messages that are, in declared order. A name is what
    /// [`ChatMessage::command_name`](crate::chat::ChatMessage::command_name)
    /// gives: without its `/`, compared as it
    /// is. No value of a declared message changes one (ADR-0036, D8).
    pub fn command_names(&self) -> impl Iterator<Item = &str> {
        let declared = self.script().into_iter().flat_map(Script::command_names);
        // Every action is listed: one that sends text must show up here, or
        // it would get around the allowlist.
        let steps = self.steps().iter().filter_map(|step| match &step.action {
            Action::SendChat { message } => message.command_name(),
            Action::Look { .. }
            | Action::RotateRandom { .. }
            | Action::Jump
            | Action::Sneak { .. }
            | Action::SwingArm
            | Action::UseItem
            | Action::HoldUse { .. }
            | Action::AttackFacingEntity
            | Action::SelectHotbarSlot { .. }
            | Action::EnterMinecart => None,
        });
        steps.chain(declared)
    }

    /// Whether `self` and `other` read the same in an answer (ADR-0020,
    /// S21): the same steps in the same order, each with the same schedule,
    /// probability and action, a chat step's message compared as a viewer
    /// reads it (without its invisible characters, trimmed). An edit whose
    /// steps read the same, with the same name and visibility, changes
    /// nothing.
    ///
    /// Two scripted modes read the same when they're equal: a script's
    /// source and its declarations hold no invisible character, so they
    /// read as they're stored. A step mode and a scripted mode never do.
    ///
    /// It never says which steps to keep, and never feeds the allowlist,
    /// which compares a command's name as it is: `/spa\u{200B}wn` reads as
    /// `/spawn` but isn't that command.
    #[must_use]
    pub fn same_as_shown(&self, other: &Self) -> bool {
        match (&self.body, &other.body) {
            (ModeBody::Steps(steps), ModeBody::Steps(others)) => {
                steps.len() == others.len()
                    && steps.iter().zip(others).all(|(one, other)| {
                        one.schedule == other.schedule
                            && one.probability == other.probability
                            && same_action(&one.action, &other.action)
                    })
            }
            (ModeBody::Script(script), ModeBody::Script(other)) => script == other,
            (ModeBody::Steps(_), ModeBody::Script(_))
            | (ModeBody::Script(_), ModeBody::Steps(_)) => false,
        }
    }

    /// A step mode of `steps`, which the caller has checked.
    const fn of_steps(steps: Vec<Step>) -> Self {
        Self {
            body: ModeBody::Steps(steps),
        }
    }

    /// The built-in `afk` mode: keeps a bot from being kicked for idling.
    ///
    /// It turns by up to 30° left or right and 10° up or down every 45–120 s,
    /// and swings its arm every 20–40 s. Turning alone doesn't reset the
    /// server's idle timer, but a swing does, and 40 s is below the shortest
    /// non-zero `player-idle-timeout` of 60 s (ADR-0008 §8, ADR-0010).
    #[must_use]
    pub fn afk() -> Self {
        Self::of_steps(vec![
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
        ])
    }

    /// The built-in `farm` mode: attacks whatever the bot looks at.
    ///
    /// It selects the first hotbar slot when it joins and attacks every
    /// 650–800 ms. It doesn't turn: the server restores the account's saved
    /// direction at join. Any other slot or direction is a custom mode
    /// (ADR-0010).
    #[must_use]
    pub fn farm() -> Self {
        Self::of_steps(vec![
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
        ])
    }
}

impl ModeDraft {
    /// Checks the mode against the limits and returns it as a
    /// [`ModeDefinition`].
    ///
    /// The limits (Plan.md P2.7; ADR-0010):
    /// - at most [`ModeDefinition::MAX_STEPS`] steps; an empty mode is valid
    ///   and does nothing
    /// - a repeating step's interval is at least 250 ms (200 ms for a use,
    ///   500 ms for an attack or a hold-use, 30 s for chat) and at most 24 h;
    ///   its jitter is at most 24 h
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
        Ok(ModeDefinition::of_steps(steps))
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
        | Action::SendChat { .. }
        | Action::EnterMinecart => Ok(()),
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
/// returns the schedule with them. Every schedule is listed, so a new one
/// needs a decision about its limits.
fn check_schedule(
    index: usize,
    action: &Action,
    schedule: Schedule,
) -> Result<Schedule, ModeError> {
    let (interval, jitter) = match schedule {
        Schedule::Every { interval, jitter } => (whole_millis(interval), whole_millis(jitter)),
        // Once per join or confirmed respawn: no interval to check.
        Schedule::AtStart | Schedule::OnRespawn => return Ok(schedule),
    };
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
        Action::UseItem => ModeDefinition::MIN_USE_ITEM_INTERVAL,
        Action::Look { .. }
        | Action::RotateRandom { .. }
        | Action::Jump
        | Action::Sneak { .. }
        | Action::SwingArm
        | Action::SelectHotbarSlot { .. }
        | Action::EnterMinecart => ModeDefinition::MIN_INTERVAL,
    }
}

/// Which kind of step a mode may have only once this step is, if any. Every
/// action and every schedule is listed, so a new one needs a decision: a
/// schedule never falls into another's kinds.
const fn limited_kind(action: &Action, schedule: Schedule) -> Option<LimitedStep> {
    // The kinds at start, repeating and on respawn.
    let [start, repeating, respawn] = match action {
        Action::SendChat { .. } => [
            LimitedStep::StartChat,
            LimitedStep::RepeatingChat,
            LimitedStep::RespawnChat,
        ],
        Action::AttackFacingEntity => [
            LimitedStep::StartAttack,
            LimitedStep::RepeatingAttack,
            LimitedStep::RespawnAttack,
        ],
        Action::HoldUse { on: true } => [
            LimitedStep::StartHold,
            LimitedStep::RepeatingHold,
            LimitedStep::RespawnHold,
        ],
        Action::HoldUse { on: false } => [
            LimitedStep::StartRelease,
            LimitedStep::RepeatingRelease,
            LimitedStep::RespawnRelease,
        ],
        Action::Look { .. }
        | Action::RotateRandom { .. }
        | Action::Jump
        | Action::Sneak { .. }
        | Action::SwingArm
        | Action::UseItem
        | Action::SelectHotbarSlot { .. }
        | Action::EnterMinecart => return None,
    };
    Some(match schedule {
        Schedule::AtStart => start,
        Schedule::Every { .. } => repeating,
        Schedule::OnRespawn => respawn,
    })
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

impl TryFrom<StoredMode> for ModeDefinition {
    type Error = StoredModeError;

    fn try_from(stored: StoredMode) -> Result<Self, StoredModeError> {
        match (stored.steps, stored.script) {
            (Some(steps), None) => Ok(ModeDraft { steps }.validate()?),
            (None, Some(script)) => Ok(Self::scripted(script)),
            (None, None) => Err(StoredModeError::NoBody),
            (Some(_), Some(_)) => Err(StoredModeError::TwoBodies),
        }
    }
}

impl From<ModeDefinition> for StoredMode {
    fn from(definition: ModeDefinition) -> Self {
        match definition.body {
            ModeBody::Steps(steps) => Self {
                steps: Some(steps),
                script: None,
            },
            ModeBody::Script(script) => Self {
                steps: None,
                script: Some(*script),
            },
        }
    }
}

/// Whether two actions read the same in an answer: a chat message as a
/// viewer reads it, every other action as it is. Every kind is listed, so
/// one that carries text must choose its rule.
fn same_action(one: &Action, other: &Action) -> bool {
    match (one, other) {
        (Action::SendChat { message: one }, Action::SendChat { message: other }) => {
            one.shown_text() == other.shown_text()
        }
        (
            Action::SendChat { .. }
            | Action::Look { .. }
            | Action::RotateRandom { .. }
            | Action::Jump
            | Action::Sneak { .. }
            | Action::SwingArm
            | Action::UseItem
            | Action::HoldUse { .. }
            | Action::AttackFacingEntity
            | Action::SelectHotbarSlot { .. }
            | Action::EnterMinecart,
            _,
        ) => one == other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatMessage;
    use crate::script::{
        CaptureDraft, CaptureKind, DecimalMark, MessageDraft, PatternDraft, ScriptDraft,
        ValueDraft, ValueKind, When, draft_of,
    };
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

    fn on_respawn(action: Action) -> Step {
        step(action, Schedule::OnRespawn)
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
    #[case::shortest_use_interval(step(Action::UseItem, every(ms(200), ms(0))))]
    #[case::shortest_enter_minecart_interval(step(Action::EnterMinecart, every(ms(250), ms(0))))]
    #[case::enter_minecart_at_start(at_start(Action::EnterMinecart))]
    #[case::enter_minecart_on_respawn(on_respawn(Action::EnterMinecart))]
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
    #[case::use_below_200_ms(
        step(Action::UseItem, every(ms(199), ms(0))),
        IntervalTooShort { step: 0, min: ms(200) }
    )]
    #[case::enter_minecart_below_250_ms(
        step(Action::EnterMinecart, every(ms(249), ms(0))),
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
                min: ms(200)
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
    #[case::chat_on_respawn(
        on_respawn(chat("/home")),
        on_respawn(chat("back")),
        LimitedStep::RespawnChat
    )]
    #[case::attack_on_respawn(
        on_respawn(Action::AttackFacingEntity),
        on_respawn(Action::AttackFacingEntity),
        LimitedStep::RespawnAttack
    )]
    #[case::hold_on_respawn(
        on_respawn(hold(true)),
        on_respawn(hold(true)),
        LimitedStep::RespawnHold
    )]
    #[case::release_on_respawn(
        on_respawn(hold(false)),
        on_respawn(hold(false)),
        LimitedStep::RespawnRelease
    )]
    fn rejects_a_second_limited_step(
        #[case] first: Step,
        #[case] second: Step,
        #[case] kind: LimitedStep,
    ) {
        let steps = vec![first, at_start(Action::Jump), second];
        assert_eq!(validate(steps), Err(DuplicateStep { step: 2, kind }));
    }

    /// One of each limited kind, each schedule counted apart (ADR-0020, S20);
    /// an on-respawn step has no interval, so no minimum.
    #[test]
    fn accepts_one_limited_step_of_each_kind() {
        let steps = vec![
            at_start(chat("/spawn")),
            repeating(chat("/afk")),
            on_respawn(chat("/home")),
            at_start(Action::AttackFacingEntity),
            repeating(Action::AttackFacingEntity),
            on_respawn(Action::AttackFacingEntity),
            at_start(hold(true)),
            repeating(hold(true)),
            on_respawn(hold(true)),
            at_start(hold(false)),
            repeating(hold(false)),
            on_respawn(hold(false)),
        ];
        assert_eq!(validate(steps.clone()).unwrap().steps(), steps);
    }

    #[rstest]
    #[case::chat(LimitedStep::RespawnChat, "chat-on-respawn")]
    #[case::attack(LimitedStep::RespawnAttack, "attack-on-respawn")]
    #[case::hold(LimitedStep::RespawnHold, "hold-on-respawn")]
    #[case::release(LimitedStep::RespawnRelease, "release-on-respawn")]
    fn on_respawn_kinds_are_named_in_errors(#[case] kind: LimitedStep, #[case] name: &str) {
        assert_eq!(kind.to_string(), name);
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
    #[case::one(vec![at_start(chat("/spawn")), repeating(chat("hello"))], &["spawn"])]
    #[case::in_step_order(
        vec![repeating(chat("/afk")), at_start(Action::Jump), at_start(chat("/home base"))],
        &["afk", "home"]
    )]
    #[case::a_lone_slash_has_an_empty_name(vec![at_start(chat("/"))], &[""])]
    #[case::as_written(vec![at_start(chat("/spa\u{200B}wn x"))], &["spa\u{200B}wn"])]
    fn command_names_lists_the_chat_steps_commands_in_step_order(
        #[case] steps: Vec<Step>,
        #[case] expected: &[&str],
    ) {
        let definition = validate(steps).unwrap();
        let names: Vec<&str> = definition.command_names().collect();
        assert_eq!(names, expected);
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
        let again = draft(preset.steps().to_vec()).validate();

        assert_eq!(again, Ok(preset));
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
                Schedule::AtStart | Schedule::OnRespawn => None,
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
            on_respawn(chat("/home")),
            at_start(Action::EnterMinecart),
        ]);
        let json = serde_json::to_string_pretty(&draft).unwrap();
        insta::assert_snapshot!("all_variants", json);
    }

    /// An on-respawn step's schedule is the tag alone, read and written as
    /// itself (ADR-0020, S20).
    #[test]
    fn an_on_respawn_step_reads_and_writes_its_tag() {
        let json = mode_json(JUMP, r#"{"type":"on_respawn"}"#, CERTAIN);

        let definition = serde_json::from_str::<ModeDefinition>(&json).unwrap();

        assert_eq!(definition.steps(), [on_respawn(Action::Jump)]);
        assert_eq!(serde_json::to_string(&definition).unwrap(), json);
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
    fn a_step_mode_has_its_steps_and_no_script() {
        let steps = vec![repeating(Action::Jump), at_start(chat("/spawn"))];

        let definition = validate(steps.clone()).unwrap();

        assert_eq!(definition.steps(), steps);
        assert_eq!(definition.body(), &ModeBody::Steps(steps));
        assert_eq!(definition.script(), None);
    }

    // --- A scripted mode ---

    const FARM_SCRIPT: &str = "fn on_join(first) {\n    timer.every(1200, \"swing\");\n}\n";

    fn message(key: &str, text: &str) -> MessageDraft {
        MessageDraft {
            key: key.to_owned(),
            text: text.to_owned(),
            values: Vec::new(),
        }
    }

    /// A script that declares `messages`, each a key and its text.
    fn script_with(source: &str, messages: &[(&str, &str)]) -> Script {
        Script::try_from(ScriptDraft {
            messages: messages
                .iter()
                .map(|(key, text)| message(key, text))
                .collect(),
            ..draft_of(source)
        })
        .unwrap()
    }

    fn scripted(source: &str) -> ModeDefinition {
        ModeDefinition::scripted(script_with(source, &[]))
    }

    /// A scripted mode with one of everything a mode's JSON can hold: both
    /// settings off their defaults, a fixed message, a message with each
    /// kind of value, and a pattern of each kind with each kind of capture.
    fn every_part() -> ModeDefinition {
        let value = |name: &str, kind| ValueDraft {
            name: name.to_owned(),
            kind,
        };
        let capture = |name: &str, kind| CaptureDraft {
            name: name.to_owned(),
            kind,
        };
        let options = |words: &[&str]| words.iter().map(|&word| word.to_owned()).collect();
        let script = Script::try_from(ScriptDraft {
            api: 1,
            source: FARM_SCRIPT.to_owned(),
            leash: 16,
            tool_guard: 0,
            messages: vec![
                message("home", "/home farm"),
                MessageDraft {
                    key: "pay".to_owned(),
                    text: "/pay {to} {amount} x{times}".to_owned(),
                    values: vec![
                        value(
                            "to",
                            ValueKind::Word {
                                options: options(&["AfkMain", "AfkBot2"]),
                            },
                        ),
                        value(
                            "amount",
                            ValueKind::Decimal {
                                min: 0.01,
                                max: 1_000_000.0,
                                places: 2,
                            },
                        ),
                        value("times", ValueKind::Integer { min: 1, max: 64 }),
                    ],
                },
                message("thanks", "thank you"),
            ],
            patterns: vec![
                PatternDraft {
                    key: "balance".to_owned(),
                    when: When::Reply,
                    text: "Balance: ${amount}".to_owned(),
                    captures: vec![capture(
                        "amount",
                        CaptureKind::Amount {
                            min: 0.0,
                            max: 1_000_000_000.0,
                            decimal_mark: DecimalMark::Point,
                        },
                    )],
                },
                PatternDraft {
                    key: "shop".to_owned(),
                    when: When::AnyTime,
                    text: "Shop is {state} with {n} items".to_owned(),
                    captures: vec![
                        capture(
                            "state",
                            CaptureKind::Word {
                                options: options(&["open", "closed"]),
                            },
                        ),
                        capture("n", CaptureKind::Integer { min: 0, max: 64 }),
                    ],
                },
            ],
        })
        .unwrap();
        ModeDefinition::scripted(script)
    }

    #[test]
    fn a_scripted_mode_has_its_script_and_no_steps() {
        let script = script_with(FARM_SCRIPT, &[("home", "/home farm")]);

        let definition = ModeDefinition::scripted(script.clone());

        assert_eq!(definition.script(), Some(&script));
        assert_eq!(definition.body(), &ModeBody::Script(Box::new(script)));
        assert_eq!(definition.steps(), []);
    }

    /// Its JSON has `script` and no `steps` (ADR-0036, D13).
    #[test]
    fn a_scripted_modes_json_has_script_and_no_steps() {
        let definition = scripted(FARM_SCRIPT);

        let json = serde_json::to_value(&definition).unwrap();

        let fields: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(fields, ["script"]);
        assert_eq!(json["script"]["source"], FARM_SCRIPT);
        assert_eq!(json["script"]["api"], 1);
        assert_eq!(json["script"]["leash"], 8);
        assert_eq!(json["script"]["tool_guard"], 2);
    }

    /// A step mode's JSON is what it always was: `steps`, and nothing new.
    #[test]
    fn a_step_modes_json_has_steps_and_no_script() {
        let json = serde_json::to_value(ModeDefinition::farm()).unwrap();

        let fields: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(fields, ["steps"]);
    }

    #[test]
    fn a_scripted_mode_reads_back_from_its_json_as_itself() {
        for definition in [scripted(""), scripted(FARM_SCRIPT), every_part()] {
            let json = serde_json::to_string(&definition).unwrap();

            assert_eq!(
                serde_json::from_str::<ModeDefinition>(&json).unwrap(),
                definition
            );
        }
    }

    #[test]
    fn scripted_mode_json_is_stable() {
        let json = serde_json::to_string_pretty(&every_part()).unwrap();
        insta::assert_snapshot!("scripted_mode", json);
    }

    /// A server from before scripts reads a mode as a [`ModeDraft`], whose
    /// `steps` are required: it can't read a scripted mode, and so fails to
    /// load it instead of running a mode with no steps.
    #[test]
    fn a_reader_that_knows_only_steps_cant_read_a_scripted_mode() {
        let json = serde_json::to_string(&every_part()).unwrap();

        assert!(serde_json::from_str::<ModeDraft>(&json).is_err());
    }

    fn script_json(script: &str) -> String {
        format!(r#"{{"script":{script}}}"#)
    }

    const EMPTY_SCRIPT: &str =
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"messages":[],"patterns":[]}"#;

    #[test]
    fn reads_a_scripted_modes_documented_json() {
        let definition: ModeDefinition = serde_json::from_str(&script_json(EMPTY_SCRIPT)).unwrap();

        assert_eq!(definition, scripted(""));
        assert_eq!(
            serde_json::to_string(&definition).unwrap(),
            script_json(EMPTY_SCRIPT)
        );
    }

    #[rstest]
    #[case::neither_body("{}".to_owned())]
    #[case::both_bodies(format!(r#"{{"steps":[],"script":{EMPTY_SCRIPT}}}"#))]
    #[case::both_bodies_script_first(format!(r#"{{"script":{EMPTY_SCRIPT},"steps":[]}}"#))]
    #[case::a_script_that_is_null(r#"{"script":null}"#.to_owned())]
    #[case::steps_that_are_null(r#"{"steps":null}"#.to_owned())]
    #[case::a_script_as_text(r#"{"script":"fn on_join(first) {}"}"#.to_owned())]
    #[case::a_script_without_its_settings(r#"{"script":{"api":1,"source":""}}"#.to_owned())]
    #[case::a_newer_api_level(script_json(
        r#"{"api":2,"source":"","leash":8,"tool_guard":2,"messages":[],"patterns":[]}"#
    ))]
    #[case::a_leash_that_is_off(script_json(
        r#"{"api":1,"source":"","leash":0,"tool_guard":2,"messages":[],"patterns":[]}"#
    ))]
    #[case::a_source_with_a_hidden_character(script_json(
        r#"{"api":1,"source":"\u202e","leash":8,"tool_guard":2,"messages":[],"patterns":[]}"#
    ))]
    #[case::a_message_that_isnt_one(script_json(
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"messages":[{"key":"k","text":"{a}","values":[]}],"patterns":[]}"#
    ))]
    #[case::a_pattern_that_reads_free_text(script_json(
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"messages":[],"patterns":[{"key":"k","when":"reply","text":"Balance: {a}","captures":[{"name":"a","type":"text"}]}]}"#
    ))]
    fn refuses_a_stored_mode_that_isnt_one(#[case] json: String) {
        assert!(
            serde_json::from_str::<ModeDefinition>(&json).is_err(),
            "{json}"
        );
    }

    #[test]
    fn the_errors_of_a_modes_two_bodies_say_what_is_wrong() {
        let error = |json: &str| {
            serde_json::from_str::<ModeDefinition>(json)
                .unwrap_err()
                .to_string()
        };

        assert!(error("{}").starts_with("a mode needs its steps or its script"));
        assert!(
            error(&format!(r#"{{"steps":[],"script":{EMPTY_SCRIPT}}}"#))
                .starts_with("a mode has steps or a script, not both")
        );
    }

    #[test]
    fn a_scripted_modes_commands_are_its_declared_ones() {
        let definition = ModeDefinition::scripted(script_with(
            FARM_SCRIPT,
            &[
                ("greet", "hello all"),
                ("home", "/home farm"),
                ("sell", "/ah sell 5"),
                ("thanks", "thank you"),
            ],
        ));

        let names: Vec<&str> = definition.command_names().collect();

        assert_eq!(names, ["home", "ah"]);
        assert_eq!(scripted(FARM_SCRIPT).command_names().count(), 0);
    }

    #[test]
    fn a_scripted_mode_reads_the_same_only_as_itself() {
        let base = ModeDefinition::scripted(script_with(FARM_SCRIPT, &[("home", "/home farm")]));
        let with = |draft: ScriptDraft| ModeDefinition::scripted(Script::try_from(draft).unwrap());
        let base_draft = || ScriptDraft::from(base.script().unwrap().clone());
        let others = [
            scripted(FARM_SCRIPT),
            ModeDefinition::scripted(script_with("", &[("home", "/home farm")])),
            ModeDefinition::scripted(script_with(FARM_SCRIPT, &[("home", "/home base")])),
            ModeDefinition::scripted(script_with(FARM_SCRIPT, &[("house", "/home farm")])),
            with(ScriptDraft {
                leash: 9,
                ..base_draft()
            }),
            with(ScriptDraft {
                tool_guard: 3,
                ..base_draft()
            }),
            with(ScriptDraft {
                patterns: vec![PatternDraft {
                    key: "back".to_owned(),
                    when: When::Reply,
                    text: "Welcome back".to_owned(),
                    captures: Vec::new(),
                }],
                ..base_draft()
            }),
        ];

        assert!(base.same_as_shown(&base.clone()));
        assert!(every_part().same_as_shown(&every_part()));
        for other in others {
            assert!(!base.same_as_shown(&other), "{other:?}");
            assert!(!other.same_as_shown(&base), "{other:?}");
        }
    }

    #[test]
    fn a_step_mode_and_a_scripted_mode_never_read_the_same() {
        let empty_steps = validate(Vec::new()).unwrap();
        let empty_script = scripted("");

        assert!(!empty_steps.same_as_shown(&empty_script));
        assert!(!empty_script.same_as_shown(&empty_steps));
        assert_ne!(empty_steps, empty_script);
    }

    #[rstest]
    #[case::a_zero_width_space(chat("he\u{200B}llo"), chat("hello"), true)]
    #[case::a_word_joiner_and_a_byte_order_mark(
        chat("/spawn\u{2060} home\u{FEFF}"),
        chat("/spawn home"),
        true
    )]
    #[case::an_invisible_next_to_an_edge_space(chat("\u{200B} hello"), chat("hello"), true)]
    #[case::an_invisible_inside_a_command_name(chat("/spa\u{200B}wn"), chat("/spawn"), true)]
    #[case::another_message(chat("hello"), chat("hallo"), false)]
    #[case::another_case(chat("Hello"), chat("hello"), false)]
    #[case::only_invisible_against_text(chat("\u{200B}"), chat("x"), false)]
    #[case::negative_zero(look(-0.0, 0.0), look(0.0, -0.0), true)]
    #[case::another_angle(look(1.0, 0.0), look(2.0, 0.0), false)]
    #[case::another_turn(rotate(30.0, 10.0), rotate(30.0, 11.0), false)]
    #[case::another_kind(Action::Jump, Action::SwingArm, false)]
    #[case::sneaking_or_not(Action::Sneak { on: true }, Action::Sneak { on: false }, false)]
    #[case::holding_or_not(hold(true), hold(false), false)]
    #[case::another_slot(
        Action::SelectHotbarSlot { slot: HotbarSlot::try_from(1).unwrap() },
        Action::SelectHotbarSlot { slot: HotbarSlot::try_from(2).unwrap() },
        false
    )]
    #[case::the_same_slot(
        Action::SelectHotbarSlot { slot: HotbarSlot::FIRST },
        Action::SelectHotbarSlot { slot: HotbarSlot::FIRST },
        true
    )]
    fn an_action_reads_the_same_when_an_answer_shows_it_the_same(
        #[case] one: Action,
        #[case] other: Action,
        #[case] same: bool,
    ) {
        for schedule in [Schedule::AtStart, Schedule::OnRespawn] {
            let one = validate(vec![step(one.clone(), schedule)]).unwrap();
            let other = validate(vec![step(other.clone(), schedule)]).unwrap();

            assert_eq!(one.same_as_shown(&other), same, "{schedule:?}");
            assert_eq!(other.same_as_shown(&one), same, "{schedule:?}");
        }
    }

    #[test]
    fn steps_read_the_same_only_in_order_with_their_schedule_and_probability() {
        let base = validate(vec![repeating(Action::Jump), at_start(Action::SwingArm)]).unwrap();
        let others = [
            vec![at_start(Action::SwingArm), repeating(Action::Jump)],
            vec![
                step(Action::Jump, every(ms(60_001), Duration::ZERO)),
                at_start(Action::SwingArm),
            ],
            vec![
                step(Action::Jump, every(ms(60_000), ms(1))),
                at_start(Action::SwingArm),
            ],
            vec![
                with_probability(50, repeating(Action::Jump)),
                at_start(Action::SwingArm),
            ],
            vec![repeating(Action::Jump), repeating(Action::SwingArm)],
            vec![repeating(Action::Jump), on_respawn(Action::SwingArm)],
            vec![repeating(Action::Jump)],
            vec![],
        ];

        assert!(base.same_as_shown(&base.clone()));
        for steps in others {
            let other = validate(steps).unwrap();
            assert!(!base.same_as_shown(&other), "{other:?}");
            assert!(!other.same_as_shown(&base), "{other:?}");
        }
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
            Just(Action::EnterMinecart),
        ]
    }

    fn any_step() -> impl Strategy<Value = Step> {
        let schedule = prop_oneof![
            Just(Schedule::AtStart),
            (any_duration(), any_duration()).prop_map(|(interval, jitter)| every(interval, jitter)),
            Just(Schedule::OnRespawn),
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
            Just(Action::EnterMinecart),
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

    fn valid_on_respawn(action: impl Strategy<Value = Action>) -> impl Strategy<Value = Step> {
        (action, valid_probability()).prop_map(|(action, probability)| Step {
            action,
            schedule: Schedule::OnRespawn,
            probability,
        })
    }

    /// A valid draft: up to 20 unlimited steps plus at most one step of each
    /// of the 12 limited kinds, in any order. Boxed, so two drafts' value
    /// trees fit a test thread's stack on Windows.
    fn valid_draft() -> BoxedStrategy<ModeDraft> {
        let unlimited = prop_oneof![
            valid_at_start(valid_unlimited_action()),
            valid_repeating(valid_unlimited_action(), 250),
            valid_on_respawn(valid_unlimited_action()),
        ];
        let chat = (
            proptest::option::of(valid_at_start(chat_action())),
            proptest::option::of(valid_repeating(chat_action(), 30_000)),
            proptest::option::of(valid_on_respawn(chat_action())),
        );
        let attack = Just(Action::AttackFacingEntity);
        let attacks = (
            proptest::option::of(valid_at_start(attack.clone())),
            proptest::option::of(valid_repeating(attack.clone(), 500)),
            proptest::option::of(valid_on_respawn(attack)),
        );
        let holds = (
            proptest::option::of(valid_at_start(Just(hold(true)))),
            proptest::option::of(valid_repeating(Just(hold(true)), 500)),
            proptest::option::of(valid_on_respawn(Just(hold(true)))),
        );
        let releases = (
            proptest::option::of(valid_at_start(Just(hold(false)))),
            proptest::option::of(valid_repeating(Just(hold(false)), 500)),
            proptest::option::of(valid_on_respawn(Just(hold(false)))),
        );
        // One kind's three schedules as one list.
        let each = |(start, repeating, respawn)| [start, repeating, respawn];
        let limited = (
            chat.prop_map(each).boxed(),
            attacks.prop_map(each).boxed(),
            holds.prop_map(each).boxed(),
            releases.prop_map(each).boxed(),
        )
            .boxed();
        (vec(unlimited, 0..=20), limited)
            .prop_map(|(mut steps, (chat, attacks, holds, releases))| {
                for kind in [chat, attacks, holds, releases] {
                    steps.extend(kind.into_iter().flatten());
                }
                steps
            })
            .prop_shuffle()
            .prop_map(draft)
            .boxed()
    }

    /// `definition` with `invisible` inserted into each chat message, at the
    /// character `at` picks, and `-0.0` for each look's zero angle; and
    /// whether a message changed, which `==` tells apart while an answer
    /// shows it the same.
    fn shadow_of(
        definition: &ModeDefinition,
        invisible: char,
        at: usize,
    ) -> (ModeDefinition, bool) {
        let mut changed = false;
        let steps = definition
            .steps()
            .iter()
            .map(|step| {
                let action = match &step.action {
                    Action::SendChat { message } => {
                        let chars: Vec<char> = message.as_str().chars().collect();
                        let at = at % (chars.len() + 1);
                        let text: String = chars
                            .iter()
                            .take(at)
                            .chain(core::iter::once(&invisible))
                            .chain(chars.iter().skip(at))
                            .collect();
                        match ChatMessage::try_from(text.as_str()) {
                            Ok(message) => {
                                changed = true;
                                Action::SendChat { message }
                            }
                            Err(_) => step.action.clone(),
                        }
                    }
                    Action::Look { yaw, pitch } => {
                        let negated = |angle: f32| if angle.to_bits() == 0 { -0.0 } else { angle };
                        look(negated(*yaw), negated(*pitch))
                    }
                    other => other.clone(),
                };
                Step { action, ..*step }
            })
            .collect();
        (validate(steps).unwrap(), changed)
    }

    proptest! {
        /// Whatever a scripted mode's source and settings: it reads back
        /// from its JSON as itself, and never as a step mode.
        #[test]
        fn a_scripted_mode_round_trips_through_json(
            source in "[ -~
    ü]{0,60}",
            leash in 1_u8..=64,
            tool_guard in 0_u8..=100,
            command in "/?[a-z]{1,8}( [a-z]{1,8})?",
        ) {
            let script = Script::try_from(ScriptDraft {
                leash,
                tool_guard,
                messages: vec![message("say", &command)],
                ..draft_of(&source)
            })
            .unwrap();
            let definition = ModeDefinition::scripted(script);

            let json = serde_json::to_string(&definition).unwrap();
            let again: ModeDefinition = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(&again, &definition);
            prop_assert!(again.same_as_shown(&definition));
            prop_assert_eq!(again.steps(), []);
            prop_assert!(serde_json::from_str::<ModeDraft>(&json).is_err());
            prop_assert_eq!(again.command_names().count(), usize::from(command.starts_with('/')));
        }

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

        /// Every definition reads the same as itself, and as itself with an
        /// invisible character in each chat message or `-0.0` for a zero
        /// angle, which `==` may tell apart; and the rule doesn't depend on
        /// which side is the stored one (PR #64's review).
        #[test]
        fn reading_the_same_is_reflexive_and_symmetric(
            one in valid_draft(),
            other in valid_draft(),
            invisible in proptest::sample::select(vec!['\u{200B}', '\u{2060}', '\u{FEFF}']),
            at in any::<usize>(),
        ) {
            let one = one.validate().unwrap();
            let other = other.validate().unwrap();
            let (shadow, changed) = shadow_of(&one, invisible, at);

            prop_assert!(one.same_as_shown(&one));
            prop_assert!(one.same_as_shown(&one.clone()));
            prop_assert!(one.same_as_shown(&shadow));
            prop_assert!(shadow.same_as_shown(&one));
            prop_assert_eq!(shadow == one, !changed);
            prop_assert_eq!(one.same_as_shown(&other), other.same_as_shown(&one));
        }
    }
}
