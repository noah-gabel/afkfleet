//! [`RuntimeConfig`]: the runtime's settings.

use core::num::{NonZeroU32, NonZeroUsize};
use core::time::Duration;

/// The runtime's settings. [`Default`] gives the ones ADR-0013 decided.
///
/// None of them is a config key yet: the agent maps only Appendix A's keys
/// (P5.1), and the others keep these defaults. Tests shrink them to reach a
/// case easily. The actor, the watchdog and the supervisor add their
/// settings in their tasks (P4.2, P4.6, P4.7).
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
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        // Adding to `MIN` gives a non-zero constant without unwrapping the
        // `Option` that `NonZero*::new` returns.
        Self {
            chat_interval: Duration::from_secs(3),
            chat_burst: NonZeroU32::MIN.saturating_add(2),
            chat_queue: NonZeroUsize::MIN.saturating_add(15),
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
    }
}
