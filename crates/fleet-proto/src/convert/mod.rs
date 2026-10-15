//! Conversions between fleet-core's types and the generated messages, one
//! module per area (ADR-0019).
//!
//! Reading a message is `TryFrom`: it runs every value through fleet-core's
//! constructors and refuses what they refuse. Minecraft's text is cleaned
//! again instead, since the agent that sent it may be compromised. Writing is
//! `From`, or `TryFrom` where a time could be outside the range the wire
//! accepts. The helpers here are the rules every module shares: IDs are 16
//! bytes, times are whole milliseconds since the Unix epoch, and a missing
//! message, oneof or enum is an error.

mod bot;
mod chat;
mod credentials;
mod disconnect;
mod mode;

use chrono::{DateTime, Utc};
use fleet_core::id::IdError;
use uuid::Uuid;

use crate::{ConvertError, Problem};

pub(crate) use chat::chat_message_from_wire;
pub(crate) use credentials::{session_from_wire, session_to_wire};

/// The earliest time the wire accepts: 0000-01-01T00:00:00.000Z, the
/// database's and the API's earliest too.
pub(crate) const EARLIEST_MS: i64 = -62_167_219_200_000;
/// The latest time the wire accepts: 9999-12-31T23:59:59.999Z.
pub(crate) const LATEST_MS: i64 = 253_402_300_799_999;

