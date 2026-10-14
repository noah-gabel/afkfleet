//! Retry and circuit-breaker policies (Plan.md §6, rows 1, 4 and 7).
//!
//! All of them are pure: the caller passes in the current time and, for
//! jitter, a random number generator (ADR-0010).
//! - [`RetryPolicy`]: exponential backoff with jitter, and when a bot counts as
//!   stably online.
//! - [`FailureWindow`]: "N failures within a time window", used by the circuit
//!   breaker and later by the supervisor's restart limit.
//! - [`CircuitBreaker`]: stops retrying a flapping server for a cool-down.
//! - [`Doubling`]: a backoff without jitter, for the server's calls to
//!   Microsoft (P9.5).

mod circuit;
mod doubling;
mod retry;
mod window;

pub use circuit::{CircuitBreaker, CircuitPolicy, CircuitPolicyError, CircuitState};
pub use doubling::Doubling;
pub use retry::{RetryPolicy, RetryPolicyError};
pub use window::FailureWindow;
