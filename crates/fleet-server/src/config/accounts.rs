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
    /// Whether offline accounts may be created (`allow_offline`), for
    /// offline-mode servers in development (P9.7). Off by default; turning
    /// it off again keeps the existing ones untouched.
    pub allow_offline: bool,
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
            allow_offline: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_appendix_as() {
        assert_eq!(AccountsConfig::default().max_per_member.get(), 3);
        assert!(!AccountsConfig::default().allow_offline);
    }
}
