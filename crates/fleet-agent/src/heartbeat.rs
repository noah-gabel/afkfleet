//! The heartbeat file: the agent's proof, for Docker's healthcheck, that its
//! fleet still answers (Plan.md P5.5, ADR-0014).
//!
//! While the agent runs, it touches its heartbeat file every [`PERIOD`], but
//! only when the fleet's supervisor answers a snapshot of every bot. The
//! `afkfleet-agent healthcheck` command ([`crate::healthcheck`]) reads the
//! file's modification time from another process: a file [`STALE_AFTER`]
//! old or more means the agent is unhealthy. This works in a distroless
//! image, which has no shell and no curl.
//!
//! The run touches the file through the [`Heartbeat`] port, so its tests
//! count the beats on paused time; the binary passes [`FileHeartbeat`].

use core::time::Duration;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How often the agent touches its heartbeat file.
pub const PERIOD: Duration = Duration::from_secs(10);

/// The age at which the heartbeat file is stale: three missed beats.
pub const STALE_AFTER: Duration = Duration::from_secs(30);

/// Where the agent's heartbeat goes: [`FileHeartbeat`] in the binary, a
/// fake in tests.
pub trait Heartbeat: Send + Sync + 'static {
    /// Records one beat.
    ///
    /// # Errors
    /// The IO error that kept the beat from being recorded.
    fn touch(&self) -> impl Future<Output = io::Result<()>> + Send;
}

/// The heartbeat file: each beat sets its modification time to now.
///
/// A beat creates the file if it's missing. It never truncates or deletes
/// the file, so a wrong path can't destroy data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHeartbeat {
    path: PathBuf,
}

impl FileHeartbeat {
    /// The heartbeat file at `path`, which should be absolute: the
    /// healthcheck may run in another directory.
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Heartbeat for FileHeartbeat {
    async fn touch(&self) -> io::Result<()> {
        let path = self.path.clone();
        // Blocking file IO stays off the runtime's threads. No timeout: a
        // filesystem that hangs leaves the heartbeat stale, which is what the
        // healthcheck should report, and the run stops waiting for it when
        // its shutdown starts.
        match tokio::task::spawn_blocking(move || touch(&path)).await {
            Ok(touched) => touched,
            Err(_) => Err(io::Error::other("the heartbeat's file task didn't finish")),
        }
    }
}

/// Creates the file at `path` if it's missing and sets its modification time
/// to now.
fn touch(path: &Path) -> io::Result<()> {
    // Write access, which Windows needs to set the time; the file is never
    // truncated.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.set_modified(SystemTime::now())
}
