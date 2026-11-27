//! The time of day in a named time zone: what `clock.local(zone)` reads
//! (Plan.md P16.21; ADR-0052).
//!
//! **The moment is the hook's,** the runtime's clock as every other time of
//! a script's: this reads no clock. **The zones are IANA's database as jiff
//! compiles it in** (`tzdb-bundle-always`), never the system's: an agent's
//! container may hold no zone files, and a hook reads no file. Summer time
//! is the database's rules, so a fixed offset never stands in for a zone.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use jiff::Timestamp;
use jiff::tz::{TimeZone, TimeZoneDatabase};

/// The zones compiled into this build: read once, shared by every script.
static ZONES: LazyLock<TimeZoneDatabase> = LazyLock::new(TimeZoneDatabase::bundled);

/// A moment as a clock on the wall of a time zone shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalTime {
    /// The year.
    pub(crate) year: i16,
    /// The month, 1 to 12.
    pub(crate) month: i8,
    /// The day of the month, 1 to 31.
    pub(crate) day: i8,
    /// The hour, 0 to 23.
    pub(crate) hour: i8,
    /// The minute, 0 to 59.
    pub(crate) minute: i8,
    /// The second, 0 to 59.
    pub(crate) second: i8,
    /// The day of the week, 1 for Monday to 7 for Sunday (ISO 8601).
    pub(crate) weekday: i8,
    /// How far the zone is ahead of UTC, in minutes: 60 for Berlin in
    /// winter, 120 in summer.
    pub(crate) offset_minutes: i32,
}

/// The zone named `name` in the IANA database compiled in, such as
/// `Europe/Berlin`; `None` for a name it doesn't hold.
pub(crate) fn zone(name: &str) -> Option<TimeZone> {
    ZONES.get(name).ok()
}

