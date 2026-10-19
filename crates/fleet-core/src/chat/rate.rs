//! [`ChatRate`]: how fast a bot may chat (Plan.md §7.4, P11.3; ADR-0020, S3).

use core::num::NonZeroU32;
use core::time::Duration;

/// Why a [`ChatRate`] can't be built. It names the number, never more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatRateError {
    /// The interval isn't 1 to 60 seconds.
    #[error(
        "the chat interval is {secs} s; it must be {min} to {max} s",
        min = ChatRate::MIN_INTERVAL_SECS,
        max = ChatRate::MAX_INTERVAL_SECS
    )]
    Interval {
        /// The interval asked for, in seconds.
        secs: u32,
    },
    /// The burst isn't 1 to 10 messages.
    #[error(
        "the chat burst is {burst} messages; it must be {min} to {max}",
        min = ChatRate::MIN_BURST,
        max = ChatRate::MAX_BURST
    )]
    Burst {
        /// The burst asked for.
        burst: u32,
    },
}

/// A bot's chat rate: a bucket that holds `burst` messages and gains one
/// every `interval`, shared by what users send and what the bot's mode says.
///
/// The server sets it in `[rate_limits]` (`chat_per_bot_interval_secs`,
/// `chat_per_bot_burst`) and sends it in each bot's spec, so the server's
/// limit and the agent's bucket always agree; a standalone agent uses the
/// [default](Self::default), one message every 3 s with a burst of 3. Values
/// outside the range are refused, never clamped. The burst stops at 10: the
/// agent enforces the rate too, and every allowed value stays under vanilla's
/// spam kick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChatRate {
    interval_secs: NonZeroU32,
    burst: NonZeroU32,
}

impl ChatRate {
    /// The shortest interval, in seconds.
    pub const MIN_INTERVAL_SECS: u32 = 1;
    /// The longest interval, in seconds.
    pub const MAX_INTERVAL_SECS: u32 = 60;
    /// The smallest burst.
    pub const MIN_BURST: u32 = 1;
    /// The largest burst.
    pub const MAX_BURST: u32 = 10;

    /// A rate of one message every `interval_secs` seconds, with a burst of
    /// `burst`.
    ///
    /// # Errors
    /// [`ChatRateError::Interval`] for an interval outside 1 to 60 s, and
    /// [`ChatRateError::Burst`] for a burst outside 1 to 10.
    pub fn try_new(interval_secs: u32, burst: u32) -> Result<Self, ChatRateError> {
        let interval = NonZeroU32::new(interval_secs)
            .filter(|secs| secs.get() <= Self::MAX_INTERVAL_SECS)
            .ok_or(ChatRateError::Interval {
                secs: interval_secs,
            })?;
        let messages = NonZeroU32::new(burst)
            .filter(|burst| burst.get() <= Self::MAX_BURST)
            .ok_or(ChatRateError::Burst { burst })?;
        Ok(Self {
            interval_secs: interval,
            burst: messages,
        })
    }

    /// The interval, in seconds.
    #[must_use]
    pub const fn interval_secs(self) -> u32 {
        self.interval_secs.get()
    }

    /// The interval.
    #[must_use]
    pub fn interval(self) -> Duration {
        Duration::from_secs(u64::from(self.interval_secs.get()))
    }

    /// How many messages the full bucket holds.
    #[must_use]
    pub const fn burst(self) -> NonZeroU32 {
        self.burst
    }
}

impl Default for ChatRate {
    /// One message every 3 s, with a burst of 3 (Plan.md §7.4).
    fn default() -> Self {
        Self {
            interval_secs: NonZeroU32::MIN.saturating_add(2),
            burst: NonZeroU32::MIN.saturating_add(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::the_shortest(1, 1)]
    #[case::the_default(3, 3)]
    #[case::the_longest_and_the_largest(60, 10)]
    fn a_rate_within_the_ranges_is_kept_as_it_is(#[case] interval_secs: u32, #[case] burst: u32) {
        let rate = ChatRate::try_new(interval_secs, burst).unwrap();

        assert_eq!(rate.interval_secs(), interval_secs);
        assert_eq!(
            rate.interval(),
            Duration::from_secs(u64::from(interval_secs))
        );
        assert_eq!(rate.burst().get(), burst);
    }

    /// The runtime's old check, moved here with the setting (ADR-0020, S3):
    /// a zero interval would be no limit at all.
    #[rstest]
    #[case::a_zero_interval(0, 3, ChatRateError::Interval { secs: 0 })]
    #[case::an_interval_past_a_minute(61, 3, ChatRateError::Interval { secs: 61 })]
    #[case::the_largest_interval(u32::MAX, 3, ChatRateError::Interval { secs: u32::MAX })]
    #[case::a_zero_burst(3, 0, ChatRateError::Burst { burst: 0 })]
    #[case::a_burst_past_ten(3, 11, ChatRateError::Burst { burst: 11 })]
    fn a_rate_outside_the_ranges_is_refused_never_clamped(
        #[case] interval_secs: u32,
        #[case] burst: u32,
        #[case] error: ChatRateError,
    ) {
        assert_eq!(ChatRate::try_new(interval_secs, burst), Err(error));
    }

    #[test]
    fn the_default_is_one_message_every_3_s_with_a_burst_of_3() {
        let rate = ChatRate::default();

        assert_eq!(rate.interval(), Duration::from_secs(3));
        assert_eq!(rate.burst().get(), 3);
        assert_eq!(ChatRate::try_new(3, 3), Ok(rate));
    }

    #[rstest]
    #[case::interval(
        ChatRateError::Interval { secs: 0 },
        "the chat interval is 0 s; it must be 1 to 60 s"
    )]
    #[case::burst(
        ChatRateError::Burst { burst: 11 },
        "the chat burst is 11 messages; it must be 1 to 10"
    )]
    fn an_error_names_the_number_and_the_range(#[case] error: ChatRateError, #[case] text: &str) {
        assert_eq!(error.to_string(), text);
    }
}
