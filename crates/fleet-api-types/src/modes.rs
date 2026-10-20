//! The modes (Plan.md P11.4; ADR-0010; ADR-0020, S12–S16, S19, U8): what a
//! bot does while it's online, as users read and write them.
//!
//! - **`GET /modes`** takes a [`crate::PageQuery`] of mode IDs and answers a
//!   [`ModePage`] of the modes the caller may see, newest first, each a
//!   [`ModeResponse`] built for its viewer (S14, S15).
//! - **`POST /modes`** takes a [`CreateModeRequest`]. A mode whose steps
//!   send a command off the server's allowlist is written only through
//!   `POST /modes/with-commands`, after a step-up; the plain route answers
//!   403 `mode_needs_step_up` (S16).
//! - **The steps** are ADR-0010's mode JSON
//!   (`{"action":{"type":"look","yaw":0.0,"pitch":0.0},"schedule":{"type":"every","interval_ms":…,"jitter_ms":…},"probability":…}`)
//!   as flat types (S12): each action and schedule has a `type` and the
//!   fields that type takes. A request that doesn't parse (an unknown field
//!   or `type`, a value of the wrong JSON type, a number its type can't hold)
//!   is the server's bare 400, which never repeats what it read. Every other
//!   rule, fleet-core's included, is one field error at the field it names,
//!   such as `steps[3].action.message` ([`ModeSteps::definition`]).

use core::time::Duration;

use fleet_core::authz::ModeVisibility as CoreVisibility;
use fleet_core::chat::{ChatMessage, Redacted};
use fleet_core::mode::{
    Action, Angle, HotbarSlot, HotbarSlotError, LimitedStep, ModeDefinition, ModeDraft, ModeError,
    ModeName, ModeNameError, Schedule, Step,
};
use garde::{Error, Path, Report, Validate};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema as _, ToSchema};

use crate::chat::message_rule;
use crate::ids::{ModeId, id_schema};
use crate::int::integer_schema;
use crate::open::{Open, OpenEnum};
use crate::text::BoundedText;
use crate::time::Timestamp;
use crate::users::UserLabel;

/// The most characters of a mode's name (`ModeName::MAX_LEN`).
pub const MODE_NAME_MAX_CHARS: usize = ModeName::MAX_LEN;

/// The most steps of a mode (`ModeDefinition::MAX_STEPS`).
pub const MODE_MAX_STEPS: usize = ModeDefinition::MAX_STEPS;

/// The most characters of a chat step's message as a response shows it: a
/// message is at most `ChatMessage::MAX_LEN` UTF-16 code units, so never
/// more characters.
pub const MODE_MESSAGE_MAX_CHARS: usize = ChatMessage::MAX_LEN;

/// The longest interval or jitter a request can carry, in milliseconds: a
/// day's (`ModeDefinition::MAX_INTERVAL`), which a `u32` holds; a longer one
/// is a field error, a number past `u32` the bare 400.
const MAX_MS: u32 = u32::MAX;

/// The field message of a name taken by another mode (S13), which only the
/// server can tell: another of the owner's modes, or, for a shared one, a
/// shared mode, has it.
pub const MODE_NAME_TAKEN: &str = "is already another mode's name";

/// The field message of a built-in mode's name (S13, R4).
pub const MODE_NAME_RESERVED: &str = "is a built-in mode's name";

/// The field message of too many steps.
const TOO_MANY_STEPS: &str = "must hold at most 32 steps";
/// The field message of a field its step's type needs but doesn't have.
const REQUIRED: &str = "is required for this type";
/// The field message of a field its step's type doesn't take.
const NOT_USED: &str = "isn't used by this type";
/// The field message of a hotbar slot past the last (the user's wording at
/// group C's plan review).
const SLOT: &str = "must be a hotbar slot from 0 to 8 (the keys 1 to 9)";
/// The field message of a turn that turns nothing.
const NO_TURN: &str = "must turn: the largest yaw and pitch can't both be 0";
/// The field message of a probability outside 1–100.
const PROBABILITY: &str = "must be from 1 to 100 percent";
/// The field message of an interval or jitter longer than a day.
const AT_MOST_A_DAY: &str = "must be at most 24 hours";

/// Who may see a mode: its owner (and those who manage the owner's
/// accounts: the Owner, and Admins for Members' modes), or everyone. A
/// built-in mode is shared.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeVisibility {
    /// Seen by its owner, the Owner, and Admins for a Member's mode.
    Private,
    /// Seen by everyone; only Admins and the Owner create one.
    Shared,
}

impl OpenEnum for ModeVisibility {
    const ALL: &'static [Self] = &[Self::Private, Self::Shared];

    fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Shared => "shared",
        }
    }
}

impl From<CoreVisibility> for ModeVisibility {
    fn from(visibility: CoreVisibility) -> Self {
        match visibility {
            CoreVisibility::Private => Self::Private,
            CoreVisibility::Shared => Self::Shared,
        }
    }
}

impl From<ModeVisibility> for CoreVisibility {
    fn from(visibility: ModeVisibility) -> Self {
        match visibility {
            ModeVisibility::Private => Self::Private,
            ModeVisibility::Shared => Self::Shared,
        }
    }
}

/// What a step does: its action's `type` (ADR-0010).
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeActionKind {
    /// Look in a direction: `yaw` and `pitch`, in degrees.
    Look,
    /// Turn by a random amount: up to `max_yaw` and `max_pitch` degrees
    /// either way.
    RotateRandom,
    /// Jump once.
    Jump,
    /// Start (`on: true`) or stop sneaking.
    Sneak,
    /// Swing the main hand.
    SwingArm,
    /// Use the held item once.
    UseItem,
    /// Hold the use button down (`on: true`) or let go of it.
    HoldUse,
    /// Attack the entity in front.
    AttackFacingEntity,
    /// Select a hotbar slot: `slot`, 0 to 8 (the keys 1 to 9).
    SelectHotbarSlot,
    /// Send chat or a command: `message`.
    SendChat,
}

impl OpenEnum for ModeActionKind {
    const ALL: &'static [Self] = &[
        Self::Look,
        Self::RotateRandom,
        Self::Jump,
        Self::Sneak,
        Self::SwingArm,
        Self::UseItem,
        Self::HoldUse,
        Self::AttackFacingEntity,
        Self::SelectHotbarSlot,
        Self::SendChat,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Look => "look",
            Self::RotateRandom => "rotate_random",
            Self::Jump => "jump",
            Self::Sneak => "sneak",
            Self::SwingArm => "swing_arm",
            Self::UseItem => "use_item",
            Self::HoldUse => "hold_use",
            Self::AttackFacingEntity => "attack_facing_entity",
            Self::SelectHotbarSlot => "select_hotbar_slot",
            Self::SendChat => "send_chat",
        }
    }
}

