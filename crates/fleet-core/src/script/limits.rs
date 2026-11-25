//! The limits of a script, as constants (ADR-0036, D3).
//!
//! One place for the numbers, so the engine that enforces them
//! (`fleet-script`), the server that validates with them and the reference
//! that prints them can't drift apart. Limits that belong to one type are
//! that type's own constants: [`ScriptSource::MAX_BYTES`], the leash's and
//! the tool guard's ranges, how many messages and patterns a mode declares.
//!
//! The two operation budgets and the two time budgets are proposals until
//! Phase 15's load test confirms or changes them (P15.19).
//!
//! [`ScriptSource::MAX_BYTES`]: super::ScriptSource::MAX_BYTES

use core::time::Duration;

/// The most operations one hook runs: a loop that never ends stops here.
pub const OPERATIONS_PER_HOOK: u64 = 10_000;
/// The most operations a script runs in any one second, over all its hooks.
pub const OPERATIONS_PER_SECOND: u64 = 30_000;
/// The longest one hook runs, as its [`Stopwatch`] counts it.
///
/// Operations alone don't bound a hook's time: one that calls a function
/// of the script's through an array's method costs a hundred times a plain
/// one, and Rhai builds a text or an array past its limit before it
/// refuses it (measured by the user's reviewer: 41 to 120 ms for one
/// hook's operations; ADR-0040's amendment). A hook of plain operations
/// that runs to [`OPERATIONS_PER_HOOK`] takes about a tenth of this.
///
/// [`Stopwatch`]: super::Stopwatch
pub const HOOK_TIME: Duration = Duration::from_millis(5);
/// The longest a script's hooks run in any one second, together: 2 % of
/// one core.
pub const TIME_PER_SECOND: Duration = Duration::from_millis(20);
/// After how many operations a running hook reads its stopwatch again.
pub const TIME_CHECK_EVERY_OPERATIONS: u64 = 8;
/// The most that the time between two readings of the stopwatch counts
/// for. No step of a script takes this long, so a longer gap is the
/// host's: the thread was set aside, or the whole process was. A hook
/// pays one such gap in full and no more for each, so a busy host doesn't
/// stop a script that did nothing wrong.
pub const LONGEST_STRETCH: Duration = Duration::from_millis(1);
/// How deep a script's own functions call each other.
pub const MAX_CALL_DEPTH: usize = 16;
/// How deeply an expression nests at the top level.
pub const MAX_EXPRESSION_DEPTH: usize = 32;
/// How deeply an expression nests inside a function: Rhai's own default
/// for a release build (the user's answer to ADR-0040). What it lets a
/// script take of its thread's stack is bounded by measuring, not by this.
pub const MAX_FUNCTION_EXPRESSION_DEPTH: usize = 32;
/// The most functions a script defines.
pub const MAX_FUNCTIONS: usize = 64;
/// The longest text a script holds, in bytes of UTF-8: Rhai counts a text's
/// bytes, not its characters, so a text of letters outside ASCII is shorter
/// than this in characters.
pub const MAX_STRING_BYTES: usize = 1024;
/// The most items an array holds.
pub const MAX_ARRAY_ITEMS: usize = 256;
/// The most entries a map holds.
pub const MAX_MAP_ENTRIES: usize = 64;

/// The most entries the state object holds, nested ones counted.
pub const STATE_MAX_ENTRIES: usize = 64;
/// The most bytes the state object holds: its keys, texts and numbers.
pub const STATE_MAX_BYTES: usize = 4096;
/// After how many operations the state's size is checked again while a
/// hook runs (ADR-0036, D5). An assignment copies its value, so a hook
/// could double the state again and again; checked only after the hook,
/// 10,000 operations could fill an agent's memory. Between two checks a
/// script adds at most what this many operations add, each bounded by
/// [`MAX_STRING_BYTES`], [`MAX_ARRAY_ITEMS`] and [`MAX_MAP_ENTRIES`].
pub const STATE_CHECK_EVERY_OPERATIONS: u64 = 256;

