//! [`Timestamp`]: a point in time in a response (Plan.md P7.6, P7.9,
//! ADR-0016).
//!
//! Always `YYYY-MM-DDTHH:MM:SS.mmmZ`: UTC with milliseconds, 24 characters,
//! the form the server's database stores times in, and a valid RFC 3339
//! time JavaScript's `Date` reads. It's written and read by hand, so the
//! form is exactly this one: reading refuses any other (an offset, no
//! milliseconds, a leap second, a year without four digits), and its error
//! never quotes the text.
//!
//! Like [`BoundedText`](crate::BoundedText), it has no TypeScript type or
//! `OpenAPI` component of its own: a field holding one carries `#[ts(type =
//! "string")]` (an optional one `#[ts(as = "Option<String>", optional)]`)
//! and `#[schema(schema_with = Timestamp::schema)]`.

use core::fmt;

use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::PartialSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};

/// The characters of every [`Timestamp`].
pub const TIMESTAMP_CHARS: usize = 24;

/// A point in time, to the millisecond, within the years 0 to 9999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(DateTime<Utc>);

/// Why a time can't be a [`Timestamp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a time outside the years 0 to 9999 has no four-digit year")]
pub struct TimestampError;

impl Timestamp {
    /// The time.
    #[must_use]
    pub const fn get(self) -> DateTime<Utc> {
        self.0
    }

    /// The text form.
    fn text(self) -> String {
        let at = self.0;
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            at.year(),
            at.month(),
            at.day(),
            at.hour(),
            at.minute(),
            at.second(),
            at.timestamp_subsec_millis()
        )
    }

    /// Reads exactly the text form, or nothing: each field's ASCII digits
    /// at its place, a real date and time, no leap second.
    fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != TIMESTAMP_CHARS {
            return None;
        }
        let separators = [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'.'),
        ];
        if separators
            .iter()
            .any(|&(at, separator)| bytes.get(at) != Some(&separator))
            || bytes.get(23) != Some(&b'Z')
        {
            return None;
        }
        let number = |from: usize, to: usize| -> Option<u32> {
            bytes.get(from..to)?.iter().try_fold(0_u32, |value, byte| {
                byte.is_ascii_digit()
                    .then(|| value * 10 + u32::from(byte - b'0'))
            })
        };
        let year = i32::try_from(number(0, 4)?).ok()?;
        let at = NaiveDate::from_ymd_opt(year, number(5, 7)?, number(8, 10)?)?
            .and_hms_milli_opt(
                number(11, 13)?,
                number(14, 16)?,
                number(17, 19)?,
                number(20, 23)?,
            )?
            .and_utc();
        // chrono takes milliseconds past 999 as a leap second; the form
        // has none, so only a time that writes back to this text counts.
        let time = Self(at);
        (time.text() == text).then_some(time)
    }
}

/// Keeps the milliseconds of `at` and drops the rest.
impl TryFrom<DateTime<Utc>> for Timestamp {
    type Error = TimestampError;

    fn try_from(at: DateTime<Utc>) -> Result<Self, TimestampError> {
        if !(0..=9999).contains(&at.year()) {
            return Err(TimestampError);
        }
        DateTime::from_timestamp_millis(at.timestamp_millis())
            .map(Self)
            .ok_or(TimestampError)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw)
            .ok_or_else(|| D::Error::custom("not a time of the form YYYY-MM-DDTHH:MM:SS.mmmZ"))
    }
}

/// A string of exactly [`TIMESTAMP_CHARS`] characters; the schema says at
/// most, since the renderer that keeps it equal to the TypeScript knows no
/// minimum.
impl PartialSchema for Timestamp {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .max_length(Some(TIMESTAMP_CHARS))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    fn read(json: &str) -> Result<Timestamp, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn a_time_is_written_in_utc_with_milliseconds() {
        let time = Timestamp::try_from(at(1_800_000_015_007)).unwrap();

        assert_eq!(
            serde_json::to_string(&time).unwrap(),
            r#""2027-01-15T08:00:15.007Z""#
        );
        assert_eq!(time.to_string(), "2027-01-15T08:00:15.007Z");
    }

    #[test]
    fn a_time_keeps_its_milliseconds_and_drops_the_rest() {
        let precise = at(1_800_000_015_007) + chrono::TimeDelta::nanoseconds(999_999);

        let time = Timestamp::try_from(precise).unwrap();

        assert_eq!(time.get(), at(1_800_000_015_007));
    }

    #[rstest]
    #[case::the_epoch(0, "1970-01-01T00:00:00.000Z")]
    #[case::year_zero(-62_167_219_200_000, "0000-01-01T00:00:00.000Z")]
    #[case::the_last_millisecond(253_402_300_799_999, "9999-12-31T23:59:59.999Z")]
    fn the_edges_of_the_four_digit_years_are_written(#[case] millis: i64, #[case] text: &str) {
        let time = Timestamp::try_from(at(millis)).unwrap();

        assert_eq!(time.to_string(), text);
        assert_eq!(read(&format!("\"{text}\"")).unwrap(), time);
    }

    #[rstest]
    #[case::before_year_zero(-62_167_219_200_001)]
    #[case::past_year_9999(253_402_300_800_000)]
    fn a_time_without_a_four_digit_year_is_refused(#[case] millis: i64) {
        assert_eq!(Timestamp::try_from(at(millis)), Err(TimestampError));
    }

    #[test]
    fn a_written_time_reads_back() {
        let time = Timestamp::try_from(at(1_800_000_015_007)).unwrap();

        let back = read(&serde_json::to_string(&time).unwrap()).unwrap();

        assert_eq!(back, time);
    }

    #[rstest]
    #[case::no_milliseconds("2027-01-15T08:00:15Z")]
    #[case::microseconds("2027-01-15T08:00:15.007000Z")]
    #[case::an_offset("2027-01-15T08:00:15.007+00:00")]
    #[case::a_lowercase_z("2027-01-15T08:00:15.007z")]
    #[case::a_space("2027-01-15 08:00:15.007Z")]
    #[case::no_zone("2027-01-15T08:00:15.007")]
    #[case::a_leap_second("2016-12-31T23:59:60.000Z")]
    #[case::the_thirtieth_of_february("2027-02-30T08:00:15.007Z")]
    #[case::month_thirteen("2027-13-15T08:00:15.007Z")]
    #[case::hour_twenty_four("2027-01-15T24:00:00.000Z")]
    #[case::a_sign("+2027-01-15T08:00:15.007Z")]
    #[case::a_digit_that_isnt_ascii("2027-01-15T08:00:15.00٧Z")]
    #[case::empty("")]
    fn any_other_form_is_refused_without_quoting_it(#[case] text: &str) {
        let error = read(&format!("\"{text}\"")).unwrap_err().to_string();

        assert!(
            error.starts_with("not a time of the form YYYY-MM-DDTHH:MM:SS.mmmZ"),
            "{text}: {error}"
        );
        if !text.is_empty() {
            assert!(!error.contains(text), "{error}");
        }
    }

    #[test]
    fn a_value_that_isnt_a_string_is_refused() {
        assert!(read("1800000015007").is_err());
    }

    #[test]
    fn the_schema_is_a_string_of_its_length() {
        let schema = serde_json::to_value(Timestamp::schema()).unwrap();

        assert_eq!(
            schema,
            serde_json::json!({ "type": "string", "maxLength": 24 })
        );
    }
}