impl From<&Action> for ModeActionKind {
    fn from(action: &Action) -> Self {
        match action {
            Action::Look { .. } => Self::Look,
            Action::RotateRandom { .. } => Self::RotateRandom,
            Action::Jump => Self::Jump,
            Action::Sneak { .. } => Self::Sneak,
            Action::SwingArm => Self::SwingArm,
            Action::UseItem => Self::UseItem,
            Action::HoldUse { .. } => Self::HoldUse,
            Action::AttackFacingEntity => Self::AttackFacingEntity,
            Action::SelectHotbarSlot { .. } => Self::SelectHotbarSlot,
            Action::SendChat { .. } => Self::SendChat,
        }
    }
}

/// When a step runs: its schedule's `type` (ADR-0010).
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeScheduleKind {
    /// Once, right after the bot joins.
    AtStart,
    /// Again and again: each gap is `interval_ms` plus up to `jitter_ms`,
    /// the first one after the join.
    Every,
}

impl OpenEnum for ModeScheduleKind {
    const ALL: &'static [Self] = &[Self::AtStart, Self::Every];

    fn as_str(self) -> &'static str {
        match self {
            Self::AtStart => "at_start",
            Self::Every => "every",
        }
    }
}

impl From<&Schedule> for ModeScheduleKind {
    fn from(schedule: &Schedule) -> Self {
        match schedule {
            Schedule::AtStart => Self::AtStart,
            Schedule::Every { .. } => Self::Every,
        }
    }
}

/// `POST /modes`: a new mode, the caller's own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateModeRequest {
    /// Its name: 1 to 32 letters, digits, plain spaces and punctuation once
    /// trimmed, never a built-in mode's, and not one of the owner's other
    /// modes', nor, for a shared mode, another shared mode's, ASCII case
    /// ignored.
    #[garde(custom(valid_mode_name))]
    #[schema(schema_with = BoundedText::<MODE_NAME_MAX_CHARS>::schema)]
    pub name: String,
    /// Who may see it; a shared mode needs an Admin or the Owner.
    #[garde(skip)]
    pub visibility: ModeVisibility,
    /// Its steps, at most 32, in the order they run when due together.
    #[garde(dive)]
    #[ts(as = "Vec<ModeStepRequest>")]
    #[schema(value_type = Vec<ModeStepRequest>)]
    pub steps: ModeSteps,
}

/// The garde rule of a mode's name: a valid `ModeName` that isn't a
/// built-in mode's. The message names the rule, never the name.
fn valid_mode_name<C>(text: &str, _context: &C) -> garde::Result {
    let name = ModeName::try_from(text).map_err(|error| Error::new(name_rule(error)))?;
    match name.preset() {
        Some(_) => Err(Error::new(MODE_NAME_RESERVED)),
        None => Ok(()),
    }
}

/// The field message of each way a name can be invalid.
const fn name_rule(error: ModeNameError) -> &'static str {
    match error {
        ModeNameError::Empty => "must not be empty",
        ModeNameError::ForbiddenChar { .. } => {
            "must hold only letters, digits, plain spaces and punctuation, without emoji or other symbols"
        }
        ModeNameError::TooLong { .. } => "must be at most 32 characters",
    }
}

/// A mode's steps in a request. On the wire it's the plain list of
/// [`ModeStepRequest`]s; its rules are checked by [`ModeSteps::definition`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModeSteps(pub Vec<ModeStepRequest>);

/// One step of a mode in a request: what it does, when, and how likely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModeStepRequest {
    /// What it does.
    pub action: ModeActionRequest,
    /// When it runs.
    pub schedule: ModeScheduleRequest,
    /// How likely it runs when due, in percent: 1 to 100.
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub probability: u8,
}

/// A step's action in a request: its `type` and the fields that type takes,
/// no others (see `ModeActionKind`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModeActionRequest {
    /// What it does.
    #[serde(rename = "type")]
    pub kind: ModeActionKind,
    /// `look`: the direction, −180 to 180 degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<-180, 180>, nullable = false)]
    pub yaw: Option<f32>,
    /// `look`: up or down, −90 to 90 degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<-90, 90>, nullable = false)]
    pub pitch: Option<f32>,
    /// `rotate_random`: the most it turns either way, 0 to 180 degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<0, 180>, nullable = false)]
    pub max_yaw: Option<f32>,
    /// `rotate_random`: the most it tilts either way, 0 to 90 degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<0, 90>, nullable = false)]
    pub max_pitch: Option<f32>,
    /// `sneak` and `hold_use`: start (true) or stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub on: Option<bool>,
    /// `select_hotbar_slot`: the slot, 0 to 8 (the keys 1 to 9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub slot: Option<u8>,
    /// `send_chat`: the line, as `POST /bots/{id}/chat` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = BoundedText::<MODE_MESSAGE_MAX_CHARS>::schema, nullable = false)]
    pub message: Option<String>,
}

/// A step's schedule in a request: its `type` and the fields that type
/// takes, no others (see `ModeScheduleKind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ModeScheduleRequest {
    /// When it runs.
    #[serde(rename = "type")]
    pub kind: ModeScheduleKind,
    /// `every`: the shortest gap, in milliseconds: at least 250 (500 to
    /// attack or use the use button, 30,000 to chat), at most a day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, MAX_MS>, nullable = false)]
    pub interval_ms: Option<u32>,
    /// `every`: how much longer a gap may be, in milliseconds, at most a
    /// day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, MAX_MS>, nullable = false)]
    pub jitter_ms: Option<u32>,
}

/// The schema of an `f32` field within `MIN..=MAX`, such as an angle: a
/// number with its range and no `format`, as [`integer_schema`] is for
/// integers. ts-rs exports such a field as `number` already.
#[must_use]
pub fn number_schema<const MIN: i32, const MAX: i32>() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::Number)
        .minimum(Some(MIN))
        .maximum(Some(MAX))
        .into()
}

/// Which field of a mode's steps broke a rule: [`ModeFieldError::path`]
/// writes it as garde does, under the request's `steps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeField {
    /// The list itself (too many steps).
    Steps,
    /// A whole step (a second step of a kind a mode has only one of).
    Step(usize),
    /// A step's action as a whole (a turn that turns nothing).
    Action(usize),
    /// A field of a step's action, by its JSON name.
    ActionField(usize, &'static str),
    /// A step's probability.
    Probability(usize),
    /// A field of a step's schedule, by its JSON name.
    ScheduleField(usize, &'static str),
}

/// A rule a mode's steps broke: where, and the field message, which names
/// the rule, never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeFieldError {
    /// Where.
    pub field: ModeField,
    /// The field message.
    pub message: &'static str,
}

impl ModeFieldError {
    /// The field's path under `steps`, the request's own path for its list:
    /// `steps`, `steps[2]`, `steps[2].action.yaw`, `steps[2].probability`.
    #[must_use]
    pub fn path(&self, steps: &Path) -> Path {
        match self.field {
            ModeField::Steps => steps.clone(),
            ModeField::Step(step) => steps.join(step),
            ModeField::Action(step) => steps.join(step).join("action"),
            ModeField::ActionField(step, name) => steps.join(step).join("action").join(name),
            ModeField::Probability(step) => steps.join(step).join("probability"),
            ModeField::ScheduleField(step, name) => steps.join(step).join("schedule").join(name),
        }
    }

