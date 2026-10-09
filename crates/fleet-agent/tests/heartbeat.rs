//! The heartbeat file on a real filesystem (Plan.md P5.5; ADR-0014): a beat
//! creates the file, moves its modification time to now and never touches
//! its content. Each test has a directory of its own under Cargo's temp
//! directory for tests.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fleet_agent::heartbeat::{FileHeartbeat, Heartbeat as _};

/// An empty directory of its own for the test `name`.
fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("heartbeat")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

/// Some filesystems keep coarse times.
const SLACK: Duration = Duration::from_secs(2);

/// Asserts that `path` was modified between `before` and `after`.
fn assert_modified_between(path: &Path, before: SystemTime, after: SystemTime) {
    let modified = modified(path);
    assert!(
        modified + SLACK >= before && modified <= after + SLACK,
        "{modified:?} is outside {before:?}..{after:?}"
    );
}

#[tokio::test]
async fn a_beat_creates_a_missing_file() {
    let path = test_dir("creates").join("agent.alive");
    let heartbeat = FileHeartbeat::new(path.clone());
    let before = SystemTime::now();

    heartbeat.touch().await.unwrap();

    assert_eq!(heartbeat.path(), path);
    assert!(path.is_file(), "the beat should create the file");
    assert_eq!(fs::read(&path).unwrap(), b"");
    assert_modified_between(&path, before, SystemTime::now());
}

#[tokio::test]
async fn a_beat_moves_an_old_modification_time_to_now() {
    let path = test_dir("moves_time").join("agent.alive");
    let file = File::create(&path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(file);
    let before = SystemTime::now();

    FileHeartbeat::new(path.clone()).touch().await.unwrap();

    assert_modified_between(&path, before, SystemTime::now());
}

#[tokio::test]
async fn a_beat_keeps_the_files_content() {
    let path = test_dir("keeps_content").join("agent.alive");
    fs::write(&path, "not ours").unwrap();

    FileHeartbeat::new(path.clone()).touch().await.unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "not ours");
}

#[tokio::test]
async fn a_beat_fails_when_the_files_directory_is_missing() {
    let missing = test_dir("missing_dir").join("missing");
    let path = missing.join("agent.alive");

    let touched = FileHeartbeat::new(path).touch().await;

    let Err(error) = touched else {
        panic!("the beat should fail without its directory");
    };
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    assert!(!missing.exists());
}
