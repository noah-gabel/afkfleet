//! [`Liveness`]: when a session last showed signs of life.

use std::time::Instant;

/// When a session last ticked and last heard from the server (ADR-0008 §4–5;
/// ADR-0010).
///
/// The session sets both stamps when it's created, so they're never empty,
/// and updates them in place instead of queueing an event for every tick or
/// packet. [`SessionHandle::liveness`](super::SessionHandle::liveness) reads
/// them. They're monotonic [`Instant`]s, never stored or sent, so a wall-clock
/// step can't trip every bot's watchdog at once.
///
/// The watchdog (P4.6) compares them against its timeouts while the bot is
/// online:
/// - no tick for `watchdog_timeout`: the session hung
///   ([`BotEvent::WatchdogTimeout`](crate::bot::BotEvent::WatchdogTimeout));
/// - no packet for `packet_liveness_timeout`: the server froze or the link
///   died ([`DisconnectReason::LivenessTimeout`](crate::disconnect::DisconnectReason::LivenessTimeout)).
///   Ticks are client-side and keep going then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Liveness {
    /// When the session last ran a game tick (20 a second while it's
    /// healthy).
    pub last_tick: Instant,
    /// When the session last received a packet from the server, such as a
    /// `KeepAlive` (about every 15 s).
    pub last_packet: Instant,
}
