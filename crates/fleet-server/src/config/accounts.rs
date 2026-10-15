//! `[accounts]`: the linked Minecraft accounts (Plan.md P9.4, P9.7,
//! Appendix A, ADR-0018).

use core::num::NonZeroU32;

use super::Bounds;
use super::rate_limits::nonzero;

/// `[accounts]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountsConfig {
    /// How many accounts a Member may own, of any kind and status
    /// (`max_per_member`); Admins and the Owner have no quota.
    pub max_per_member: NonZeroU32,
}

impl AccountsConfig {
    /// `max_per_member`: 1 to 100. Default 3.
    pub const MAX_PER_MEMBER: Bounds = Bounds { min: 1, max: 100 };
}

impl Default for AccountsConfig {
    /// Appendix A's: 3 accounts per Member.
    fn default() -> Self {
        Self {
            max_per_member: nonzero(3),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_appendix_as() {
        assert_eq!(AccountsConfig::default().max_per_member.get(), 3);
    }
}
