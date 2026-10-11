//! `afkfleet-server migrate` (Plan.md P6.11, ADR-0005, ADR-0015): applies
//! every pending migration and exits.
//!
//! It opens the database the way `serve` does, through
//! [`sqlite::open`]: the folder must exist, a
//! missing file is created with owner-only permissions, and every pending
//! migration runs. Then it closes the database within
//! [`DATABASE_CLOSE`]. `serve` migrates at startup
//! too; this command lets an operator migrate before starting a new version.
//!
//! It logs like `serve`: "the database is migrated" at `info`, or one
//! `error`, "the database can't be migrated", with the error's whole chain.

use tracing::{error, info};

use super::DATABASE_CLOSE;
use super::serve::StartError;
use crate::config::DatabaseConfig;
use crate::http::error::chain;
use crate::infra::sqlite::{self, DatabaseOptions};

/// How the migration ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Every migration is applied and the database closed.
    Migrated,
    /// Something failed; the log says what.
    Failed,
}

impl Exit {
    /// The process's exit code (Plan.md P6.11, ADR-0015).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Migrated => 0,
            Self::Failed => 1,
        }
    }
}

/// Logs that the migration couldn't run because of `error`, such as the
/// async runtime, and returns [`Exit::Failed`].
pub fn cant_migrate(error: &StartError) -> Exit {
    error!(error = %chain(error), "the database can't be migrated");
    Exit::Failed
}

/// Opens the database at `config`'s path, which applies every pending
/// migration, and closes it.
pub async fn run(config: &DatabaseConfig) -> Exit {
    let path = config.path.display();
    let database = match sqlite::open(config, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => {
            error!(%path, error = %chain(&error), "the database can't be migrated");
            return Exit::Failed;
        }
    };
    match database.close_within(DATABASE_CLOSE).await {
        Ok(()) => {
            info!(%path, "the database is migrated");
            Exit::Migrated
        }
        Err(error) => {
            error!(%path, %error, "the database can't be migrated");
            Exit::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_exit_has_its_code() {
        assert_eq!(Exit::Migrated.code(), 0);
        assert_eq!(Exit::Failed.code(), 1);
    }
}
