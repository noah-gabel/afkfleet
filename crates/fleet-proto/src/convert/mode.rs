//! A mode, as structured messages that mirror [`ModeDefinition`]'s steps
//! (ADR-0019). Reading rebuilds a [`ModeDraft`] and validates it, so the
//! receiver keeps fleet-core's mode rules whatever the sender did, and a step
//! whose action or schedule this version doesn't know is refused.

use core::time::Duration;

use fleet_core::mode::{Action, HotbarSlot, ModeDefinition, ModeDraft, Schedule, Step};

use super::{chat_message_from_wire, required};
use crate::generated as wire;
use crate::{ConvertError, Problem};

impl From<ModeDefinition> for wire::ModeDefinition {
    fn from(mode: ModeDefinition) -> Self {
        Self {
            steps: mode.steps().iter().map(step_to_wire).collect(),
        }
    }
}

impl TryFrom<wire::ModeDefinition> for ModeDefinition {
    type Error = ConvertError;

    fn try_from(mode: wire::ModeDefinition) -> Result<Self, ConvertError> {
        let steps = mode
            .steps
            .into_iter()
            .enumerate()
            .map(|(index, step)| step_from_wire(step).map_err(|e| e.at(index)))
            .collect::<Result<_, _>>()?;
        ModeDraft { steps }
            .validate()
            .map_err(|e| ConvertError::new("ModeDefinition.steps", e.into()))
    }
}

/// Whole milliseconds, as a validated mode keeps them; a mode's durations are
/// at most a day, far below `u64::MAX` milliseconds.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn step_to_wire(step: &Step) -> wire::Step {
    use wire::action::Action as A;
    let unit = wire::Unit {};
    let action = match &step.action {
        Action::Look { yaw, pitch } => A::Look(wire::action::Look {
            yaw: *yaw,
            pitch: *pitch,
        }),
        Action::RotateRandom { max_yaw, max_pitch } => {
            A::RotateRandom(wire::action::RotateRandom {
                max_yaw: *max_yaw,
                max_pitch: *max_pitch,
            })
        }
        Action::Jump => A::Jump(unit),
        Action::Sneak { on } => A::Sneak(wire::action::Sneak { on: *on }),
        Action::SwingArm => A::SwingArm(unit),
        Action::UseItem => A::UseItem(unit),
        Action::HoldUse { on } => A::HoldUse(wire::action::HoldUse { on: *on }),
        Action::AttackFacingEntity => A::AttackFacingEntity(unit),
        Action::SelectHotbarSlot { slot } => A::SelectHotbarSlot(wire::action::SelectHotbarSlot {
            slot: u32::from(slot.get()),
        }),
        Action::SendChat { message } => A::SendChat(wire::action::SendChat {
            message: message.as_str().to_owned(),
        }),
    };
    let schedule = match step.schedule {
        Schedule::AtStart => wire::schedule::Schedule::AtStart(unit),
        Schedule::OnRespawn => wire::schedule::Schedule::OnRespawn(unit),
        Schedule::Every { interval, jitter } => {
            wire::schedule::Schedule::Every(wire::schedule::Every {
                interval_ms: millis(interval),
                jitter_ms: millis(jitter),
            })
        }
    };
    wire::Step {
        action: Some(wire::Action {
            action: Some(action),
        }),
        schedule: Some(wire::Schedule {
            schedule: Some(schedule),
        }),
        probability: u32::from(step.probability),
    }
}

fn step_from_wire(step: wire::Step) -> Result<Step, ConvertError> {
    use wire::action::Action as A;
    let action = match required(
        "Action.action",
        required("Step.action", step.action)?.action,
    )? {
        A::Look(look) => Action::Look {
            yaw: look.yaw,
            pitch: look.pitch,
        },
        A::RotateRandom(rotate) => Action::RotateRandom {
            max_yaw: rotate.max_yaw,
            max_pitch: rotate.max_pitch,
        },
        A::Jump(wire::Unit {}) => Action::Jump,
        A::Sneak(sneak) => Action::Sneak { on: sneak.on },
        A::SwingArm(wire::Unit {}) => Action::SwingArm,
        A::UseItem(wire::Unit {}) => Action::UseItem,
        A::HoldUse(hold) => Action::HoldUse { on: hold.on },
        A::AttackFacingEntity(wire::Unit {}) => Action::AttackFacingEntity,
        A::SelectHotbarSlot(select) => {
            const FIELD: &str = "Action.SelectHotbarSlot.slot";
            let slot = u8::try_from(select.slot)
                .map_err(|_| ConvertError::new(FIELD, Problem::OutOfRange))?;
            Action::SelectHotbarSlot {
                slot: HotbarSlot::try_from(slot).map_err(|e| ConvertError::new(FIELD, e.into()))?,
            }
        }
        A::SendChat(send) => Action::SendChat {
            message: chat_message_from_wire("Action.SendChat.message", &send.message)?,
        },
    };
    let schedule = match required(
        "Schedule.schedule",
        required("Step.schedule", step.schedule)?.schedule,
    )? {
        wire::schedule::Schedule::AtStart(wire::Unit {}) => Schedule::AtStart,
        wire::schedule::Schedule::OnRespawn(wire::Unit {}) => Schedule::OnRespawn,
        wire::schedule::Schedule::Every(every) => Schedule::Every {
            interval: Duration::from_millis(every.interval_ms),
            jitter: Duration::from_millis(every.jitter_ms),
        },
    };
    let probability = u8::try_from(step.probability)
        .map_err(|_| ConvertError::new("Step.probability", Problem::OutOfRange))?;
    Ok(Step {
        action,
        schedule,
        probability,
    })
}
