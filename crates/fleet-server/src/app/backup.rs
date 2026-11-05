//! [`BackupService`]: the server's backups (Plan.md P12.3; ADR-0022, S2–S6,
//! R1, R2), one service for `serve`'s schedule and the `backup` command.
//!
//! - **Off unless configured:** [`BackupService::from_config`] is `None`
//!   without `[database] backup_dir`.
//! - **The folder** ([`BackupService::check`]): it must exist, be a folder
//!   and not a link, and on disk neither hold the database nor any key file
//!   the config names. It's never created.
//! - **Due** ([`BackupService::is_due`]): from the finished backups' names,
//!   never their files' times, so a restart neither skips nor doubles one. A
//!   name dated more than [`FUTURE_SLACK`] ahead doesn't count, with one
//!   `warn` the first time.
//! - **A backup** ([`BackupService::take`]): named for the clock's second,
//!   written by [`backup::write()`], then pruned to `backup_keep` finished
//!   backups, never the new one, with leftover temporary files a day old. A
//!   failed prune leaves the new backup and is tried again with the next.
//!
//! A backup writes no audit entry: `VACUUM INTO` writes nothing to the
//! database (S6). Its `info` line holds the file's name, its size, how long
//! it took and how many went; never anything from a row.

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, TimeDelta};
use fleet_core::backup::{BackupName, BackupPolicy, FUTURE_SLACK, is_stale_partial};
use fleet_core::system::Clock;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::{BackupConfig, ServerConfig};
use crate::http::error::chain;
use crate::infra::sqlite::DatabaseOptions;
use crate::infra::sqlite::backup::{self, BackupError, Listing, Written};

/// The longest a backup may take before it's stopped: far beyond the
/// largest database the runbook sizes for.
pub const BACKUP_BOUND: Duration = Duration::from_hours(1);

/// How long `serve` waits after a failed backup before it tries again: a
/// try rewrites the whole database and holds a read snapshot, so never every
/// minute (S3).
pub const RETRY_AFTER: TimeDelta = TimeDelta::hours(1);

/// A backup taken, and what its pruning did.
#[derive(Debug)]
pub struct Taken {
    /// The new backup.
    pub written: Written,
    /// How many finished backups pruning removed.
    pub pruned: usize,
    /// Why pruning failed, when it did: the new backup stays, and the next
    /// backup prunes again.
    pub prune_failed: Option<BackupError>,
}

/// The server's backups; see the module docs.
pub struct BackupService {
    clock: Arc<dyn Clock>,
    database: PathBuf,
    config: BackupConfig,
    key_files: Vec<PathBuf>,
    options: DatabaseOptions,
    warned_ahead: AtomicBool,
}

impl core::fmt::Debug for BackupService {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BackupService")
            .field("database", &self.database)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl BackupService {
    /// The backups of `database` into `config`'s folder, with `key_files`
    /// kept out of it.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        database: PathBuf,
        config: BackupConfig,
        key_files: Vec<PathBuf>,
        options: DatabaseOptions,
    ) -> Self {
        Self {
            clock,
            database,
            config,
            key_files,
            options,
            warned_ahead: AtomicBool::new(false),
        }
    }

    /// The backups `config` asks for, or `None` without `backup_dir`. The
    /// key files kept out of the folder are the vault's keys and, with
    /// `[grpc]`, the CA's and the server's keys.
    #[must_use]
    pub fn from_config(
        config: &ServerConfig,
        clock: Arc<dyn Clock>,
        options: DatabaseOptions,
    ) -> Option<Self> {
        let backups = config.database.backups.clone()?;
        let mut key_files: Vec<PathBuf> = config.vault.key_files.values().cloned().collect();
        if let Some(grpc) = &config.grpc {
            key_files.push(grpc.pki.ca_key_file.clone());
            key_files.push(grpc.pki.server_key_file.clone());
        }
        Some(Self::new(
            clock,
            config.database.path.clone(),
            backups,
            key_files,
            options,
        ))
    }

    /// The folder the backups go into.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.config.dir
    }

    /// How often and how many.
    #[must_use]
    pub const fn policy(&self) -> BackupPolicy {
        self.config.policy
    }

    /// Checks the folder on disk; see the module docs.
    ///
    /// # Errors
    /// [`backup::check_folder`]'s and [`backup::check_placement`]'s.
    pub async fn check(&self) -> Result<(), BackupError> {
        backup::check_folder(&self.config.dir).await?;
        backup::check_placement(&self.config.dir, &self.database, &self.key_files).await
    }

    /// Whether a backup is due now; see the module docs.
    ///
    /// # Errors
    /// [`BackupError::Io`] when the folder can't be read.
    pub async fn is_due(&self) -> Result<bool, BackupError> {
        let now = self.clock.now();
        let listing = backup::list(&self.config.dir).await?;
        let latest = now.checked_add_signed(FUTURE_SLACK).unwrap_or(now);
        if let Some(ahead) = listing.finished.iter().find(|name| name.stamp() > latest)
            && !self.warned_ahead.swap(true, Ordering::Relaxed)
        {
            warn!(
                file = %ahead.file_name(),
                "a backup is dated ahead of the clock; it doesn't count toward the next one"
            );
        }
        Ok(self.config.policy.is_due(&listing.finished, now))
    }

    /// Takes one backup now and prunes after it; see the module docs.
    /// `stop` or [`BACKUP_BOUND`] end it.
    ///
    /// # Errors
    /// [`BackupError::Unnamed`] when the clock can't name it, or
    /// [`backup::write()`]'s errors; nothing is pruned then.
    pub async fn take(&self, stop: &CancellationToken) -> Result<Taken, BackupError> {
        let now = self.clock.now();
        let name = BackupName::at(now).ok_or(BackupError::Unnamed)?;
        let started = Instant::now();
        let written = backup::write(
            &self.database,
            &self.config.dir,
            name,
            stop,
            BACKUP_BOUND,
            &self.options,
        )
        .await?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (pruned, prune_failed) = match self.prune(name, now).await {
            Ok(pruned) => (pruned, None),
            Err(error) => (0, Some(error)),
        };
        info!(
            file = %name.file_name(),
            bytes = written.bytes,
            duration_ms,
            pruned,
            "a backup was written"
        );
        if let Some(error) = &prune_failed {
            warn!(
                error = %chain(error),
                "a backup's pruning failed; the next backup prunes again"
            );
        }
        Ok(Taken {
            written,
            pruned,
            prune_failed,
        })
    }

    /// Removes every finished backup past `backup_keep` but `written`, and
    /// the temporary files a day old; how many finished ones went.
    async fn prune(
        &self,
        written: BackupName,
        now: DateTime<chrono::Utc>,
    ) -> Result<usize, BackupError> {
        let listing = backup::list(&self.config.dir).await?;
        let gone = Listing {
            finished: self.config.policy.to_prune(&listing.finished, written),
            partial: stale(listing.partial, written, now),
            partial_journals: stale(listing.partial_journals, written, now),
        };
        backup::remove(&self.config.dir, &gone).await?;
        Ok(gone.finished.len())
    }
}

/// The leftover temporary files of `names` pruning removes at `now`: those
/// a day old, never `written`'s.
fn stale(
    names: Vec<BackupName>,
    written: BackupName,
    now: DateTime<chrono::Utc>,
) -> Vec<BackupName> {
    names
        .into_iter()
        .filter(|name| *name != written && is_stale_partial(*name, now))
        .collect()
}