    /// A rule broken at `field`.
    const fn at(field: ModeField, message: &'static str) -> Self {
        Self { field, message }
    }
}

/// The action fields, in the order they're checked, each with whether
/// `request` has it.
fn action_fields(request: &ModeActionRequest) -> [(&'static str, bool); 7] {
    [
        ("yaw", request.yaw.is_some()),
        ("pitch", request.pitch.is_some()),
        ("max_yaw", request.max_yaw.is_some()),
        ("max_pitch", request.max_pitch.is_some()),
        ("on", request.on.is_some()),
        ("slot", request.slot.is_some()),
        ("message", request.message.is_some()),
    ]
}

/// The fields an action of `kind` takes.
const fn action_takes(kind: ModeActionKind) -> &'static [&'static str] {
    match kind {
        ModeActionKind::Look => &["yaw", "pitch"],
        ModeActionKind::RotateRandom => &["max_yaw", "max_pitch"],
        ModeActionKind::Sneak | ModeActionKind::HoldUse => &["on"],
        ModeActionKind::SelectHotbarSlot => &["slot"],
        ModeActionKind::SendChat => &["message"],
        ModeActionKind::Jump
        | ModeActionKind::SwingArm
        | ModeActionKind::UseItem
        | ModeActionKind::AttackFacingEntity => &[],
    }
}

/// The schedule fields, in the order they're checked, each with whether
/// `request` has it.
const fn schedule_fields(request: &ModeScheduleRequest) -> [(&'static str, bool); 2] {
    [
        ("interval_ms", request.interval_ms.is_some()),
        ("jitter_ms", request.jitter_ms.is_some()),
    ]
}

/// The fields a schedule of `kind` takes.
const fn schedule_takes(kind: ModeScheduleKind) -> &'static [&'static str] {
    match kind {
        ModeScheduleKind::AtStart => &[],
        ModeScheduleKind::Every => &["interval_ms", "jitter_ms"],
    }
}

/// The first field of `fields` that's missing although `takes` names it, or
/// there although `takes` doesn't, as `at` names it.
fn first_misplaced(
    fields: &[(&'static str, bool)],
    takes: &[&str],
    at: impl Fn(&'static str) -> ModeField,
) -> Result<(), ModeFieldError> {
    for &(name, present) in fields {
        match (takes.contains(&name), present) {
            (true, false) => return Err(ModeFieldError::at(at(name), REQUIRED)),
            (false, true) => return Err(ModeFieldError::at(at(name), NOT_USED)),
            (true, true) | (false, false) => {}
        }
    }
    Ok(())
}

/// `value`, which the step's type needs, or the field error that says so.
fn needed<T: Copy>(value: Option<T>, field: ModeField) -> Result<T, ModeFieldError> {
    value.ok_or(ModeFieldError::at(field, REQUIRED))
}

/// Step `step`'s action, or the first of its own fields' rules it breaks.
fn action(step: usize, request: &ModeActionRequest) -> Result<Action, ModeFieldError> {
    let field = |name| ModeField::ActionField(step, name);
    first_misplaced(&action_fields(request), action_takes(request.kind), field)?;
    Ok(match request.kind {
        ModeActionKind::Look => Action::Look {
            yaw: needed(request.yaw, field("yaw"))?,
            pitch: needed(request.pitch, field("pitch"))?,
        },
        ModeActionKind::RotateRandom => Action::RotateRandom {
            max_yaw: needed(request.max_yaw, field("max_yaw"))?,
            max_pitch: needed(request.max_pitch, field("max_pitch"))?,
        },
        ModeActionKind::Jump => Action::Jump,
        ModeActionKind::Sneak => Action::Sneak {
            on: needed(request.on, field("on"))?,
        },
        ModeActionKind::SwingArm => Action::SwingArm,
        ModeActionKind::UseItem => Action::UseItem,
        ModeActionKind::HoldUse => Action::HoldUse {
            on: needed(request.on, field("on"))?,
        },
        ModeActionKind::AttackFacingEntity => Action::AttackFacingEntity,
        ModeActionKind::SelectHotbarSlot => Action::SelectHotbarSlot {
            slot: HotbarSlot::try_from(needed(request.slot, field("slot"))?).map_err(|error| {
                match error {
                    HotbarSlotError::OutOfRange => ModeFieldError::at(field("slot"), SLOT),
                }
            })?,
        },
        ModeActionKind::SendChat => Action::SendChat {
            message: ChatMessage::try_from(
                request
                    .message
                    .as_deref()
                    .ok_or(ModeFieldError::at(field("message"), REQUIRED))?,
            )
            .map_err(|error| ModeFieldError::at(field("message"), message_rule(error)))?,
        },
    })
}

/// Step `step`'s schedule, or the first of its own fields' rules it breaks.
fn schedule(step: usize, request: &ModeScheduleRequest) -> Result<Schedule, ModeFieldError> {
    let field = |name| ModeField::ScheduleField(step, name);
    first_misplaced(
        &schedule_fields(request),
        schedule_takes(request.kind),
        field,
    )?;
    Ok(match request.kind {
        ModeScheduleKind::AtStart => Schedule::AtStart,
        ModeScheduleKind::Every => Schedule::Every {
            interval: Duration::from_millis(u64::from(needed(
                request.interval_ms,
                field("interval_ms"),
            )?)),
            jitter: Duration::from_millis(u64::from(needed(
                request.jitter_ms,
                field("jitter_ms"),
            )?)),
        },
    })
}

/// fleet-core's rule `error` as the field error of the step it names; the
/// steps tell which action an interval belongs to.
fn core_rule(error: ModeError, steps: &[ModeStepRequest]) -> ModeFieldError {
    match error {
        ModeError::TooManySteps { .. } => ModeFieldError::at(ModeField::Steps, TOO_MANY_STEPS),
        ModeError::IntervalTooShort { step, .. } => {
            let message = steps
                .get(step)
                .map_or(interval_rule(ModeActionKind::Jump), |request| {
                    interval_rule(request.action.kind)
                });
            ModeFieldError::at(ModeField::ScheduleField(step, "interval_ms"), message)
        }
        ModeError::IntervalTooLong { step } => {
            ModeFieldError::at(ModeField::ScheduleField(step, "interval_ms"), AT_MOST_A_DAY)
        }
        ModeError::JitterTooLong { step } => {
            ModeFieldError::at(ModeField::ScheduleField(step, "jitter_ms"), AT_MOST_A_DAY)
        }
        ModeError::InvalidProbability { step } => {
            ModeFieldError::at(ModeField::Probability(step), PROBABILITY)
        }
        ModeError::AngleOutOfRange { step, angle } => ModeFieldError::at(
            ModeField::ActionField(step, angle.as_str()),
            angle_rule(angle),
        ),
        ModeError::NoRotation { step } => ModeFieldError::at(ModeField::Action(step), NO_TURN),
        ModeError::DuplicateStep { step, kind } => {
            ModeFieldError::at(ModeField::Step(step), duplicate_rule(kind))
        }
    }
}

/// The field message of an angle out of its range.
const fn angle_rule(angle: Angle) -> &'static str {
    match angle {
        Angle::Yaw => "must be from -180 to 180 degrees",
        Angle::Pitch => "must be from -90 to 90 degrees",
        Angle::MaxYaw => "must be from 0 to 180 degrees",
        Angle::MaxPitch => "must be from 0 to 90 degrees",
    }
}

/// The field message of a second step of a kind a mode has only one of.
const fn duplicate_rule(kind: LimitedStep) -> &'static str {
    match kind {
        LimitedStep::StartChat => "is a second chat step at start; a mode can have only one",
        LimitedStep::RepeatingChat => "is a second repeating chat step; a mode can have only one",
        LimitedStep::StartAttack => "is a second attack step at start; a mode can have only one",
        LimitedStep::RepeatingAttack => {
            "is a second repeating attack step; a mode can have only one"
        }
        LimitedStep::StartHold => {
            "is a second step at start that holds the use button; a mode can have only one"
        }
        LimitedStep::RepeatingHold => {
            "is a second repeating step that holds the use button; a mode can have only one"
        }
        LimitedStep::StartRelease => {
            "is a second step at start that lets go of the use button; a mode can have only one"
        }
        LimitedStep::RepeatingRelease => {
            "is a second repeating step that lets go of the use button; a mode can have only one"
        }
    }
}

/// The field message of an interval shorter than its action allows:
/// fleet-core's minimum for that action (`ModeDefinition::MIN_INTERVAL`,
/// `MIN_ATTACK_INTERVAL`, `MIN_HOLD_USE_INTERVAL`, `MIN_CHAT_INTERVAL`).
const fn interval_rule(kind: ModeActionKind) -> &'static str {
    match kind {
        ModeActionKind::SendChat => "must be at least 30 seconds for chat",
        ModeActionKind::AttackFacingEntity | ModeActionKind::HoldUse => {
            "must be at least 500 ms for attacking or the use button"
        }
        ModeActionKind::Look
        | ModeActionKind::RotateRandom
        | ModeActionKind::Jump
        | ModeActionKind::Sneak
        | ModeActionKind::SwingArm
        | ModeActionKind::UseItem
        | ModeActionKind::SelectHotbarSlot => "must be at least 250 ms",
    }
}

