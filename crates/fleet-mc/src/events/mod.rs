//! From azalea's events to [`SessionEvent`](fleet_core::mc::SessionEvent)s
//! (Plan.md P3.5; ADR-0008 §4–7, ADR-0010, ADR-0011).
//!
//! - `map` turns each azalea `Event` into what it means for the session, with
//!   pure functions: chat goes through the core sanitizer, with a sender only
//!   from player packets; kick reasons go through the core classifier input.
//! - `bridge` is the bounded queue between the host thread and the bot's
//!   actor. It keeps ADR-0010's contract: only chat is dropped (and counted),
//!   `Joined` is the first spawn, a death before it is held, a death is
//!   reported once until the respawn, and the first terminal event wins.
//!   [`McEvents`] is its consumer side.
//! - `liveness` holds the session's tick and packet stamps. Ticks and received
//!   packets only stamp liveness; they're never queued.
//! - `render` turns server text into plain text within a fixed budget, since
//!   azalea's own rendering of nested translations grows exponentially.

mod bridge;
mod liveness;
mod map;
mod render;

pub use bridge::McEvents;
pub(crate) use bridge::{BridgeControl, EventCounters, EventSink, Phase, bridge};
pub(crate) use liveness::{LivenessStamps, PacketLivenessPlugin};
pub(crate) use map::Terminal;
