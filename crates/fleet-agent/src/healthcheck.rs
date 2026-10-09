//! `afkfleet-agent healthcheck`: is the agent's heartbeat fresh? (Plan.md
//! P5.5, ADR-0014)
//!
//! Docker runs the command inside the agent's container, which has no shell
//! and no curl. It loads the agent's own config, so both read the same
//! `heartbeat_file`, and [`check`] reads the file's modification time:
//! - Touched less than [`STALE_AFTER`] ago: healthy, exit 0.
//! - Missing, [`STALE_AFTER`] old or more, or unreadable: unhealthy, exit 1.
//!   Docker reserves 2 for healthchecks, so it's never used.
//! - A time in the future means the wall clock went back. Up to
//!   [`STALE_AFTER`] ahead counts as just touched, so a small clock
//!   correction never fails the check; further ahead is unhealthy, so a hung
//!   agent can't look healthy for the length of a big jump. A healthy
//!   agent's next beat sets the time to the new "now" anyway.
//!
//! Each outcome is one line on stdout, which Docker keeps with the check's
//! result.

use core::fmt;
use core::time::Duration;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::heartbeat::STALE_AFTER;

/// The agent's health, as its heartbeat file shows it.
#[derive(Debug)]
pub enum Health {
    /// The heartbeat is fresh.
    Fresh {
        /// How long ago the file was touched; a time slightly in the future
        /// counts as 0.
        age: Duration,
    },
    /// The file is [`STALE_AFTER`] old or more.
    Stale {
        /// How long ago it was touched.
        age: Duration,
    },
    /// The file's time is more than [`STALE_AFTER`] in the future.
    Future {
        /// How far ahead of now it is.
        ahead: Duration,
    },
    /// There's no heartbeat file.
    Missing {
        /// Where it should be.
        path: PathBuf,
    },
    /// The file's time can't be read.
    Unreadable {
        /// The file.
        path: PathBuf,
        /// Why it can't be read.
        error: io::Error,
    },
}

impl Health {
    /// Whether the agent is healthy.
    #[must_use]
    pub const fn is_healthy(&self) -> bool {
        matches!(self, Self::Fresh { .. })
    }

    /// The healthcheck's exit code: 0 when healthy, 1 otherwise.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        if self.is_healthy() { 0 } else { 1 }
    }
}

impl fmt::Display for Health {
    /// The one line the healthcheck prints, with times in whole seconds.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fresh { age } => {
                write!(f, "healthy: the heartbeat is {} s old", age.as_secs())
            }
            Self::Stale { age } => {
                write!(
                    f,
                    "unhealthy: the heartbeat file is {} s old",
                    age.as_secs()
                )
            }
            Self::Future { ahead } => write!(
                f,
                "unhealthy: the heartbeat file's time is {} s in the future",
                ahead.as_secs()
            ),
            Self::Missing { path } => {
                write!(f, "unhealthy: no heartbeat file at {}", path.display())
            }
            Self::Unreadable { path, error } => write!(
                f,
                "unhealthy: the heartbeat file at {} can't be read: {error}",
                path.display()
            ),
        }
    }
}

/// Checks the heartbeat file at `path` at the wall time `now`.
#[must_use]
pub fn check(path: &Path, now: SystemTime) -> Health {
    match std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => freshness(modified, now),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Health::Missing {
            path: path.to_owned(),
        },
        Err(error) => Health::Unreadable {
            path: path.to_owned(),
            error,
        },
    }
}

