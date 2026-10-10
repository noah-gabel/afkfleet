//! The one place IDs and times turn into column text and back (ADR-0015).
//!
//! - **IDs** are stored as lowercase hyphenated text (`uuid::fmt::Hyphenated`).
//!   Code never binds a plain `Uuid`: sqlx would encode it as a 16-byte BLOB,
//!   which never equals the TEXT column, so a lookup would silently find
//!   nothing.
//! - **Times** are stored as `YYYY-MM-DDTHH:MM:SS.mmmZ`: UTC, milliseconds,
//!   always 24 characters, so text order is time order and `expires_at > ?`
//!   compares correctly.

use chrono::{DateTime, Datelike, NaiveDateTime, SubsecRound, Utc};
use uuid::Uuid;
use uuid::fmt::Hyphenated;

/// The stored form of a time.
const TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// The length of every stored time.
const TIME_LEN: usize = 24;

/// The text form an ID is bound as.
pub(super) fn id_text(id: impl Into<Uuid>) -> Hyphenated {
    id.into().hyphenated()
}

/// Reads a stored ID: only the canonical lowercase hyphenated form of a UUID
/// the ID type accepts (a version 7 one). `None` for anything else.
pub(super) fn parse_id<I: TryFrom<Uuid>>(text: &str) -> Option<I> {
    let uuid = Uuid::try_parse(text).ok()?;
    if uuid.hyphenated().to_string() != text {
        return None;
    }
    I::try_from(uuid).ok()
}

/// The stored form of `at`, truncated to milliseconds. `None` for a year
/// outside 0 to 9999, which doesn't fit four digits.
pub(super) fn time_text(at: DateTime<Utc>) -> Option<String> {
    (0..=9999)
        .contains(&at.year())
        .then(|| at.trunc_subsecs(3).format(TIME_FORMAT).to_string())
}

/// Reads a stored time. `None` for anything but the exact stored form.
pub(super) fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    if text.len() != TIME_LEN {
        return None;
    }
    let at = NaiveDateTime::parse_from_str(text, TIME_FORMAT)
        .ok()?
        .and_utc();
    // The length check and the format leave no room for another form, but
    // only a time that writes back to exactly this text is accepted.
    (time_text(at).as_deref() == Some(text)).then_some(at)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use fleet_core::id::UserId;
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    #[test]
    fn an_id_is_bound_as_lowercase_hyphenated_text() {
        let id: UserId = "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap();

        assert_eq!(
            id_text(id).to_string(),
            "018bcfe5-6800-7bab-abab-abababababab"
        );
    }

    #[test]
    fn a_stored_id_reads_back() {
        let id: Option<UserId> = parse_id("018bcfe5-6800-7bab-abab-abababababab");

        assert_eq!(
            id.unwrap().to_string(),
            "018bcfe5-6800-7bab-abab-abababababab"
        );
    }

    #[rstest]
    #[case::uppercase("018BCFE5-6800-7BAB-ABAB-ABABABABABAB")]
    #[case::simple("018bcfe568007babababababababab")]
    #[case::braced("{018bcfe5-6800-7bab-abab-abababababab}")]
    #[case::v4("550e8400-e29b-41d4-a716-446655440000")]
    #[case::empty("")]
    fn other_id_forms_dont_read_back(#[case] text: &str) {
        assert_eq!(parse_id::<UserId>(text), None);
    }

    #[test]
    fn a_time_is_stored_in_milliseconds() {
        let at = Utc.with_ymd_and_hms(2026, 10, 10, 12, 0, 0).unwrap()
            + chrono::TimeDelta::nanoseconds(123_456_789);

        assert_eq!(time_text(at).unwrap(), "2026-10-10T12:00:00.123Z");
    }

    #[rstest]
    #[case::first_year(0, "0000-01-01T00:00:00.000Z")]
    #[case::last_year(9999, "9999-01-01T00:00:00.000Z")]
    fn the_years_0_to_9999_are_stored(#[case] year: i32, #[case] text: &str) {
        let at = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap();

        assert_eq!(time_text(at).as_deref(), Some(text));
        assert_eq!(parse_time(text), Some(at));
    }

    #[rstest]
    #[case::before_year_0(-1)]
    #[case::after_9999(10_000)]
    fn other_years_cant_be_stored(#[case] year: i32) {
        let at = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap();

        assert_eq!(time_text(at), None);
    }

    #[rstest]
    #[case::no_millis("2026-10-10T12:00:00Z")]
    #[case::two_digits("2026-10-10T12:00:00.12Z")]
    #[case::offset("2026-10-10T12:00:00.123+00:00")]
    #[case::space("2026-10-10 12:00:00.123Z")]
    #[case::no_zone("2026-10-10T12:00:00.123")]
    #[case::invalid_date("2026-02-30T12:00:00.123Z")]
    #[case::empty("")]
    fn other_time_forms_dont_read_back(#[case] text: &str) {
        assert_eq!(parse_time(text), None);
    }

    /// Any time in the storable range, to the nanosecond.
    fn storable_time() -> impl Strategy<Value = DateTime<Utc>> {
        let first = Utc.with_ymd_and_hms(0, 1, 1, 0, 0, 0).unwrap().timestamp();
        let last = Utc
            .with_ymd_and_hms(9999, 12, 31, 23, 59, 59)
            .unwrap()
            .timestamp();
        (first..=last, 0..1_000_000_000_u32)
            .prop_map(|(secs, nanos)| DateTime::from_timestamp(secs, nanos).unwrap())
    }

    proptest! {
        #[test]
        fn a_time_reads_back_truncated_to_milliseconds(at in storable_time()) {
            let text = time_text(at).unwrap();

            prop_assert_eq!(text.len(), TIME_LEN);
            prop_assert_eq!(parse_time(&text), Some(at.trunc_subsecs(3)));
        }

        #[test]
        fn text_order_is_time_order(a in storable_time(), b in storable_time()) {
            let order = time_text(a).unwrap().cmp(&time_text(b).unwrap());

            prop_assert_eq!(order, a.trunc_subsecs(3).cmp(&b.trunc_subsecs(3)));
        }
    }
}
