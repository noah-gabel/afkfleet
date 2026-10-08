//! [`RuntimeConfig`]: the runtime's settings.

use core::num::{NonZeroU32, NonZeroUsize};
use core::time::Duration;

/// The runtime's settings. [`Default`] gives the ones ADR-0013 decided.
///
/// `connect_timeout`, `watchdog_timeout`, `packet_liveness_timeout`,
/// `max_bots` and `shutdown_timeout` are Appendix A's `[runtime]` keys, which
/// the agent maps (P5.1); the others aren't config keys yet and keep these
/// defaults. Tests shrink them to reach a case easily.
///
/// `Fleet::new` checks every capacity it hands to tokio against tokio's
/// limits, so no setting can make the runtime panic (ADR-0013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// A bot's chat bucket gains one message per this interval (Plan.md
    /// §7.4).
    pub chat_interval: Duration,
    /// How many messages a full chat bucket lets through at once (§7.4).
    pub chat_burst: NonZeroU32,
    /// How many messages a bot's chat queue holds. A message beyond that is
    /// refused with `QueueFull` instead of waiting.
    pub chat_queue: NonZeroUsize,
    /// How long a bot waits for its session credentials. A request that
    /// takes longer counts as a retryable failure (ADR-0010).
    pub session_request_timeout: Duration,
    /// How long a session may take from `connect()` until the bot has
    /// joined (Appendix A, ADR-0011). The actor also bounds `connect()`
    /// itself with it.
    pub connect_timeout: Duration,
    /// How many commands a bot actor's inbox holds. A command beyond that is
    /// refused instead of waiting.
    pub actor_inbox: NonZeroUsize,
    /// How long the actor waits after a failed respawn before it calls
    /// again.
    pub respawn_interval: Duration,
    /// After this many failed respawn calls in a row, the session ends with
    /// `RespawnFailed`, so the bot respawns when it joins again.
    pub respawn_attempts: NonZeroU32,
    /// How many ready session events the actor applies, at most, before an
    /// input that ends the session, so a duplicate-login kick that's already
    /// queued is never lost (ADR-0013).
    ///
    /// fleet-mc's event queue holds 64 events before it drops chat, but it
    /// always admits a lifecycle event, and the port contract allows at most
    /// 3 of those pending at once: `Joined`, one `Died` until the respawn,
    /// and one terminal event. The queue is first in, first out, so 64 + 3
    /// reads always reach every event that was queued when the input came.
    pub session_drain: NonZeroUsize,
    /// While the bot is Online, a session that hasn't ticked for this long
    /// has hung: `WatchdogTimeout` (Appendix A's
    /// `watchdog_timeout_secs`; ADR-0008 §5).
    pub watchdog_timeout: Duration,
    /// While the bot is Online, a session that hasn't heard from the server
    /// for this long has a frozen server or a dead link:
    /// `Disconnected(LivenessTimeout)` (Appendix A's
    /// `packet_liveness_timeout_secs`; ADR-0008 §5).
    pub packet_liveness_timeout: Duration,
    /// How often the watchdog checks while the bot is Online. A zero period
    /// counts as 1 ms, so the actor's loop can't spin.
    pub watchdog_period: Duration,
    /// How many bots the fleet holds at most, in any state, until each one's
    /// `Removed`. A new bot beyond that is refused with `AtCapacity`
    /// (Appendix A's `max_bots`).
    pub max_bots: NonZeroUsize,
    /// How many `Fleet` calls the supervisor's queue holds. A call beyond
    /// that is refused with `Busy` instead of waiting.
    pub supervisor_queue: NonZeroUsize,
    /// How many events the fleet's `broadcast` keeps for a subscriber that
    /// lags behind; one that falls further behind resyncs from the
    /// snapshots (Plan.md §6 row 12). tokio's `broadcast` allocates its whole
    /// buffer up front, so a huge value that passes `Fleet::new`'s check can
    /// still run out of memory; the `mpsc` queues allocate as they fill.
    pub event_buffer: NonZeroUsize,
    /// How long a `Fleet` call waits for the supervisor's answer before it
    /// gives up with `TimedOut`.
    pub reply_timeout: Duration,
    /// How long a shutdown waits for the actors to stop before it aborts the
    /// rest. The supervisor also uses it when its token is cancelled or every
    /// `Fleet` handle is dropped (Appendix A's `shutdown_timeout_secs`).
    pub shutdown_timeout: Duration,
    /// How many crashes of a bot's actor within `restart_window` end in
    /// `CrashLoop` (Plan.md §6 row 7).
    pub restart_limit: NonZeroUsize,
    /// The window that `restart_limit` counts crashes in.
    pub restart_window: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        // Adding to `MIN` gives a non-zero constant without unwrapping the
        // `Option` that `NonZero*::new` returns.
        Self {
            chat_interval: Duration::from_secs(3),
            chat_burst: NonZeroU32::MIN.saturating_add(2),
            chat_queue: NonZeroUsize::MIN.saturating_add(15),
            session_request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(30),
            actor_inbox: NonZeroUsize::MIN.saturating_add(31),
            respawn_interval: Duration::from_secs(5),
            respawn_attempts: NonZeroU32::MIN.saturating_add(11),
            session_drain: NonZeroUsize::MIN.saturating_add(66),
            watchdog_timeout: Duration::from_secs(30),
            packet_liveness_timeout: Duration::from_secs(30),
            watchdog_period: Duration::from_secs(1),
            max_bots: NonZeroUsize::MIN.saturating_add(49),
            supervisor_queue: NonZeroUsize::MIN.saturating_add(63),
            event_buffer: NonZeroUsize::MIN.saturating_add(1023),
            reply_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_secs(10),
            restart_limit: NonZeroUsize::MIN.saturating_add(5),
            restart_window: Duration::from_mins(10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_ones_adr_0013_decided() {
        let config = RuntimeConfig::default();

        assert_eq!(config.chat_interval, Duration::from_secs(3));
        assert_eq!(config.chat_burst.get(), 3);
        assert_eq!(config.chat_queue.get(), 16);
        assert_eq!(config.session_request_timeout, Duration::from_secs(30));
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
        assert_eq!(config.actor_inbox.get(), 32);
        assert_eq!(config.respawn_interval, Duration::from_secs(5));
        assert_eq!(config.respawn_attempts.get(), 12);
        assert_eq!(config.session_drain.get(), 64 + 3);
        assert_eq!(config.watchdog_timeout, Duration::from_secs(30));
        assert_eq!(config.packet_liveness_timeout, Duration::from_secs(30));
        assert_eq!(config.watchdog_period, Duration::from_secs(1));
        assert_eq!(config.max_bots.get(), 50);
        assert_eq!(config.supervisor_queue.get(), 64);
        assert_eq!(config.event_buffer.get(), 1024);
        assert_eq!(config.reply_timeout, Duration::from_secs(5));
        assert_eq!(config.shutdown_timeout, Duration::from_secs(10));
        assert_eq!(config.restart_limit.get(), 6);
        assert_eq!(config.restart_window, Duration::from_mins(10));
    }
}
