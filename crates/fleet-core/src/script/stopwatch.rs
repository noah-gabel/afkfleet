//! [`Stopwatch`]: how the engine that runs a script reads how long a hook
//! has run (ADR-0040's amendment).
//!
//! A hook is bounded by its operations and by its time. Operations are
//! counted; time has to be read, and neither this crate nor the engine
//! reads a clock, so whoever runs a script hands one in. The real one
//! reads the OS's monotonic clock; fleet-testkit's stands still, or moves
//! by a fixed step at each reading, so a test says how long a hook took.

use core::fmt;
use core::time::Duration;

/// A stopwatch that runs and is never reset.
///
/// **It must never go back.** What it counts from is its own business:
/// only the difference between two readings is used. A reading must be
/// cheap, since a running hook reads it every few operations
/// ([`limits::TIME_CHECK_EVERY_OPERATIONS`]).
///
/// [`limits::TIME_CHECK_EVERY_OPERATIONS`]: super::limits::TIME_CHECK_EVERY_OPERATIONS
pub trait Stopwatch: Send + Sync + fmt::Debug {
    /// How long the stopwatch has run.
    fn elapsed(&self) -> Duration;
}
