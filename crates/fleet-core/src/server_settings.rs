//! Per-server settings (ADR-0020, S74–S78): what every bot on one Minecraft
//! server shares, whoever's bot it is, keyed by the server's
//! [`ServerKey`](crate::value::ServerKey).
//!
//! The fleet holds settings for at most [`MAX_SERVER_SETTINGS`] servers. The
//! server counts in the create's write transaction, so two creates can't
//! both take the last place, and the reconciler, which reads every row each
//! pass, reads a bounded set.

/// The most servers with settings.
pub const MAX_SERVER_SETTINGS: u32 = 100;

/// Whether `held` servers' settings already fill [`MAX_SERVER_SETTINGS`], so
/// one more is refused.
#[must_use]
pub const fn server_settings_cap_reached(held: u32) -> bool {
    held >= MAX_SERVER_SETTINGS
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::none(0, false)]
    #[case::one_place_left(99, false)]
    #[case::full(100, true)]
    #[case::past_it(101, true)]
    fn the_hundred_and_first_server_is_refused(#[case] held: u32, #[case] reached: bool) {
        assert_eq!(server_settings_cap_reached(held), reached);
    }

    #[test]
    fn the_fleet_holds_settings_for_at_most_a_hundred_servers() {
        assert_eq!(MAX_SERVER_SETTINGS, 100);
    }
}
