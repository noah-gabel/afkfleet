//! [`AgentCapacity`]: how many bots an agent may run at once.

use core::fmt;
use core::num::NonZeroU16;

/// Why a number isn't a valid [`AgentCapacity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentCapacityError {
    /// The number isn't 1 to 1000.
    #[error("an agent's capacity is {min} to {max} bots, not {value}", min = AgentCapacity::MIN, max = AgentCapacity::MAX)]
    OutOfRange {
        /// The number given.
        value: u32,
    },
}

/// How many bots an agent may run at once: its `[runtime] max_bots`, which it
/// sends in its hello (Plan.md P10.1; ADR-0019).
///
/// It's 1 to 1000, the agent config's range. The server never assigns an
/// agent more bots than this, so a value outside it is refused, never
/// clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentCapacity(NonZeroU16);

impl AgentCapacity {
    /// The smallest capacity.
    pub const MIN: u16 = 1;
    /// The largest capacity.
    pub const MAX: u16 = 1000;

    /// Returns the number of bots.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u32> for AgentCapacity {
    type Error = AgentCapacityError;

    fn try_from(value: u32) -> Result<Self, AgentCapacityError> {
        u16::try_from(value)
            .ok()
            .filter(|bots| (Self::MIN..=Self::MAX).contains(bots))
            .and_then(NonZeroU16::new)
            .map(Self)
            .ok_or(AgentCapacityError::OutOfRange { value })
    }
}

impl From<AgentCapacity> for u32 {
    fn from(capacity: AgentCapacity) -> Self {
        Self::from(capacity.get())
    }
}

impl fmt::Display for AgentCapacity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case::smallest(1)]
    #[case::the_agent_default(50)]
    #[case::largest(1000)]
    fn accepts_1_to_1000(#[case] value: u32) {
        let capacity = AgentCapacity::try_from(value).unwrap();
        assert_eq!(u32::from(capacity.get()), value);
        assert_eq!(u32::from(capacity), value);
        assert_eq!(capacity.to_string(), value.to_string());
    }

    #[rstest]
    #[case::zero(0)]
    #[case::one_too_many(1001)]
    #[case::above_u16(65_537)]
    #[case::largest_u32(u32::MAX)]
    fn refuses_everything_else(#[case] value: u32) {
        assert_eq!(
            AgentCapacity::try_from(value),
            Err(AgentCapacityError::OutOfRange { value })
        );
    }

    #[test]
    fn the_constants_are_the_range() {
        assert_eq!((AgentCapacity::MIN, AgentCapacity::MAX), (1, 1000));
    }

    #[test]
    fn the_error_has_a_fixed_message() {
        assert_eq!(
            AgentCapacityError::OutOfRange { value: 0 }.to_string(),
            "an agent's capacity is 1 to 1000 bots, not 0"
        );
    }

    proptest! {
        #[test]
        fn accepts_exactly_the_range(value in prop_oneof![0..=1100_u32, any::<u32>()]) {
            let expected = (1..=1000).contains(&value);
            prop_assert_eq!(AgentCapacity::try_from(value).is_ok(), expected);
        }
    }
}