/// Reads an entity's ID: exactly 16 bytes of a version 7 UUID.
pub(crate) fn id_from_wire<I>(field: &'static str, bytes: &[u8]) -> Result<I, ConvertError>
where
    I: TryFrom<Uuid, Error = IdError>,
{
    // `TryFrom<Uuid>` can only refuse a UUID that isn't version 7.
    I::try_from(uuid_from_wire(field, bytes)?).map_err(|_| ConvertError::new(field, Problem::NotV7))
}

/// Writes an entity's ID as its 16 bytes.
pub(crate) fn id_to_wire<I: Into<Uuid>>(id: I) -> Vec<u8> {
    id.into().as_bytes().to_vec()
}

/// Reads any UUID: exactly 16 bytes.
pub(crate) fn uuid_from_wire(field: &'static str, bytes: &[u8]) -> Result<Uuid, ConvertError> {
    Uuid::from_slice(bytes)
        .map_err(|_| ConvertError::new(field, Problem::WrongLength { len: bytes.len() }))
}

/// Reads a time: whole milliseconds since the Unix epoch, within
/// [`EARLIEST_MS`] and [`LATEST_MS`].
pub(crate) fn time_from_wire(
    field: &'static str,
    millis: Option<i64>,
) -> Result<DateTime<Utc>, ConvertError> {
    let millis = required(field, millis)?;
    if !(EARLIEST_MS..=LATEST_MS).contains(&millis) {
        return Err(ConvertError::new(field, Problem::OutOfRange));
    }
    DateTime::from_timestamp_millis(millis).ok_or(ConvertError::new(field, Problem::OutOfRange))
}

/// Writes a time as whole milliseconds since the Unix epoch. Anything finer
/// than a millisecond is dropped; a time outside the wire's range is refused.
pub(crate) fn time_to_wire(field: &'static str, at: DateTime<Utc>) -> Result<i64, ConvertError> {
    // `timestamp_millis` rounds toward negative infinity, so a time before the
    // epoch keeps its millisecond too.
    let millis = at.timestamp_millis();
    if (EARLIEST_MS..=LATEST_MS).contains(&millis) {
        Ok(millis)
    } else {
        Err(ConvertError::new(field, Problem::OutOfRange))
    }
}

/// Requires a message or a oneof that prost reads as an `Option`.
pub(crate) fn required<T>(field: &'static str, value: Option<T>) -> Result<T, ConvertError> {
    value.ok_or(ConvertError::new(field, Problem::Missing))
}

/// Reads a generated enum's number: 0 (unspecified) is missing, a number this
/// version doesn't know is unknown.
pub(crate) fn enum_from_wire<E>(field: &'static str, number: i32) -> Result<E, ConvertError>
where
    E: TryFrom<i32>,
{
    if number == 0 {
        return Err(ConvertError::new(field, Problem::Missing));
    }
    E::try_from(number).map_err(|_| ConvertError::new(field, Problem::Unknown))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use fleet_core::id::BotId;
    use proptest::prelude::*;
    use rstest::rstest;

    use crate::generated;

    const V7: &str = "018bcfe5-6800-7bab-abab-abababababab";
    const V4: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn bytes(text: &str) -> Vec<u8> {
        text.parse::<Uuid>().unwrap().as_bytes().to_vec()
    }

    #[test]
    fn an_id_crosses_as_its_16_bytes() {
        let id: BotId = V7.parse().unwrap();
        assert_eq!(id_to_wire(id), bytes(V7));
        assert_eq!(id_from_wire::<BotId>("Test.id", &bytes(V7)), Ok(id));
    }

    #[rstest]
    #[case::empty(0)]
    #[case::fifteen(15)]
    #[case::seventeen(17)]
    fn an_id_of_another_length_is_refused(#[case] len: usize) {
        assert_eq!(
            id_from_wire::<BotId>("Test.id", &vec![0x11; len]),
            Err(ConvertError::new("Test.id", Problem::WrongLength { len }))
        );
    }

    #[test]
    fn an_id_that_isnt_v7_is_refused() {
        assert_eq!(
            id_from_wire::<BotId>("Test.id", &bytes(V4)),
            Err(ConvertError::new("Test.id", Problem::NotV7))
        );
    }

    #[test]
    fn any_uuid_of_16_bytes_is_read() {
        assert_eq!(
            uuid_from_wire("Test.uuid", &bytes(V4)),
            Ok(V4.parse().unwrap())
        );
        assert_eq!(uuid_from_wire("Test.uuid", &[0; 16]), Ok(Uuid::nil()));
        assert_eq!(
            uuid_from_wire("Test.uuid", &[0; 3]),
            Err(ConvertError::new(
                "Test.uuid",
                Problem::WrongLength { len: 3 }
            ))
        );
    }

    #[rstest]
    #[case::epoch(0)]
    #[case::earliest(EARLIEST_MS)]
    #[case::latest(LATEST_MS)]
    #[case::before_the_epoch(-1)]
    fn a_time_in_range_round_trips(#[case] millis: i64) {
        let at = time_from_wire("Test.at", Some(millis)).unwrap();
        assert_eq!(at.timestamp_millis(), millis);
        assert_eq!(time_to_wire("Test.at", at), Ok(millis));
    }

    #[test]
    fn the_range_is_years_0_to_9999() {
        fn parts(at: DateTime<Utc>) -> (i32, u32, u32, u32, u32, u32, u32) {
            use chrono::{Datelike, Timelike};
            (
                at.year(),
                at.month(),
                at.day(),
                at.hour(),
                at.minute(),
                at.second(),
                at.timestamp_subsec_millis(),
            )
        }
        let earliest = time_from_wire("Test.at", Some(EARLIEST_MS)).unwrap();
        let latest = time_from_wire("Test.at", Some(LATEST_MS)).unwrap();
        assert_eq!(parts(earliest), (0, 1, 1, 0, 0, 0, 0));
        assert_eq!(parts(latest), (9999, 12, 31, 23, 59, 59, 999));
    }

    #[rstest]
    #[case::one_before(EARLIEST_MS - 1)]
    #[case::one_after(LATEST_MS + 1)]
    #[case::smallest(i64::MIN)]
    #[case::largest(i64::MAX)]
    fn a_time_out_of_range_is_refused(#[case] millis: i64) {
        assert_eq!(
            time_from_wire("Test.at", Some(millis)),
            Err(ConvertError::new("Test.at", Problem::OutOfRange))
        );
    }

    #[test]
    fn a_missing_time_is_refused() {
        assert_eq!(
            time_from_wire("Test.at", None),
            Err(ConvertError::new("Test.at", Problem::Missing))
        );
    }

    #[test]
    fn writing_a_time_drops_what_is_finer_than_a_millisecond() {
        let at = DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap();
        assert_eq!(time_to_wire("Test.at", at), Ok(1_700_000_000_123));
    }

    #[test]
    fn writing_a_time_out_of_range_is_refused() {
        let late = DateTime::from_timestamp_millis(LATEST_MS).unwrap() + TimeDelta::milliseconds(1);
        assert_eq!(
            time_to_wire("Test.at", late),
            Err(ConvertError::new("Test.at", Problem::OutOfRange))
        );
        let early =
            DateTime::from_timestamp_millis(EARLIEST_MS).unwrap() - TimeDelta::milliseconds(1);
        assert_eq!(
            time_to_wire("Test.at", early),
            Err(ConvertError::new("Test.at", Problem::OutOfRange))
        );
    }

    #[test]
    fn an_enum_reads_its_known_values() {
        assert_eq!(
            enum_from_wire::<generated::DesiredRunState>("Test.state", 1),
            Ok(generated::DesiredRunState::Running)
        );
    }

    #[test]
    fn an_unspecified_enum_is_missing() {
        assert_eq!(
            enum_from_wire::<generated::DesiredRunState>("Test.state", 0),
            Err(ConvertError::new("Test.state", Problem::Missing))
        );
    }

    #[rstest]
    #[case::next(3)]
    #[case::negative(-1)]
    fn an_unknown_enum_value_is_refused(#[case] number: i32) {
        assert_eq!(
            enum_from_wire::<generated::DesiredRunState>("Test.state", number),
            Err(ConvertError::new("Test.state", Problem::Unknown))
        );
    }

    #[test]
    fn a_required_value_must_be_there() {
        assert_eq!(required("Test.part", Some(1)), Ok(1));
        assert_eq!(
            required::<u8>("Test.part", None),
            Err(ConvertError::new("Test.part", Problem::Missing))
        );
    }

    proptest! {
        #[test]
        fn every_i64_is_refused_or_round_trips(millis in prop_oneof![
            any::<i64>(),
            // Around the wire's range, and beyond it where chrono still has times.
            (EARLIEST_MS - 10_000_000_000_000)..=(LATEST_MS + 10_000_000_000_000),
            (EARLIEST_MS - 2)..=(EARLIEST_MS + 2),
            (LATEST_MS - 2)..=(LATEST_MS + 2),
        ]) {
            match time_from_wire("Test.at", Some(millis)) {
                Ok(at) => prop_assert_eq!(time_to_wire("Test.at", at), Ok(millis)),
                Err(error) => {
                    prop_assert!(!(EARLIEST_MS..=LATEST_MS).contains(&millis));
                    prop_assert_eq!(error, ConvertError::new("Test.at", Problem::OutOfRange));
                }
            }
        }

        #[test]
        fn every_byte_string_is_refused_or_round_trips(raw in proptest::collection::vec(any::<u8>(), 0..20)) {
            if let Ok(id) = id_from_wire::<BotId>("Test.id", &raw) {
                prop_assert_eq!(id_to_wire(id), raw);
            }
        }

        #[test]
        fn every_v7_id_round_trips(millis in 0..(1_i64 << 48), random in any::<[u8; 10]>()) {
            let id = BotId::new_v7(DateTime::from_timestamp_millis(millis).unwrap(), random).unwrap();
            prop_assert_eq!(id_from_wire::<BotId>("Test.id", &id_to_wire(id)), Ok(id));
        }
    }
}
