//! [`Pace`]: how a background task waits between its calls to another
//! service, such as the link flow's polls of Microsoft (Plan.md P9.4,
//! ADR-0018).
//!
//! The waits go through a port so tests can see each one and move their
//! clock by it, without waiting: the link flow's tests run on real time,
//! since sqlx can't run on paused time (ADR-0015). Production waits with
//! tokio's timer ([`crate::infra::system::TokioPace`]).

use core::fmt;
use core::time::Duration;

use async_trait::async_trait;

/// Waits between a task's calls.
#[async_trait]
pub trait Pace: Send + Sync + fmt::Debug {
    /// Returns after `duration`. Cancel-safe: dropping the future ends the
    /// wait and nothing else.
    async fn wait(&self, duration: Duration);
}
