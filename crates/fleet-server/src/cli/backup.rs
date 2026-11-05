//! `afkfleet-server backup` (Plan.md P12.3; ADR-0022, S5): one backup now,
//! from the server's host, beside a running `serve` or with it stopped.
//!
//! - **Into `[database] backup_dir`** only, named and pruned like `serve`'s
//!   scheduled backups, through the same [`BackupService`]. Without
//!   `backup_dir` it's refused before anything is read.
//! - **The folder** is checked as `serve` checks it at its start.
//! - **The database** must exist with every migration applied: it's opened
//!   with [`sqlite::open_migrated`], never created or migrated, so a
//!   forgotten volume can't become an empty backup. The vault's keys are
//!   never read: the backup holds their secrets sealed.
//! - **Its output:** stdout carries the new file's absolute path alone, for a
//!   script that copies it; stderr one line with its size and how many older
//!   backups went, and the logs.

use std::io::Write;
use std::sync::Arc;

use fleet_core::system::Clock;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::app::backup::BackupService;
use crate::config::ServerConfig;
use crate::http::error::chain;
use crate::infra::sqlite::{self, DatabaseOptions};

/// The exit codes of `afkfleet-server backup`, shown in its help.
pub const EXIT_CODES: &str = "\
Exit codes:
  0  the backup is written: its path is on stdout
  1  nothing was written: [database] backup_dir isn't set or its folder can't
     be used, or the config, logging, the database (a missing one and pending
     migrations included) or the copy failed
  2  usage error
  3  the backup is written and its path is on stdout, but removing older
     backups failed";

/// How `afkfleet-server backup` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The backup is written, and older ones pruned.
    Written,
    /// The backup is written, but pruning failed.
    Unpruned,
    /// Nothing was written; stderr (and the log) says why.
    Failed,
}

impl Exit {
    /// The process's exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Written => 0,
            Self::Failed => 1,
            Self::Unpruned => 3,
        }
    }
}

/// Takes one backup into `config`'s `backup_dir`; see the
/// [module docs](self). The path goes to `out`, everything else to `err`.
pub async fn run(
    config: &ServerConfig,
    clock: Arc<dyn Clock>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let Some(backups) = BackupService::from_config(config, clock, DatabaseOptions::default())
    else {
        let what = "backups are off: [database] backup_dir isn't set";
        error!("{what}");
        let _ = writeln!(err, "afkfleet-server: {what}");
        return Exit::Failed;
    };
    if let Err(error) = backups.check().await {
        return fail(err, "the backups' folder can't be used", &error);
    }
    match sqlite::open_migrated(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database.close().await,
        Err(error) => return fail(err, "the database can't be backed up", &error),
    }
    let taken = match backups.take(&CancellationToken::new()).await {
        Ok(taken) => taken,
        Err(error) => return fail(err, "the backup failed", &error),
    };
    let _ = writeln!(out, "{}", taken.written.file.display());
    let _ = writeln!(
        err,
        "afkfleet-server: backup written, {} bytes; {} older backups removed. The vault's keys \
         aren't in it: keep them apart.",
        taken.written.bytes, taken.pruned
    );
    match taken.prune_failed {
        None => Exit::Written,
        Some(error) => {
            let _ = writeln!(
                err,
                "afkfleet-server: removing older backups failed: {}",
                chain(&error)
            );
            Exit::Unpruned
        }
    }
}

/// Logs `error` under `what` and says so on `err`, one line.
fn fail(err: &mut dyn Write, what: &str, error: &(dyn std::error::Error + 'static)) -> Exit {
    let chain = chain(error);
    error!(error = %chain, "{what}");
    let _ = writeln!(err, "afkfleet-server: {what}: {chain}");
    Exit::Failed
}