impl ModeSteps {
    /// The mode these steps make, or the first rule they break, in step
    /// order: a step's own fields first (one its type needs, or doesn't
    /// take, a message, a slot), then fleet-core's rules for it (an angle,
    /// the probability, the schedule, a second step of a limited kind).
    /// Too many steps is named before any step. The server reads the steps
    /// through this alone, so what passes here is exactly what's stored.
    ///
    /// # Errors
    /// The first [`ModeFieldError`].
    pub fn definition(&self) -> Result<ModeDefinition, ModeFieldError> {
        if self.0.len() > MODE_MAX_STEPS {
            return Err(ModeFieldError::at(ModeField::Steps, TOO_MANY_STEPS));
        }
        let mut draft = ModeDraft::default();
        for (index, request) in self.0.iter().enumerate() {
            draft.steps.push(Step {
                action: action(index, &request.action)?,
                schedule: schedule(index, &request.schedule)?,
                probability: request.probability,
            });
            // fleet-core names the first rule broken in step order, and the
            // steps before this one passed, so a rule it names now is this
            // step's: the earliest step's rule comes first, whichever kind.
            draft
                .clone()
                .validate()
                .map_err(|error| core_rule(error, &self.0))?;
        }
        draft.validate().map_err(|error| core_rule(error, &self.0))
    }
}

/// garde can't express the steps' rules, which span fields and steps, so
/// [`ModeSteps::definition`] checks them and reports the first one broken
/// at its own path.
impl Validate for ModeSteps {
    type Context = ();

    fn validate_into(&self, _context: &(), parent: &mut dyn FnMut() -> Path, report: &mut Report) {
        if let Err(error) = self.definition() {
            report.append(error.path(&parent()), Error::new(error.message));
        }
    }
}

/// Whose a mode is, as every viewer may know it (S14): who exactly owns
/// someone else's is the mode's `owner`, for Admins and the Owner only.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeOwnership {
    /// Built in: no owner, read-only for everyone.
    BuiltIn,
    /// The viewer's own.
    Mine,
    /// Another user's.
    SomeoneElse,
}

impl OpenEnum for ModeOwnership {
    const ALL: &'static [Self] = &[Self::BuiltIn, Self::Mine, Self::SomeoneElse];

    fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built_in",
            Self::Mine => "mine",
            Self::SomeoneElse => "someone_else",
        }
    }
}

/// A mode as its viewer may see it (S14, S15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeResponse {
    /// The mode's ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: ModeId,
    /// Its name; a built-in mode's is its preset's (`afk`, `farm`).
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<MODE_NAME_MAX_CHARS>::schema)]
    pub name: BoundedText<MODE_NAME_MAX_CHARS>,
    /// Who may see it.
    #[ts(as = "ModeVisibility")]
    #[schema(value_type = ModeVisibility)]
    pub visibility: Open<ModeVisibility>,
    /// Whose it is, as far as every viewer may know: built in, the
    /// viewer's, or someone else's (S14).
    #[ts(as = "ModeOwnership")]
    #[schema(value_type = ModeOwnership)]
    pub ownership: Open<ModeOwnership>,
    /// Who owns it: for Admins and the Owner only (S14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub owner: Option<UserLabel>,
    /// Whether the viewer may change its name and steps.
    pub editable: bool,
    /// Whether the viewer may delete it.
    pub deletable: bool,
    /// Whether the viewer may make it private if it's shared, or shared if
    /// it's private.
    pub visibility_editable: bool,
    /// 1 when it was created, one more on every change.
    #[schema(schema_with = integer_schema::<1, { u32::MAX }>)]
    pub version: u32,
    /// When it was created.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub created_at: Timestamp,
    /// When it last changed.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub updated_at: Timestamp,
    /// Its steps. A viewer who may not edit it sees each command a chat
    /// step sends as its `/` and name alone (S15).
    pub steps: Vec<ModeStep>,
}

/// One step of a mode as its viewer may see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeStep {
    /// What it does.
    pub action: ModeAction,
    /// When it runs.
    pub schedule: ModeSchedule,
    /// How likely it runs when due, in percent.
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub probability: u8,
}

/// A step's action as its viewer may see it: its `type` and the fields that
/// type takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeAction {
    /// What it does.
    #[serde(rename = "type")]
    #[ts(as = "ModeActionKind")]
    #[schema(value_type = ModeActionKind)]
    pub kind: Open<ModeActionKind>,
    /// `look`: the direction, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<-180, 180>, nullable = false)]
    pub yaw: Option<f32>,
    /// `look`: up or down, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<-90, 90>, nullable = false)]
    pub pitch: Option<f32>,
    /// `rotate_random`: the most it turns either way, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<0, 180>, nullable = false)]
    pub max_yaw: Option<f32>,
    /// `rotate_random`: the most it tilts either way, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = number_schema::<0, 90>, nullable = false)]
    pub max_pitch: Option<f32>,
    /// `sneak` and `hold_use`: start or stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub on: Option<bool>,
    /// `select_hotbar_slot`: the slot, 0 to 8 (the keys 1 to 9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub slot: Option<u8>,
    /// `send_chat`: the line, cleaned; below the right to edit the mode, a
    /// command is only its `/` and name (S15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<MODE_MESSAGE_MAX_CHARS>::schema, nullable = false)]
    pub message: Option<BoundedText<MODE_MESSAGE_MAX_CHARS>>,
    /// Whether a command's arguments were left out for this viewer (S15).
    pub arguments_hidden: bool,
}

