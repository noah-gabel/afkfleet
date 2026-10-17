//! `[chat]`: what users may make the bots say (Plan.md P11.2, P11.3,
//! Appendix A, ADR-0010).
//!
//! It holds the command allowlist, which a mode switch reads from P11.2 on
//! (a mode that sends a command off it needs Manage) and P11.3's chat after
//! it. P11.6's `retention_days` joins it with its first user.

use fleet_core::authz::CommandAllowlist;

/// `[chat]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatConfig {
    /// The `/commands` that Control holders may send, in chat or through a
    /// mode (`command_allowlist`, e.g. `["/spawn", "/home"]`): `/` and a
    /// command name each, at most 64. Missing means empty: every command
    /// needs Manage.
    pub command_allowlist: CommandAllowlist,
}
