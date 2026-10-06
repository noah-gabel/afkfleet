//! serde for durations stored as whole milliseconds, the `*_ms` fields of the
//! mode JSON (ADR-0010). Use it with `#[serde(with = "super::millis")]`.

use core::time::Duration;

use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Writes `duration` as whole milliseconds; anything finer is dropped.
///
/// A duration beyond `u64::MAX` milliseconds is an error. A validated mode
/// never has one: its durations are at most 24 h.
pub(super) fn serialize<S: Serializer>(
    duration: &Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    u64::try_from(duration.as_millis())
        .map_err(|_| S::Error::custom("the duration is too long to store in milliseconds"))?
        .serialize(serializer)
}

/// Reads whole milliseconds.
pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Duration, D::Error> {
    u64::deserialize(deserializer).map(Duration::from_millis)
}
