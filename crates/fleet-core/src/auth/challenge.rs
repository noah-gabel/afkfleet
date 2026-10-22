//! The login's challenges (Plan.md P7.6, P7.7, §7.2, ADR-0016): how long the
//! mfa token and the setup token live, and how many wrong codes an mfa token
//! takes.
//!
//! The password step hands out one of them, never real tokens: an mfa token
//! when the user has 2FA, a setup token when they must enroll first. A 2FA
//! replacement hands out a setup token too. The server holds them in memory;
//! these rules decide when one is over. A live events ticket (P11.5;
//! ADR-0020, S29) is held the same way, and lives [`EVENT_TICKET_TTL`].

use core::time::Duration;

use chrono::{DateTime, Utc};

use crate::time::add;

/// How long an mfa token lives (§7.2).
pub const MFA_TOKEN_TTL: Duration = Duration::from_mins(5);

/// How long a setup token lives (§7.2).
pub const SETUP_TOKEN_TTL: Duration = Duration::from_mins(10);

/// How long a live events ticket lives (§7.4: single use, 30 s): from
/// `POST /events/ticket` to the socket it opens.
pub const EVENT_TICKET_TTL: Duration = Duration::from_secs(30);

/// How many wrong codes one mfa token takes: the last one uses it up, and
/// the user goes back to the password step.
pub const MFA_ATTEMPTS: u32 = 3;

/// Whether a challenge issued at `issued_at` that lives `ttl` is over at
/// `now`: expired at `now >= issued_at + ttl`, like every other lifetime.
#[must_use]
pub fn challenge_expired(issued_at: DateTime<Utc>, ttl: Duration, now: DateTime<Utc>) -> bool {
    now >= add(issued_at, ttl)
}

/// How long a challenge issued at `issued_at` that lives `ttl` still has at
/// `now`, in whole seconds rounded down: what the response's `expires_in`
/// says. Zero once it's over, and never more than `ttl` (a clock that went
/// back).
#[must_use]
pub fn expires_in(issued_at: DateTime<Utc>, ttl: Duration, now: DateTime<Utc>) -> u64 {
    add(issued_at, ttl)
        .signed_duration_since(now)
        .to_std()
        .map_or(0, |left| left.as_secs())
        .min(ttl.as_secs())
}

/// Whether an mfa token that has taken `wrong_codes` wrong codes is used up.
#[must_use]
pub const fn used_up(wrong_codes: u32) -> bool {
    wrong_codes >= MFA_ATTEMPTS
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use rstest::rstest;

    use super::*;

    const T0: i64 = 1_800_000_000_000;

    fn after(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(T0 + millis).unwrap()
    }

    #[rstest]
    #[case::at_issue(0, false)]
    #[case::one_millisecond_before(299_999, false)]
    #[case::exactly_at_its_end(300_000, true)]
    #[case::after(300_001, true)]
    #[case::clock_went_back(-60_000, false)]
    fn an_mfa_token_lives_five_minutes(#[case] millis: i64, #[case] expired: bool) {
        assert_eq!(
            challenge_expired(after(0), MFA_TOKEN_TTL, after(millis)),
            expired
        );
    }

    #[rstest]
    #[case::one_millisecond_before(599_999, false)]
    #[case::exactly_at_its_end(600_000, true)]
    fn a_setup_token_lives_ten_minutes(#[case] millis: i64, #[case] expired: bool) {
        assert_eq!(
            challenge_expired(after(0), SETUP_TOKEN_TTL, after(millis)),
            expired
        );
    }

    #[rstest]
    #[case::one_millisecond_before(29_999, false)]
    #[case::exactly_at_its_end(30_000, true)]
    fn an_event_ticket_lives_30_s(#[case] millis: i64, #[case] expired: bool) {
        assert_eq!(
            challenge_expired(after(0), EVENT_TICKET_TTL, after(millis)),
            expired
        );
    }

    #[rstest]
    #[case::at_issue(0, 300)]
    #[case::rounded_down(1, 299)]
    #[case::last_second(299_000, 1)]
    #[case::under_a_second(299_500, 0)]
    #[case::at_its_end(300_000, 0)]
    #[case::long_after(900_000, 0)]
    fn what_is_left_is_whole_seconds_rounded_down(#[case] millis: i64, #[case] left: u64) {
        assert_eq!(expires_in(after(0), MFA_TOKEN_TTL, after(millis)), left);
    }

    #[test]
    fn a_clock_that_went_back_shows_no_more_than_the_lifetime() {
        let left = expires_in(after(0), MFA_TOKEN_TTL, after(0) - TimeDelta::seconds(10));

        assert_eq!(left, 300);
    }

    #[rstest]
    #[case::none(0, false)]
    #[case::two(2, false)]
    #[case::three(3, true)]
    #[case::more(9, true)]
    fn the_third_wrong_code_uses_an_mfa_token_up(#[case] wrong: u32, #[case] up: bool) {
        assert_eq!(used_up(wrong), up);
    }

    #[test]
    fn the_lifetimes_are_pinned() {
        assert_eq!(MFA_TOKEN_TTL, Duration::from_secs(300));
        assert_eq!(SETUP_TOKEN_TTL, Duration::from_secs(600));
        assert_eq!(EVENT_TICKET_TTL, Duration::from_secs(30));
        assert_eq!(MFA_ATTEMPTS, 3);
    }
}
