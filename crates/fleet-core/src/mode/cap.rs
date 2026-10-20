//! How many modes one user may own (ADR-0020, S19).
//!
//! An owner holds at most [`MAX_MODES_PER_OWNER`] modes, private and shared
//! together, the Owner included; the built-in modes have no owner and never
//! count. The server counts in the create's write transaction, so two
//! creates can't both take the last place.

/// The most modes one user may own.
pub const MAX_MODES_PER_OWNER: u32 = 50;

/// Whether an owner who already owns `owned` modes is at
/// [`MAX_MODES_PER_OWNER`], so a new one is refused.
#[must_use]
pub const fn mode_cap_reached(owned: u32) -> bool {
    owned >= MAX_MODES_PER_OWNER
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::none(0, false)]
    #[case::one_place_left(49, false)]
    #[case::full(50, true)]
    #[case::past_it(51, true)]
    fn the_fifty_first_mode_is_refused(#[case] owned: u32, #[case] reached: bool) {
        assert_eq!(mode_cap_reached(owned), reached);
    }

    #[test]
    fn an_owner_holds_at_most_fifty_modes() {
        assert_eq!(MAX_MODES_PER_OWNER, 50);
    }
}