/// A step's schedule as its viewer sees it: its `type` and the fields that
/// type takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModeSchedule {
    /// When it runs.
    #[serde(rename = "type")]
    #[ts(as = "ModeScheduleKind")]
    #[schema(value_type = ModeScheduleKind)]
    pub kind: Open<ModeScheduleKind>,
    /// `every`: the shortest gap, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, MAX_MS>, nullable = false)]
    pub interval_ms: Option<u32>,
    /// `every`: how much longer a gap may be, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, MAX_MS>, nullable = false)]
    pub jitter_ms: Option<u32>,
}

impl ModeStep {
    /// `step` as its viewer may see it, each chat message as `shown`
    /// redacts it (S15: whole for a viewer who may edit the mode).
    #[must_use]
    pub fn of(step: &Step, shown: impl for<'a> Fn(&'a ChatMessage) -> Redacted<'a>) -> Self {
        let mut action = ModeAction {
            kind: ModeActionKind::from(&step.action).into(),
            yaw: None,
            pitch: None,
            max_yaw: None,
            max_pitch: None,
            on: None,
            slot: None,
            message: None,
            arguments_hidden: false,
        };
        match &step.action {
            Action::Look { yaw, pitch } => {
                action.yaw = Some(*yaw);
                action.pitch = Some(*pitch);
            }
            Action::RotateRandom { max_yaw, max_pitch } => {
                action.max_yaw = Some(*max_yaw);
                action.max_pitch = Some(*max_pitch);
            }
            Action::Sneak { on } | Action::HoldUse { on } => action.on = Some(*on),
            Action::SelectHotbarSlot { slot } => action.slot = Some(slot.get()),
            Action::SendChat { message } => {
                let redacted = shown(message);
                action.message = Some(BoundedText::new(redacted.text));
                action.arguments_hidden = redacted.arguments_hidden;
            }
            Action::Jump | Action::SwingArm | Action::UseItem | Action::AttackFacingEntity => {}
        }
        let schedule = match step.schedule {
            Schedule::AtStart => ModeSchedule {
                kind: ModeScheduleKind::AtStart.into(),
                interval_ms: None,
                jitter_ms: None,
            },
            Schedule::Every { interval, jitter } => ModeSchedule {
                kind: ModeScheduleKind::Every.into(),
                interval_ms: Some(millis(interval)),
                jitter_ms: Some(millis(jitter)),
            },
        };
        Self {
            action,
            schedule,
            probability: step.probability,
        }
    }
}

/// A stored gap in whole milliseconds. A valid mode's gaps are at most a
/// day, which a `u32` holds; a longer one would never have been stored, and
/// reads as the longest a request can carry.
fn millis(gap: Duration) -> u32 {
    u32::try_from(gap.as_millis()).unwrap_or(MAX_MS)
}

/// `GET /modes`: one page of the modes the caller may see, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct ModePage {
    /// The modes.
    pub items: Vec<ModeResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub next: Option<ModeId>,
}

#[cfg(test)]
mod tests {
    use fleet_core::mode::ModePreset;
    use rstest::rstest;
    use serde_json::{Value, json};

    use super::*;

    /// Every action and schedule type, as ADR-0010's mode JSON writes them.
    fn every_type() -> Value {
        json!({ "steps": [
            { "action": { "type": "look", "yaw": -90.5, "pitch": 12.0 }, "schedule": { "type": "at_start" }, "probability": 100 },
            { "action": { "type": "rotate_random", "max_yaw": 30.0, "max_pitch": 10.0 }, "schedule": { "type": "every", "interval_ms": 45000, "jitter_ms": 75000 }, "probability": 100 },
            { "action": { "type": "jump" }, "schedule": { "type": "every", "interval_ms": 250, "jitter_ms": 0 }, "probability": 1 },
            { "action": { "type": "sneak", "on": true }, "schedule": { "type": "at_start" }, "probability": 100 },
            { "action": { "type": "swing_arm" }, "schedule": { "type": "every", "interval_ms": 20000, "jitter_ms": 20000 }, "probability": 50 },
            { "action": { "type": "use_item" }, "schedule": { "type": "every", "interval_ms": 1000, "jitter_ms": 0 }, "probability": 100 },
            { "action": { "type": "hold_use", "on": true }, "schedule": { "type": "at_start" }, "probability": 100 },
            { "action": { "type": "hold_use", "on": false }, "schedule": { "type": "every", "interval_ms": 2000, "jitter_ms": 0 }, "probability": 100 },
            { "action": { "type": "attack_facing_entity" }, "schedule": { "type": "every", "interval_ms": 650, "jitter_ms": 150 }, "probability": 100 },
            { "action": { "type": "select_hotbar_slot", "slot": 8 }, "schedule": { "type": "at_start" }, "probability": 100 },
            { "action": { "type": "send_chat", "message": "/home base" }, "schedule": { "type": "every", "interval_ms": 30000, "jitter_ms": 0 }, "probability": 100 }
        ] })
    }

    /// `steps` as a request reads them.
    fn steps(steps: Value) -> ModeSteps {
        serde_json::from_value(steps).unwrap()
    }

    /// One step: `action` and `schedule` as JSON, always due.
    fn step(action: Value, schedule: Value) -> Value {
        Value::Object(serde_json::Map::from_iter([
            ("action".to_owned(), action),
            ("schedule".to_owned(), schedule),
            ("probability".to_owned(), json!(100)),
        ]))
    }

    fn every(interval_ms: u32) -> Value {
        json!({ "type": "every", "interval_ms": interval_ms, "jitter_ms": 0 })
    }

    fn at_start() -> Value {
        json!({ "type": "at_start" })
    }

    /// What garde reports for `request`'s steps: each path and message.
    fn problems(request: &CreateModeRequest) -> Vec<(String, String)> {
        request.validate().map_or_else(
            |report| {
                report
                    .iter()
                    .map(|(path, error)| (path.to_string(), error.message().to_owned()))
                    .collect()
            },
            |()| Vec::new(),
        )
    }

    fn request(name: &str, steps: Value) -> CreateModeRequest {
        CreateModeRequest {
            name: name.to_owned(),
            visibility: ModeVisibility::Private,
            steps: serde_json::from_value(steps).unwrap(),
        }
    }

    /// The one problem a request with these `steps` has.
    fn problem(steps: Value) -> (String, String) {
        let found = problems(&request("Fine", steps));
        assert_eq!(found.len(), 1, "{found:?}");
        found.into_iter().next().unwrap()
    }

