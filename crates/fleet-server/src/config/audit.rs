//! `[audit]`: how long the audit log is kept (Plan.md P11.6, Appendix A;
//! ADR-0020, U22).
//!
//! Every entry holds the acting user's IP address, so the log is kept a year
//! by default and two at most. The longest window any limit reads from the
//! log is 24 h; the count of wrong 2FA codes since a user's last login (on
//! `/me`) sees only the entries still kept.

use fleet_core::retention::AuditRetention;

use super::Bounds;

/// `[audit]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditConfig {
    /// How long the audit log is kept (`retention_days`): 90 to 730 days,
    /// 365 by default. Unused invites and enrollment tokens are kept that
    /// long past their expiry too (S46).
    pub retention: AuditRetention,
}

impl AuditConfig {
    /// `retention_days`: 90 to 730 days (U22).
    pub const RETENTION_DAYS: Bounds = Bounds {
        min: AuditRetention::MIN_DAYS,
        max: AuditRetention::MAX_DAYS,
    };
}
