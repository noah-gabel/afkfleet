//! `[database]`'s backup keys (Plan.md P12.3, Appendix A; ADR-0022, S4):
//! where the server writes its backups, how often, and how many it keeps.
//!
//! Backups are off unless `backup_dir` is set. The folder is checked when the
//! config loads (absolute, no `..`, neither the database's folder nor one
//! above it, holding none of the server's key files) and again on disk when
//! `serve` or `backup` starts: it must exist and be a folder, never a link,
//! and it's never created, so a missing volume stops the server instead of
//! filling its throwaway layer.

use std::path::PathBuf;

use fleet_core::backup::BackupPolicy;

use super::Bounds;

/// `[database]`'s backups, when `backup_dir` is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupConfig {
    /// The folder the backups go into (`backup_dir`): absolute.
    pub dir: PathBuf,
    /// How often and how many (`backup_interval_hours`, `backup_keep`).
    pub policy: BackupPolicy,
}

impl BackupConfig {
    /// `backup_interval_hours`: 1 to 168 hours, 24 by default.
    pub const INTERVAL_HOURS: Bounds = Bounds {
        min: BackupPolicy::MIN_INTERVAL_HOURS,
        max: BackupPolicy::MAX_INTERVAL_HOURS,
    };
    /// `backup_keep`: 1 to 60 backups, 14 by default.
    pub const KEEP: Bounds = Bounds {
        min: BackupPolicy::MIN_KEEP,
        max: BackupPolicy::MAX_KEEP,
    };
}