    fn at(path: &str, message: &str) -> (String, String) {
        (path.to_owned(), message.to_owned())
    }

    #[test]
    fn every_type_reads_as_fleet_cores_mode_from_the_same_json() {
        let json = every_type();
        let expected: ModeDefinition = serde_json::from_value(json.clone()).unwrap();

        let read = steps(json["steps"].clone()).definition();

        assert_eq!(read, Ok(expected));
    }

    #[test]
    fn the_presets_read_as_themselves_and_answer_their_own_json() {
        for preset in ModePreset::ALL {
            let definition = preset.definition();
            let json = serde_json::to_value(&definition).unwrap();

            let read = steps(json["steps"].clone()).definition();
            let answered: Vec<ModeStep> = definition
                .steps()
                .iter()
                .map(|step| ModeStep::of(step, ChatMessage::without_arguments))
                .collect();

            assert_eq!(read, Ok(definition.clone()), "{preset}");
            assert_eq!(
                without_arguments_hidden(serde_json::to_value(&answered).unwrap()),
                json["steps"],
                "{preset}"
            );
        }
    }

    /// `steps` as answered, each action without its `arguments_hidden`.
    fn without_arguments_hidden(mut steps: Value) -> Value {
        for step in steps.as_array_mut().unwrap() {
            let action = step["action"].as_object_mut().unwrap();
            assert_eq!(action.remove("arguments_hidden"), Some(json!(false)));
        }
        steps
    }

    #[test]
    fn every_type_is_answered_as_its_own_json() {
        let json = every_type();
        let definition: ModeDefinition = serde_json::from_value(json.clone()).unwrap();

        let answered: Vec<ModeStep> = definition
            .steps()
            .iter()
            .map(|step| {
                ModeStep::of(step, |message| Redacted {
                    text: message.as_str(),
                    arguments_hidden: false,
                })
            })
            .collect();

        assert_eq!(
            without_arguments_hidden(serde_json::to_value(&answered).unwrap()),
            json["steps"]
        );
    }

    #[test]
    fn a_chat_step_is_answered_as_shown_redacts_it() {
        let definition: ModeDefinition = serde_json::from_value(json!({ "steps": [
            step(json!({ "type": "send_chat", "message": "/login hunter2" }), every(30_000)),
            step(json!({ "type": "send_chat", "message": "hello" }), at_start()),
        ] }))
        .unwrap();

        let answered: Vec<ModeStep> = definition
            .steps()
            .iter()
            .map(|step| ModeStep::of(step, ChatMessage::without_arguments))
            .collect();

        let shown: Vec<(Option<&str>, bool)> = answered
            .iter()
            .map(|step| {
                (
                    step.action.message.as_ref().map(BoundedText::as_str),
                    step.action.arguments_hidden,
                )
            })
            .collect();
        assert_eq!(shown, [(Some("/login"), true), (Some("hello"), false)]);
        assert!(
            !serde_json::to_string(&answered)
                .unwrap()
                .contains("hunter2")
        );
    }

    #[test]
    fn each_kind_is_named_as_fleet_core_tags_it() {
        let json = every_type();
        let definition: ModeDefinition = serde_json::from_value(json).unwrap();
        let mut named = Vec::new();

        for step in definition.steps() {
            let core = serde_json::to_value(step).unwrap();
            assert_eq!(
                ModeActionKind::from(&step.action).as_str(),
                core["action"]["type"]
            );
            assert_eq!(
                ModeScheduleKind::from(&step.schedule).as_str(),
                core["schedule"]["type"]
            );
            named.push(ModeActionKind::from(&step.action));
        }
        for kind in ModeActionKind::ALL {
            assert!(named.contains(kind), "{kind:?} isn't in every_type()");
        }
    }

