//! [`SafeInt`]: the one way a 64-bit integer crosses the API (ADR-0015).
//!
//! JSON carries a number as text, and JavaScript reads it into a 64-bit
//! float, which holds an integer exactly only within ±(2^53 − 1)
//! (`Number.MAX_SAFE_INTEGER`). A bigger value would arrive changed, without
//! an error. So a DTO never holds a plain `i64` or `u64` (ts-rs would export
//! it as `bigint`, which `JSON.parse` never produces): it holds a `SafeInt`,
//! which can't hold a value outside that range and is a `number` in
//! TypeScript. 32-bit integers and smaller are `number`s already.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;

/// An integer within ±(2^53 − 1), which JavaScript reads exactly. It's a
/// plain JSON number on the wire and `number` in TypeScript; reading a number
/// outside the range fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(type = "number")]
pub struct SafeInt(i64);

/// Why an integer can't be a [`SafeInt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SafeIntError {
    /// The integer is outside ±[`SafeInt::MAX`].
    #[error("the integer is outside ±(2^53 − 1), so JavaScript can't read it exactly")]
    OutOfRange,
}

impl SafeInt {
    /// The largest value, 2^53 − 1: JavaScript's `Number.MAX_SAFE_INTEGER`.
    pub const MAX: i64 = (1 << 53) - 1;

    /// The smallest value, −(2^53 − 1): JavaScript's `Number.MIN_SAFE_INTEGER`.
    pub const MIN: i64 = -Self::MAX;

    /// Wraps `value`.
    ///
    /// # Errors
    /// [`SafeIntError::OutOfRange`] outside [`SafeInt::MIN`]..=[`SafeInt::MAX`].
    pub const fn try_new(value: i64) -> Result<Self, SafeIntError> {
        if Self::MIN <= value && value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(SafeIntError::OutOfRange)
        }
    }

    /// The value.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for SafeInt {
    type Error = SafeIntError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<SafeInt> for i64 {
    fn from(value: SafeInt) -> Self {
        value.get()
    }
}

// By hand rather than serde's `try_from`/`into`, which ts-rs can't read and
// warns about on every build.
impl Serialize for SafeInt {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for SafeInt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i64::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::audit::AuditMetadata;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::zero(0)]
    #[case::minus_one(-1)]
    #[case::max(SafeInt::MAX)]
    #[case::min(SafeInt::MIN)]
    fn a_value_within_the_safe_range_is_kept(#[case] value: i64) {
        assert_eq!(SafeInt::try_new(value).unwrap().get(), value);
    }

    #[rstest]
    #[case::just_above(SafeInt::MAX + 1)]
    #[case::just_below(SafeInt::MIN - 1)]
    #[case::i64_max(i64::MAX)]
    #[case::i64_min(i64::MIN)]
    fn a_value_outside_the_safe_range_is_refused(#[case] value: i64) {
        assert_eq!(SafeInt::try_new(value), Err(SafeIntError::OutOfRange));
        assert_eq!(SafeInt::try_from(value), Err(SafeIntError::OutOfRange));
    }

    #[test]
    fn the_bounds_are_javascripts_safe_integers() {
        assert_eq!(SafeInt::MAX, 9_007_199_254_740_991);
        assert_eq!(SafeInt::MIN, -9_007_199_254_740_991);
    }

    #[test]
    fn the_bound_equals_the_audit_metadatas_integer_bound() {
        assert_eq!(SafeInt::MAX, AuditMetadata::MAX_INT);
    }

    #[test]
    fn it_is_a_plain_json_number() {
        let value = SafeInt::try_new(-42).unwrap();

        assert_eq!(serde_json::to_string(&value).unwrap(), "-42");
        assert_eq!(serde_json::from_str::<SafeInt>("-42").unwrap(), value);
        assert_eq!(i64::from(value), -42);
    }

    #[rstest]
    #[case::max("9007199254740991", SafeInt::MAX)]
    #[case::min("-9007199254740991", SafeInt::MIN)]
    fn the_edges_deserialize(#[case] json: &str, #[case] expected: i64) {
        assert_eq!(
            serde_json::from_str::<SafeInt>(json).unwrap().get(),
            expected
        );
    }

    #[rstest]
    #[case::just_above("9007199254740992")]
    #[case::just_below("-9007199254740992")]
    #[case::u64_max("18446744073709551615")]
    fn deserializing_a_number_outside_the_range_fails(#[case] json: &str) {
        assert!(serde_json::from_str::<SafeInt>(json).is_err());
    }
}
