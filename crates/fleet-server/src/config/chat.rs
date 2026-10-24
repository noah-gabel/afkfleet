//! `[chat]`: what users may make the bots say, and how long it's kept
//! (Plan.md P11.2, P11.3, P11.6, Appendix A, ADR-0010, ADR-0020).
//!
//! It holds the command allowlist, which a mode switch reads from P11.2 on
//! (a mode that sends a command off it needs Manage) and P11.3's chat after
//! it, and P11.6's retention: chat's age and each bot's bound (U19, U21).

use fleet_core::authz::CommandAllowlist;
use fleet_core::retention::{ChatBound, ChatRetention};

use super::Bounds;

/// `[chat]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatConfig {
    /// The `/commands` that Control holders may send, in chat or through a
    /// mode (`command_allowlist`, e.g. `["/spawn", "/home"]`): `/` and a
    /// command name each, at most 64. Missing means empty: every command
    /// needs Manage.
    pub command_allowlist: CommandAllowlist,
    /// How long chat is kept (`retention_days`): 1 to 30 days, 14 by
    /// default.
    pub retention: ChatRetention,
    /// How many lines each bot keeps at most, its newest
    /// (`max_lines_per_bot`): 100 to 200,000, 50,000 by default.
    pub bound: ChatBound,
}

impl ChatConfig {
    /// `retention_days`: 1 to 30 days (U21).
    pub const RETENTION_DAYS: Bounds = Bounds {
        min: ChatRetention::MIN_DAYS,
        max: ChatRetention::MAX_DAYS,
    };
    /// `max_lines_per_bot`: 100 to 200,000 lines (U19).
    pub const MAX_LINES_PER_BOT: Bounds = Bounds {
        min: ChatBound::MIN_LINES,
        max: ChatBound::MAX_LINES,
    };
}
