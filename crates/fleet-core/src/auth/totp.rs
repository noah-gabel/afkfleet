//! The TOTP rules (Plan.md P7.7, §7.2, ADR-0016, RFC 6238): which time
//! steps a code may be from ([`candidate_steps`]), and what a typed code
//! must look like ([`TotpCode`]).
//!
//! The parameters are what every authenticator app supports: SHA-1, 6
//! digits, 30-second steps, and one step of drift on each side. A code is
//! accepted only for a step strictly after the last one accepted, so it
//! can't be used twice (RFC 6238 section 5.2), and neither can any older
//! code in the window. The server computes a step's code (its crypto);
//! this module decides which steps to try.

use core::fmt;
use core::time::Duration;

use chrono::{DateTime, Utc};

/// The length of a time step (RFC 6238's default).
pub const TOTP_STEP: Duration = Duration::from_secs(30);

/// How many steps before and after the current one a code may come from,
/// for a phone clock that drifts or a code typed at the end of its step.
pub const TOTP_SKEW: u64 = 1;

/// The digits of a code.
pub const TOTP_DIGITS: usize = 6;

/// The secret's length in bytes: 160 bits, RFC 4226's recommendation and
/// SHA-1's block of output.
pub const TOTP_SECRET_LEN: usize = 20;

/// The issuer an authenticator app shows next to the account.
pub const TOTP_ISSUER: &str = "afkfleet";

/// A code as typed: exactly [`TOTP_DIGITS`] ASCII digits. Its `Debug` shows
/// none of them.
#[derive(Clone, PartialEq, Eq)]
pub struct TotpCode([u8; TOTP_DIGITS]);

impl TotpCode {
    /// The code, if `text` is exactly [`TOTP_DIGITS`] ASCII digits.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let digits: [u8; TOTP_DIGITS] = text.as_bytes().try_into().ok()?;
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then_some(Self(digits))
    }

    /// The digits, for comparing with a step's code.
    #[must_use]
    pub const fn digits(&self) -> &[u8; TOTP_DIGITS] {
        &self.0
    }
}

/// Shows none of the digits.
impl fmt::Debug for TotpCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TotpCode(..)")
    }
}

/// The step `now` falls in: whole [`TOTP_STEP`]s since the Unix epoch.
/// `None` before the epoch, where no step exists.
#[must_use]
pub fn current_step(now: DateTime<Utc>) -> Option<u64> {
    let seconds = u64::try_from(now.timestamp()).ok()?;
    seconds.checked_div(TOTP_STEP.as_secs())
}

/// The Unix time, in seconds, at which `step` begins: the time a step's
/// code is computed for. `None` beyond `u64`.
#[must_use]
pub fn step_start(step: u64) -> Option<u64> {
    step.checked_mul(TOTP_STEP.as_secs())
}

/// The steps a code may match at `now`, oldest first: the current step and
/// [`TOTP_SKEW`] on each side, but only those strictly after `last_used`,
/// the last step accepted (none before a first enrollment's confirm). Empty
/// before the Unix epoch.
#[must_use]
pub fn candidate_steps(now: DateTime<Utc>, last_used: Option<u64>) -> Vec<u64> {
    let Some(current) = current_step(now) else {
        return Vec::new();
    };
    (current.saturating_sub(TOTP_SKEW)..=current.saturating_add(TOTP_SKEW))
        .filter(|step| last_used.is_none_or(|last| *step > last))
        .collect()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[rstest]
    #[case::digits("123456")]
    #[case::zeros("000000")]
    fn six_digits_are_a_code(#[case] text: &str) {
        assert_eq!(TotpCode::parse(text).unwrap().digits(), text.as_bytes());
    }

    #[rstest]
    #[case::five("12345")]
    #[case::seven("1234567")]
    #[case::empty("")]
    #[case::letter("12345a")]
    #[case::space("123 456")]
    #[case::full_width_digit("12345\u{FF16}")]
    #[case::sign("+12345")]
    fn anything_else_isnt(#[case] text: &str) {
        assert_eq!(TotpCode::parse(text), None, "{text}");
    }

    #[test]
    fn debug_shows_no_digit() {
        let debug = format!("{:?}", TotpCode::parse("987654").unwrap());

        assert_eq!(debug, "TotpCode(..)");
    }

    #[rstest]
    #[case::epoch(0, 0)]
    #[case::last_second_of_step_0(29, 0)]
    #[case::step_1(30, 1)]
    #[case::rfc_6238_vector(59, 1)]
    #[case::rfc_6238_vector_2(1_111_111_109, 37_037_036)]
    #[case::rfc_6238_vector_3(2_000_000_000, 66_666_666)]
    fn the_current_step_counts_whole_steps_since_the_epoch(#[case] secs: i64, #[case] step: u64) {
        assert_eq!(current_step(at(secs)), Some(step));
    }

    #[test]
    fn there_is_no_step_before_the_epoch() {
        assert_eq!(current_step(at(-1)), None);
        assert_eq!(candidate_steps(at(-1), None), Vec::<u64>::new());
    }

    #[test]
    fn a_steps_start_is_its_number_of_steps_in_seconds() {
        assert_eq!(step_start(0), Some(0));
        assert_eq!(step_start(37_037_036), Some(1_111_111_080));
        assert_eq!(step_start(u64::MAX), None);
    }

    #[test]
    fn without_a_last_step_the_window_is_one_step_each_side() {
        assert_eq!(candidate_steps(at(3_000), None), [99, 100, 101]);
    }

    #[rstest]
    #[case::long_ago(10, &[99, 100, 101])]
    #[case::the_step_before_the_window(98, &[99, 100, 101])]
    #[case::the_previous_step(99, &[100, 101])]
    #[case::the_current_step(100, &[101])]
    #[case::the_next_step(101, &[])]
    #[case::the_future(500, &[])]
    fn only_steps_after_the_last_accepted_one_count(#[case] last: u64, #[case] steps: &[u64]) {
        assert_eq!(candidate_steps(at(3_000), Some(last)), steps);
    }

    #[test]
    fn at_step_0_the_window_doesnt_reach_before_the_epoch() {
        assert_eq!(candidate_steps(at(0), None), [0, 1]);
    }

    #[test]
    fn the_parameters_are_pinned() {
        assert_eq!(TOTP_STEP, Duration::from_secs(30));
        assert_eq!(TOTP_SKEW, 1);
        assert_eq!(TOTP_DIGITS, 6);
        assert_eq!(TOTP_SECRET_LEN, 20);
        assert_eq!(TOTP_ISSUER, "afkfleet");
    }
}