/// The moment `at` as the clocks of `zone` show it; `None` only for a
/// moment outside what the database covers (years -9999 to 9999).
pub(crate) fn local_time(zone: &TimeZone, at: DateTime<Utc>) -> Option<LocalTime> {
    // A second is all a time of day shows.
    let instant = Timestamp::from_second(at.timestamp()).ok()?;
    let wall = zone.to_datetime(instant);
    Some(LocalTime {
        year: wall.year(),
        month: wall.month(),
        day: wall.day(),
        hour: wall.hour(),
        minute: wall.minute(),
        second: wall.second(),
        weekday: wall.weekday().to_monday_one_offset(),
        offset_minutes: zone.to_offset(instant).seconds() / 60,
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    fn berlin() -> TimeZone {
        zone("Europe/Berlin").unwrap()
    }

    /// `at` in UTC: year, month, day, hour, minute.
    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    /// The hour, minute, weekday and offset Berlin's clocks show at `at`.
    fn shown(at: DateTime<Utc>) -> (i8, i8, i8, i32) {
        let local = local_time(&berlin(), at).unwrap();
        (
            local.hour,
            local.minute,
            local.weekday,
            local.offset_minutes,
        )
    }

    // --- The last Sunday of March 2026, the 29th: 02:00 is skipped ---

    #[rstest]
    #[case::the_last_minute_of_winter_time(utc(2026, 3, 29, 0, 59), (1, 59, 7, 60))]
    #[case::the_switch(utc(2026, 3, 29, 1, 0), (3, 0, 7, 120))]
    #[case::a_minute_after(utc(2026, 3, 29, 1, 1), (3, 1, 7, 120))]
    #[case::the_afternoon(utc(2026, 3, 29, 12, 0), (14, 0, 7, 120))]
    fn on_the_last_sunday_of_march_berlin_skips_the_hour_from_two_to_three(
        #[case] at: DateTime<Utc>,
        #[case] expected: (i8, i8, i8, i32),
    ) {
        assert_eq!(shown(at), expected);
    }

    #[test]
    fn no_moment_of_the_spring_switch_day_shows_two_oclock_in_berlin() {
        let midnight = utc(2026, 3, 28, 23, 0);
        let hours: Vec<i8> = (0..24 * 60)
            .map(|minute| {
                shown(fleet_core::time::add(
                    midnight,
                    core::time::Duration::from_secs(minute * 60),
                ))
                .0
            })
            .collect();

        assert!(!hours.contains(&2));
        assert_eq!(hours.first(), Some(&0));
    }

    // --- The last Sunday of October 2026, the 25th: 02:00 to 03:00 comes twice ---

    #[rstest]
    #[case::the_first_two_oclock(utc(2026, 10, 25, 0, 0), (2, 0, 7, 120))]
    #[case::the_last_minute_of_summer_time(utc(2026, 10, 25, 0, 59), (2, 59, 7, 120))]
    #[case::the_second_two_oclock(utc(2026, 10, 25, 1, 0), (2, 0, 7, 60))]
    #[case::the_second_half_past_two(utc(2026, 10, 25, 1, 30), (2, 30, 7, 60))]
    #[case::three_oclock(utc(2026, 10, 25, 2, 0), (3, 0, 7, 60))]
    fn on_the_last_sunday_of_october_berlin_shows_the_hour_from_two_to_three_twice(
        #[case] at: DateTime<Utc>,
        #[case] expected: (i8, i8, i8, i32),
    ) {
        assert_eq!(shown(at), expected);
    }

    // --- Midnight ---

    #[rstest]
    #[case::the_last_minute_of_a_saturday(utc(2026, 11, 28, 22, 59), (23, 59, 6, 60))]
    #[case::midnight_is_sunday(utc(2026, 11, 28, 23, 0), (0, 0, 7, 60))]
    #[case::the_last_minute_of_a_sunday(utc(2026, 11, 29, 22, 59), (23, 59, 7, 60))]
    #[case::midnight_is_monday(utc(2026, 11, 29, 23, 0), (0, 0, 1, 60))]
    #[case::summer_midnight(utc(2026, 7, 4, 22, 0), (0, 0, 7, 120))]
    fn the_weekday_changes_at_berlins_midnight_not_at_utcs(
        #[case] at: DateTime<Utc>,
        #[case] expected: (i8, i8, i8, i32),
    ) {
        assert_eq!(shown(at), expected);
    }

    #[test]
    fn a_moment_reads_its_whole_date_and_second() {
        let at = Utc.with_ymd_and_hms(2026, 12, 31, 23, 30, 45).unwrap();

        assert_eq!(
            local_time(&berlin(), at),
            Some(LocalTime {
                year: 2027,
                month: 1,
                day: 1,
                hour: 0,
                minute: 30,
                second: 45,
                weekday: 5,
                offset_minutes: 60,
            })
        );
    }

    #[rstest]
    #[case::utc("UTC", (12, 0, 4, 0))]
    #[case::tokyo("Asia/Tokyo", (21, 0, 4, 540))]
    #[case::new_york("America/New_York", (7, 0, 4, -300))]
    #[case::kolkata("Asia/Kolkata", (17, 30, 4, 330))]
    fn other_zones_read_their_own_rules(#[case] name: &str, #[case] expected: (i8, i8, i8, i32)) {
        let local = local_time(&zone(name).unwrap(), utc(2026, 11, 26, 12, 0)).unwrap();

        assert_eq!(
            (
                local.hour,
                local.minute,
                local.weekday,
                local.offset_minutes
            ),
            expected
        );
    }

    #[rstest]
    #[case::a_typo("Europe/Berln")]
    #[case::a_city_alone("Berlin")]
    #[case::an_offset("+01:00")]
    #[case::a_posix_rule("CET-1CEST,M3.5.0,M10.5.0/3")]
    #[case::empty("")]
    #[case::a_path("../../etc/passwd")]
    fn a_name_the_database_doesnt_hold_is_no_zone(#[case] name: &str) {
        assert!(zone(name).is_none(), "{name}");
    }

    /// The EU's rule for Berlin since 1996, written out apart from the zone
    /// data: summer time from 01:00 UTC on the last Sunday of March to 01:00
    /// UTC on the last Sunday of October.
    fn eu_summer_time(at: DateTime<Utc>) -> bool {
        use chrono::Datelike as _;
        let last_sunday = |month: u32| {
            let next = Utc
                .with_ymd_and_hms(at.year(), month + 1, 1, 1, 0, 0)
                .unwrap();
            let back = (next.weekday().num_days_from_sunday() + 6) % 7 + 1;
            next - chrono::Days::new(u64::from(back))
        };
        at >= last_sunday(3) && at < last_sunday(10)
    }

    proptest! {
        #[test]
        fn berlin_follows_the_eu_rule_and_its_clock_is_utc_plus_its_offset(
            seconds in 852_076_800_i64..2_145_916_800_i64,
        ) {
            // 1997 to 2037: the EU rule's years in the data.
            let at = DateTime::<Utc>::from_timestamp(seconds, 0).unwrap();
            let local = local_time(&berlin(), at).unwrap();

            let summer = eu_summer_time(at);
            prop_assert_eq!(local.offset_minutes, if summer { 120 } else { 60 });
            let wall = at + chrono::Duration::minutes(i64::from(local.offset_minutes));
            {
                use chrono::{Datelike as _, Timelike as _};
                prop_assert_eq!(i32::from(local.year), wall.year());
                prop_assert_eq!(u32::try_from(local.month).unwrap(), wall.month());
                prop_assert_eq!(u32::try_from(local.day).unwrap(), wall.day());
                prop_assert_eq!(u32::try_from(local.hour).unwrap(), wall.hour());
                prop_assert_eq!(u32::try_from(local.minute).unwrap(), wall.minute());
                prop_assert_eq!(u32::try_from(local.second).unwrap(), wall.second());
                prop_assert_eq!(
                    u32::try_from(local.weekday).unwrap(),
                    wall.weekday().number_from_monday()
                );
            }
        }

        #[test]
        fn every_zone_in_the_database_reads_every_moment(
            pick in any::<prop::sample::Index>(),
            seconds in -2_208_988_800_i64..4_102_444_800_i64,
        ) {
            // 1900 to 2100, in every zone the build holds.
            let names: Vec<String> = ZONES.available().map(|name| name.to_string()).collect();
            prop_assume!(!names.is_empty());
            let name = pick.get(&names);
            let at = DateTime::<Utc>::from_timestamp(seconds, 0).unwrap();

            let local = local_time(&zone(name).unwrap(), at).unwrap();

            prop_assert!((1..=12).contains(&local.month));
            prop_assert!((1..=31).contains(&local.day));
            prop_assert!((0..=23).contains(&local.hour));
            prop_assert!((0..=59).contains(&local.minute));
            prop_assert!((1..=7).contains(&local.weekday));
            prop_assert!(local.offset_minutes.abs() <= 26 * 60);
        }

        #[test]
        fn any_name_is_a_zone_or_none_and_never_panics(name in any::<String>()) {
            let _ = zone(&name);
        }
    }

    #[test]
    fn the_database_is_compiled_in_with_the_zones_scripts_name() {
        let names: Vec<String> = ZONES.available().map(|name| name.to_string()).collect();

        assert!(names.len() > 300, "{}", names.len());
        for name in ["Europe/Berlin", "Europe/London", "America/New_York", "UTC"] {
            assert!(names.iter().any(|known| known == name), "{name}");
        }
    }
}
