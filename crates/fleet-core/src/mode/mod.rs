//! Modes: what a bot does while it's online (Plan.md P2.7, P2.8; ADR-0010).
//!
//! A mode is a list of [`Step`]s, and each step pairs an [`Action`] with a
//! [`Schedule`] and a probability.
//! - [`ModeDraft`] is a mode as stored or entered. [`ModeDraft::validate`]
//!   checks the limits (intervals, angles, how many steps) and returns a
//!   [`ModeDefinition`], the only form the rest of the system runs.
//! - [`ModeDefinition::afk`] and [`ModeDefinition::farm`] are the built-in
//!   presets, and [`ModePreset`] names them (`afk`, `farm`), as the standalone
//!   agent's config does.
//! - [`ModeName`] is a user's name for a mode (S13, U8), and an owner
//!   holds at most [`MAX_MODES_PER_OWNER`] modes (S19); both from ADR-0020.
//! - [`ModePlan`] runs a definition: it decides when each step is due and
//!   resolves it into [`PlannedAction`]s, a [`GameAction`] for the session
//!   or a chat message for the chat queue. The runtime only sleeps until the
//!   next due time and executes them.
//!
//! Modes are stored as JSON in `modes.definition_json`. Actions and schedules
//! are tagged with their `"type"`, and durations are whole milliseconds in
//! `*_ms` fields. Reading a [`ModeDefinition`] validates it again, unknown
//! fields are ignored, and changes must be additive. A new action or schedule
//! *type* can't be read by an older server, so a stored mode that uses one
//! fails to load there (ADR-0010). Snapshot tests of the presets and of every
//! tag guard the format.

mod action;
mod cap;
mod definition;
mod millis;
mod name;
mod plan;
mod preset;

pub use action::{Action, HotbarSlot, HotbarSlotError};
pub use cap::{MAX_MODES_PER_OWNER, mode_cap_reached};
pub use definition::{Angle, LimitedStep, ModeDefinition, ModeDraft, ModeError, Schedule, Step};
pub use name::{ModeName, ModeNameError};
pub use plan::{GameAction, ModePlan, PlanTick, PlannedAction};
pub use preset::{ModePreset, UnknownPresetError};