/// How fresh a heartbeat touched at `modified` is at `now`.
fn freshness(modified: SystemTime, now: SystemTime) -> Health {
    match now.duration_since(modified) {
        Ok(age) if age < STALE_AFTER => Health::Fresh { age },
        Ok(age) => Health::Stale { age },
        // The wall clock went back since the last beat.
        Err(ahead) => {
            let ahead = ahead.duration();
            if ahead <= STALE_AFTER {
                Health::Fresh {
                    age: Duration::ZERO,
                }
            } else {
                Health::Future { ahead }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    /// The wall time every pure check runs at.
    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + secs(1_700_000_000)
    }

    #[rstest]
    #[case::just_touched(0)]
    #[case::two_beats_missed(29)]
    fn a_heartbeat_younger_than_30_s_is_healthy(#[case] age: u64) {
        let health = freshness(now() - secs(age), now());

        assert!(
            matches!(health, Health::Fresh { age: fresh } if fresh == secs(age)),
            "{health:?}"
        );
        assert!(health.is_healthy());
        assert_eq!(health.exit_code(), 0);
        assert_eq!(
            health.to_string(),
            format!("healthy: the heartbeat is {age} s old")
        );
    }

    #[rstest]
    #[case::at_the_limit(30)]
    #[case::an_hour(3600)]
    fn a_heartbeat_30_s_old_or_more_is_stale(#[case] age: u64) {
        let health = freshness(now() - secs(age), now());

        assert!(
            matches!(health, Health::Stale { age: stale } if stale == secs(age)),
            "{health:?}"
        );
        assert!(!health.is_healthy());
        assert_eq!(health.exit_code(), 1);
        assert_eq!(
            health.to_string(),
            format!("unhealthy: the heartbeat file is {age} s old")
        );
    }

    #[rstest]
    #[case::a_second(1)]
    #[case::at_the_limit(30)]
    fn a_time_at_most_30_s_ahead_counts_as_just_touched(#[case] ahead: u64) {
        let health = freshness(now() + secs(ahead), now());

        assert!(
            matches!(health, Health::Fresh { age } if age == Duration::ZERO),
            "{health:?}"
        );
        assert_eq!(health.exit_code(), 0);
        assert_eq!(health.to_string(), "healthy: the heartbeat is 0 s old");
    }

    #[rstest]
    #[case::past_the_limit(31)]
    #[case::an_hour(3600)]
    fn a_time_more_than_30_s_ahead_is_unhealthy(#[case] ahead: u64) {
        let health = freshness(now() + secs(ahead), now());

        assert!(
            matches!(health, Health::Future { ahead: future } if future == secs(ahead)),
            "{health:?}"
        );
        assert!(!health.is_healthy());
        assert_eq!(health.exit_code(), 1);
        assert_eq!(
            health.to_string(),
            format!("unhealthy: the heartbeat file's time is {ahead} s in the future")
        );
    }

    #[test]
    fn a_partial_second_is_shown_in_whole_seconds() {
        let health = freshness(now() - Duration::from_millis(4_900), now());

        assert_eq!(health.to_string(), "healthy: the heartbeat is 4 s old");
    }

    #[test]
    fn a_missing_file_is_unhealthy_with_its_path() {
        // The directory doesn't exist either, so nothing can create the file.
        let path = std::env::temp_dir()
            .join("afkfleet-agent-healthcheck-test-missing")
            .join("agent.alive");

        let health = check(&path, now());

        assert!(
            matches!(&health, Health::Missing { path: missing } if *missing == path),
            "{health:?}"
        );
        assert_eq!(health.exit_code(), 1);
        assert_eq!(
            health.to_string(),
            format!("unhealthy: no heartbeat file at {}", path.display())
        );
    }

    #[test]
    fn an_existing_files_age_comes_from_its_modification_time() {
        // A file that exists without the test writing anything.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();

        let health = check(&path, modified + secs(5));

        assert!(
            matches!(health, Health::Fresh { age } if age == secs(5)),
            "{health:?}"
        );
    }

    /// A path through a regular file is `ENOTDIR` on Unix, not `NotFound`.
    #[cfg(unix)]
    #[test]
    fn a_file_whose_time_cant_be_read_is_unhealthy_with_the_error() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("Cargo.toml")
            .join("agent.alive");

        let health = check(&path, now());

        assert!(
            matches!(&health, Health::Unreadable { path: unreadable, .. } if *unreadable == path),
            "{health:?}"
        );
        assert_eq!(health.exit_code(), 1);
        let line = health.to_string();
        assert!(
            line.starts_with(&format!(
                "unhealthy: the heartbeat file at {} can't be read: ",
                path.display()
            )),
            "{line}"
        );
    }
}
