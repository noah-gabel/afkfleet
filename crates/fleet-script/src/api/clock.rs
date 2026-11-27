//! `clock`: the time of day of the hook's moment in a named time zone
//! (Plan.md P16.21; ADR-0052). API level 2.

use rhai::{Dynamic, Map, Module};

use super::{Registered, Returns, int, misuse, text};
use crate::clock::{LocalTime, local_time, zone};
use crate::error::Misuse;
use crate::turn::{SharedTurn, lock};

/// `clock`: the time of day.
#[derive(Debug, Clone)]
pub(super) struct Clock(pub(super) SharedTurn);

/// The time of day as a script reads it: plain data.
fn local(time: LocalTime) -> Map {
    let mut map = Map::new();
    map.insert("year".into(), int(time.year));
    map.insert("month".into(), int(time.month));
    map.insert("day".into(), int(time.day));
    map.insert("hour".into(), int(time.hour));
    map.insert("minute".into(), int(time.minute));
    map.insert("second".into(), int(time.second));
    map.insert("weekday".into(), int(time.weekday));
    map.insert("offset".into(), int(time.offset_minutes));
    map
}

/// `clock.local(zone)`.
pub(super) fn register(m: &mut Module, r: &mut Registered) {
    m.set_custom_type::<Clock>("clock");
    method!(m, r, "clock", "local", |clock: &mut Clock,
                                     name: Dynamic|
     -> Returns<Map> {
        // A name no zone has is a mistake in the program, as a message the
        // mode doesn't declare is: no value at run time excuses it.
        let found = text(&name)
            .and_then(|name| zone(&name))
            .ok_or_else(|| Box::new(misuse(Misuse::UnknownTimeZone)))?;
        let now = lock(&clock.0).now();
        // Every moment of a hook is inside what the database covers.
        Ok(local_time(&found, now).map(local).unwrap_or_default())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_of_day_is_a_map_of_plain_numbers() {
        let map = local(LocalTime {
            year: 2026,
            month: 11,
            day: 26,
            hour: 14,
            minute: 5,
            second: 9,
            weekday: 4,
            offset_minutes: 60,
        });

        let read = |key: &str| map.get(key).unwrap().as_int().unwrap();
        assert_eq!(
            [
                "year", "month", "day", "hour", "minute", "second", "weekday", "offset"
            ]
            .map(read),
            [2026, 11, 26, 14, 5, 9, 4, 60]
        );
        assert_eq!(map.len(), 8);
        let _: Dynamic = Dynamic::from_map(map);
    }
}
