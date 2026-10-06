//! Modes: what a bot does while it's online (Plan.md P2.7; ADR-0010).
//!
//! A mode is a list of [`Step`]s, and each step pairs an [`Action`] with a
//! [`Schedule`] and a probability.
//! - [`ModeDraft`] is a mode as stored or entered. [`ModeDraft::validate`]
//!   checks the limits (intervals, angles, how many steps) and returns a
//!   [`ModeDefinition`], the only form the rest of the system runs.
//! - [`ModeDefinition::afk`] and [`ModeDefinition::farm`] are the built-in
//!   presets.
//!
//! Modes are stored as JSON in `modes.definition_json`. Actions and schedules
//! are tagged with their `"type"`, and durations are whole milliseconds in
//! `*_ms` fields. Reading a [`ModeDefinition`] validates it again, unknown
//! fields are ignored, and changes must be additive. A new action or schedule
//! *type* can't be read by an older server, so a stored mode that uses one
//! fails to load there (ADR-0010). Snapshot tests of the presets and of every
//! tag guard the format.

mod action;
mod definition;
mod millis;

pub use action::{Action, HotbarSlot, HotbarSlotError};
pub use definition::{Angle, LimitedStep, ModeDefinition, ModeDraft, ModeError, Schedule, Step};