/// The most timers a script holds at once.
pub const MAX_TIMERS: usize = 16;
/// The shortest timer: today's shortest interval of a step (ADR-0010).
pub const MIN_TIMER: Duration = Duration::from_millis(250);
/// The longest timer: 24 hours, today's longest interval of a step. The
/// user's balance script needs 10 hours (ADR-0037).
pub const MAX_TIMER: Duration = Duration::from_hours(24);
/// The shortest `hold_for`: one game tick.
pub const MIN_HOLD_FOR: Duration = Duration::from_millis(50);
/// The longest `hold_for`: 10 minutes.
pub const MAX_HOLD_FOR: Duration = Duration::from_mins(10);

/// The shortest time between two of a bot's clicks, or anything else a
/// screen answers: one game tick, no burst (ADR-0036, R2). At most 20 clicks
/// a second, 40 packets with each click's barrier, a sixth of what Paper's
/// packet limiter takes; vanilla has no limit.
pub const CLICK_INTERVAL: Duration = Duration::from_millis(50);

/// The longest line a script writes into its log, in characters.
pub const LOG_LINE_CHARS: usize = 200;
/// The most lines a script writes into its log in a minute.
pub const LOG_LINES_PER_MINUTE: u32 = 20;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::ModeDefinition;

    /// The numbers of ADR-0036's table and of the reference
    /// (docs/scripting.md, "Limits"): a change here changes both.
    #[test]
    fn the_limits_are_the_documented_ones() {
        assert_eq!(
            (OPERATIONS_PER_HOOK, OPERATIONS_PER_SECOND),
            (10_000, 30_000)
        );
        assert_eq!(
            (
                MAX_CALL_DEPTH,
                MAX_EXPRESSION_DEPTH,
                MAX_FUNCTION_EXPRESSION_DEPTH
            ),
            (16, 32, 32)
        );
        assert_eq!(
            (HOOK_TIME, TIME_PER_SECOND),
            (Duration::from_millis(5), Duration::from_millis(20))
        );
        assert_eq!(MAX_FUNCTIONS, 64);
        assert_eq!(
            (MAX_STRING_BYTES, MAX_ARRAY_ITEMS, MAX_MAP_ENTRIES),
            (1024, 256, 64)
        );
        assert_eq!((STATE_MAX_ENTRIES, STATE_MAX_BYTES), (64, 4096));
        assert_eq!(MAX_TIMERS, 16);
        assert_eq!((LOG_LINE_CHARS, LOG_LINES_PER_MINUTE), (200, 20));
        assert_eq!(MIN_HOLD_FOR, Duration::from_millis(50));
        assert_eq!(MAX_HOLD_FOR, Duration::from_millis(600_000));
    }

    #[test]
    fn a_timer_runs_as_often_and_as_rarely_as_a_step() {
        assert_eq!(MIN_TIMER, ModeDefinition::MIN_INTERVAL);
        assert_eq!(MAX_TIMER, ModeDefinition::MAX_INTERVAL);
    }

    #[test]
    fn a_timer_of_ten_hours_is_allowed() {
        assert!(Duration::from_hours(10) <= MAX_TIMER);
    }

    /// The state is checked many times within one hook's budget, never
    /// only after it.
    #[test]
    fn the_state_is_checked_many_times_within_one_hook() {
        assert_eq!(STATE_CHECK_EVERY_OPERATIONS, 256);
        assert_eq!(OPERATIONS_PER_HOOK / STATE_CHECK_EVERY_OPERATIONS, 39);
    }

    /// A hook's time is read often, a gap counts for far less than a
    /// hook's time, and a second holds several hooks of the longest kind.
    #[test]
    fn the_time_budgets_fit_each_other() {
        assert_eq!(TIME_CHECK_EVERY_OPERATIONS, 8);
        assert_eq!(HOOK_TIME.as_micros() / LONGEST_STRETCH.as_micros(), 5);
        assert_eq!(TIME_PER_SECOND.as_micros() / HOOK_TIME.as_micros(), 4);
    }
}