    #[test]
    fn each_kind_reads_and_writes_its_wire_name() {
        for kind in ModeActionKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        }
        for kind in ModeScheduleKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        }
        for ownership in ModeOwnership::ALL {
            assert_eq!(
                serde_json::to_value(ownership).unwrap(),
                json!(ownership.as_str())
            );
        }
        for visibility in ModeVisibility::ALL {
            assert_eq!(
                serde_json::to_value(visibility).unwrap(),
                json!(visibility.as_str())
            );
            assert_eq!(
                ModeVisibility::from(CoreVisibility::from(*visibility)),
                *visibility
            );
            assert_eq!(
                visibility.as_str(),
                CoreVisibility::from(*visibility).as_str()
            );
        }
    }

    #[rstest]
    #[case::plain("Greeter")]
    #[case::umlauts_and_punctuation("Grüße, „Farm“ #2!")]
    #[case::a_built_in_name_inside("afk farm")]
    fn a_valid_name_has_no_problem(#[case] name: &str) {
        assert_eq!(problems(&request(name, json!([]))), []);
    }

    #[rstest]
    #[case::empty("", "must not be empty")]
    #[case::only_spaces("  ", "must not be empty")]
    #[case::emoji(
        "Farm 🐄",
        "must hold only letters, digits, plain spaces and punctuation, without emoji or other symbols"
    )]
    #[case::too_long(&"a".repeat(33), "must be at most 32 characters")]
    #[case::built_in("afk", MODE_NAME_RESERVED)]
    #[case::built_in_in_capitals("FARM", MODE_NAME_RESERVED)]
    fn a_name_that_breaks_its_rule_names_the_rule(#[case] name: &str, #[case] message: &str) {
        assert_eq!(problems(&request(name, json!([]))), [at("name", message)]);
    }

    #[test]
    fn too_many_steps_are_named_on_the_list_before_any_step() {
        let swing = step(json!({ "type": "swing_arm" }), every(1));
        let steps = json!(vec![swing; 33]);

        assert_eq!(problem(steps), at("steps", TOO_MANY_STEPS));
    }

    #[test]
    fn thirty_two_steps_are_fine() {
        let swing = step(json!({ "type": "swing_arm" }), every(1000));

        assert_eq!(problems(&request("Fine", json!(vec![swing; 32]))), []);
    }

    #[rstest]
    #[case::look_without_pitch(json!({ "type": "look", "yaw": 0.0 }), "steps[0].action.pitch", REQUIRED)]
    #[case::look_without_both(json!({ "type": "look" }), "steps[0].action.yaw", REQUIRED)]
    #[case::rotate_without_max_pitch(json!({ "type": "rotate_random", "max_yaw": 1.0 }), "steps[0].action.max_pitch", REQUIRED)]
    #[case::sneak_without_on(json!({ "type": "sneak" }), "steps[0].action.on", REQUIRED)]
    #[case::hold_use_without_on(json!({ "type": "hold_use" }), "steps[0].action.on", REQUIRED)]
    #[case::slot_without_slot(json!({ "type": "select_hotbar_slot" }), "steps[0].action.slot", REQUIRED)]
    #[case::chat_without_message(json!({ "type": "send_chat" }), "steps[0].action.message", REQUIRED)]
    #[case::jump_with_on(json!({ "type": "jump", "on": true }), "steps[0].action.on", NOT_USED)]
    #[case::look_with_max_yaw(json!({ "type": "look", "yaw": 0.0, "pitch": 0.0, "max_yaw": 1.0 }), "steps[0].action.max_yaw", NOT_USED)]
    #[case::swing_with_message(json!({ "type": "swing_arm", "message": "hi" }), "steps[0].action.message", NOT_USED)]
    #[case::sneak_with_slot(json!({ "type": "sneak", "on": true, "slot": 1 }), "steps[0].action.slot", NOT_USED)]
    #[case::fields_in_order(json!({ "type": "send_chat", "yaw": 1.0 }), "steps[0].action.yaw", NOT_USED)]
    fn a_field_its_action_needs_or_doesnt_take_is_named(
        #[case] action: Value,
        #[case] path: &str,
        #[case] message: &str,
    ) {
        assert_eq!(
            problem(json!([step(action, every(30_000))])),
            at(path, message)
        );
    }

    #[rstest]
    #[case::every_without_interval(json!({ "type": "every", "jitter_ms": 0 }), "steps[0].schedule.interval_ms", REQUIRED)]
    #[case::every_without_jitter(json!({ "type": "every", "interval_ms": 1000 }), "steps[0].schedule.jitter_ms", REQUIRED)]
    #[case::at_start_with_interval(json!({ "type": "at_start", "interval_ms": 1000 }), "steps[0].schedule.interval_ms", NOT_USED)]
    #[case::at_start_with_jitter(json!({ "type": "at_start", "jitter_ms": 0 }), "steps[0].schedule.jitter_ms", NOT_USED)]
    fn a_field_its_schedule_needs_or_doesnt_take_is_named(
        #[case] schedule: Value,
        #[case] path: &str,
        #[case] message: &str,
    ) {
        assert_eq!(
            problem(json!([step(json!({ "type": "jump" }), schedule)])),
            at(path, message)
        );
    }

    #[rstest]
    #[case::empty("  ", "must not be empty")]
    #[case::line_break(
        "a\nb",
        "must be one line, without control characters, formatting codes or text-direction marks"
    )]
    #[case::too_long(&"x".repeat(257), "must be at most 256 characters, an emoji counting as 2")]
    fn a_chat_message_that_breaks_its_rule_uses_chats_own_text(
        #[case] message: &str,
        #[case] text: &str,
    ) {
        let chat = step(
            json!({ "type": "send_chat", "message": message }),
            at_start(),
        );

        assert_eq!(problem(json!([chat])), at("steps[0].action.message", text));
    }

    #[test]
    fn a_slot_past_the_last_names_the_keys() {
        let select = step(
            json!({ "type": "select_hotbar_slot", "slot": 9 }),
            at_start(),
        );

        assert_eq!(problem(json!([select])), at("steps[0].action.slot", SLOT));
    }

    #[rstest]
    #[case::yaw(json!({ "type": "look", "yaw": 180.5, "pitch": 0.0 }), "steps[0].action.yaw", "must be from -180 to 180 degrees")]
    #[case::pitch(json!({ "type": "look", "yaw": 0.0, "pitch": -91.0 }), "steps[0].action.pitch", "must be from -90 to 90 degrees")]
    #[case::max_yaw(json!({ "type": "rotate_random", "max_yaw": -1.0, "max_pitch": 1.0 }), "steps[0].action.max_yaw", "must be from 0 to 180 degrees")]
    #[case::max_pitch(json!({ "type": "rotate_random", "max_yaw": 1.0, "max_pitch": 90.5 }), "steps[0].action.max_pitch", "must be from 0 to 90 degrees")]
    #[case::an_angle_too_large_for_f32(json!({ "type": "look", "yaw": 1e40, "pitch": 0.0 }), "steps[0].action.yaw", "must be from -180 to 180 degrees")]
    #[case::no_turn(json!({ "type": "rotate_random", "max_yaw": 0.0, "max_pitch": 0.0 }), "steps[0].action", NO_TURN)]
    fn an_angle_out_of_range_names_its_field(
        #[case] action: Value,
        #[case] path: &str,
        #[case] message: &str,
    ) {
        assert_eq!(
            problem(json!([step(action, at_start())])),
            at(path, message)
        );
    }

    #[rstest]
    #[case::zero(0)]
    #[case::over_a_hundred(101)]
    fn a_probability_outside_one_to_a_hundred_is_named(#[case] probability: u8) {
        let jump = json!({ "action": { "type": "jump" }, "schedule": { "type": "at_start" }, "probability": probability });

        assert_eq!(
            problem(json!([jump])),
            at("steps[0].probability", PROBABILITY)
        );
    }

    #[rstest]
    #[case::any_action(json!({ "type": "jump" }), 249, "must be at least 250 ms")]
    #[case::an_attack(json!({ "type": "attack_facing_entity" }), 499, "must be at least 500 ms for attacking or the use button")]
    #[case::the_use_button(json!({ "type": "hold_use", "on": false }), 499, "must be at least 500 ms for attacking or the use button")]
    #[case::chat(json!({ "type": "send_chat", "message": "hi" }), 29_999, "must be at least 30 seconds for chat")]
    fn an_interval_too_short_names_its_actions_minimum(
        #[case] action: Value,
        #[case] interval_ms: u32,
        #[case] message: &str,
    ) {
        assert_eq!(
            problem(json!([step(action.clone(), every(interval_ms))])),
            at("steps[0].schedule.interval_ms", message)
        );
        assert_eq!(
            problems(&request(
                "Fine",
                json!([step(action, every(interval_ms + 1))])
            )),
            [],
            "the minimum itself is fine"
        );
    }

    #[test]
    fn each_minimum_text_is_fleet_cores_minimum() {
        assert_eq!(ModeDefinition::MIN_INTERVAL, Duration::from_millis(250));
        assert_eq!(
            ModeDefinition::MIN_ATTACK_INTERVAL,
            Duration::from_millis(500)
        );
        assert_eq!(
            ModeDefinition::MIN_HOLD_USE_INTERVAL,
            Duration::from_millis(500)
        );
        assert_eq!(ModeDefinition::MIN_CHAT_INTERVAL, Duration::from_secs(30));
        assert_eq!(ModeDefinition::MAX_INTERVAL, Duration::from_hours(24));
        assert_eq!(ModeDefinition::MAX_JITTER, Duration::from_hours(24));
        assert_eq!(MODE_MAX_STEPS, 32);
        assert_eq!(MODE_NAME_MAX_CHARS, 32);
    }

    #[rstest]
    #[case::interval(json!({ "type": "every", "interval_ms": 86_400_001, "jitter_ms": 0 }), "steps[0].schedule.interval_ms")]
    #[case::jitter(json!({ "type": "every", "interval_ms": 1000, "jitter_ms": 86_400_001 }), "steps[0].schedule.jitter_ms")]
    fn a_gap_longer_than_a_day_is_named(#[case] schedule: Value, #[case] path: &str) {
        assert_eq!(
            problem(json!([step(json!({ "type": "jump" }), schedule)])),
            at(path, AT_MOST_A_DAY)
        );
    }

    #[rstest]
    #[case::chat_at_start(json!({ "type": "send_chat", "message": "hi" }), at_start(), "is a second chat step at start; a mode can have only one")]
    #[case::repeating_chat(json!({ "type": "send_chat", "message": "hi" }), every(30_000), "is a second repeating chat step; a mode can have only one")]
    #[case::attack_at_start(json!({ "type": "attack_facing_entity" }), at_start(), "is a second attack step at start; a mode can have only one")]
    #[case::repeating_attack(json!({ "type": "attack_facing_entity" }), every(500), "is a second repeating attack step; a mode can have only one")]
    #[case::hold_at_start(json!({ "type": "hold_use", "on": true }), at_start(), "is a second step at start that holds the use button; a mode can have only one")]
    #[case::repeating_hold(json!({ "type": "hold_use", "on": true }), every(500), "is a second repeating step that holds the use button; a mode can have only one")]
    #[case::release_at_start(json!({ "type": "hold_use", "on": false }), at_start(), "is a second step at start that lets go of the use button; a mode can have only one")]
    #[case::repeating_release(json!({ "type": "hold_use", "on": false }), every(500), "is a second repeating step that lets go of the use button; a mode can have only one")]
    fn a_second_limited_step_is_named_on_its_step(
        #[case] action: Value,
        #[case] schedule: Value,
        #[case] message: &str,
    ) {
        let jump = step(json!({ "type": "jump" }), at_start());
        let limited = step(action, schedule);

        assert_eq!(
            problem(json!([limited.clone(), jump, limited])),
            at("steps[2]", message)
        );
    }

    #[test]
    fn the_earliest_step_is_named_whichever_kind_of_rule_it_breaks() {
        let bad_angle = step(
            json!({ "type": "look", "yaw": 999.0, "pitch": 0.0 }),
            at_start(),
        );
        let missing_field = step(json!({ "type": "sneak" }), at_start());

        assert_eq!(
            problem(json!([bad_angle.clone(), missing_field.clone()])),
            at("steps[0].action.yaw", "must be from -180 to 180 degrees")
        );
        assert_eq!(
            problem(json!([missing_field, bad_angle])),
            at("steps[0].action.on", REQUIRED)
        );
    }

    #[test]
    fn a_steps_own_fields_are_named_before_fleet_cores_rules_for_it() {
        let both = json!({ "action": { "type": "look", "yaw": 999.0 }, "schedule": { "type": "at_start" }, "probability": 0 });

        assert_eq!(
            problem(json!([both])),
            at("steps[0].action.pitch", REQUIRED)
        );
    }

    #[test]
    fn every_field_message_comes_through_the_cleaning_unchanged() {
        let messages = [
            TOO_MANY_STEPS,
            REQUIRED,
            NOT_USED,
            SLOT,
            NO_TURN,
            PROBABILITY,
            AT_MOST_A_DAY,
            MODE_NAME_TAKEN,
            MODE_NAME_RESERVED,
            name_rule(ModeNameError::Empty),
            name_rule(ModeNameError::ForbiddenChar { index: 0 }),
            name_rule(ModeNameError::TooLong { chars: 33 }),
        ]
        .into_iter()
        .chain(
            [Angle::Yaw, Angle::Pitch, Angle::MaxYaw, Angle::MaxPitch]
                .into_iter()
                .map(angle_rule),
        )
        .chain(
            [
                LimitedStep::StartChat,
                LimitedStep::RepeatingChat,
                LimitedStep::StartAttack,
                LimitedStep::RepeatingAttack,
                LimitedStep::StartHold,
                LimitedStep::RepeatingHold,
                LimitedStep::StartRelease,
                LimitedStep::RepeatingRelease,
            ]
            .into_iter()
            .map(duplicate_rule),
        )
        .chain(ModeActionKind::ALL.iter().map(|&kind| interval_rule(kind)));
        for message in messages {
            let cleaned = BoundedText::<{ crate::error::FIELD_MESSAGE_MAX_CHARS }>::new(message);
            assert_eq!(cleaned.as_str(), message);
        }
    }

    #[test]
    fn a_message_never_reaches_a_field_error() {
        let canary = "secret-canary";
        let bad = [
            step(
                json!({ "type": "send_chat", "message": format!("{canary}\nnext") }),
                at_start(),
            ),
            step(
                json!({ "type": "send_chat", "message": format!("/login {canary}") }),
                every(1),
            ),
        ];

        for steps in bad {
            let found = problems(&request("Fine", json!([steps])));
            assert_eq!(found.len(), 1);
            assert!(!format!("{found:?}").contains(canary), "{found:?}");
        }
    }

    #[test]
    fn a_shape_error_is_serdes_and_never_a_field_error() {
        let shapes = [
            json!([{ "action": { "type": "fly" }, "schedule": { "type": "at_start" }, "probability": 100 }]),
            json!([{ "action": { "type": "select_hotbar_slot", "slot": "one" }, "schedule": { "type": "at_start" }, "probability": 100 }]),
            json!([{ "action": { "type": "select_hotbar_slot", "slot": 256 }, "schedule": { "type": "at_start" }, "probability": 100 }]),
            json!([{ "action": { "type": "jump", "colour": "red" }, "schedule": { "type": "at_start" }, "probability": 100 }]),
            json!([{ "action": { "type": "jump" }, "schedule": { "type": "at_start" } }]),
            json!([{ "action": { "type": "jump" }, "schedule": { "type": "every", "interval_ms": -1, "jitter_ms": 0 }, "probability": 100 }]),
        ];

        for steps in shapes {
            assert!(
                serde_json::from_value::<ModeSteps>(steps.clone()).is_err(),
                "{steps}"
            );
        }
    }

    #[test]
    fn a_request_refuses_unknown_fields_and_round_trips() {
        let json =
            json!({ "name": "Greeter", "visibility": "shared", "steps": every_type()["steps"] });
        let read: CreateModeRequest = serde_json::from_value(json.clone()).unwrap();

        assert_eq!(serde_json::to_value(&read).unwrap(), json);
        let mut extra = json;
        extra["owner"] = json!("someone");
        assert!(serde_json::from_value::<CreateModeRequest>(extra).is_err());
    }

    #[test]
    fn an_answer_round_trips_and_reads_an_unknown_type() {
        let answer = json!({
            "id": "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd",
            "name": "Greeter",
            "visibility": "shared",
            "ownership": "mine",
            "editable": true,
            "deletable": true,
            "visibility_editable": false,
            "version": 3,
            "created_at": "2027-01-15T08:00:00.000Z",
            "updated_at": "2027-01-15T09:00:00.000Z",
            "steps": [{ "action": { "type": "fly_somewhere", "arguments_hidden": false }, "schedule": { "type": "on_a_whim" }, "probability": 100 }],
        });

        let read: ModeResponse = serde_json::from_value(answer.clone()).unwrap();

        assert_eq!(read.steps.first().unwrap().action.kind.known(), None);
        assert_eq!(serde_json::to_value(&read).unwrap(), answer);
    }
}
