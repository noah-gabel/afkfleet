//! The process's OS threads, for the clean-up test (Plan.md P3.8; ADR-0011).
//! Only Linux has a portable count without `unsafe` or a new crate: the
//! `Threads:` line of `/proc/self/status`, and the names in
//! `/proc/self/task/*/comm`.

use std::collections::BTreeMap;
use std::fs;

/// How fleet-mc's host threads are named: `mc-` and the last 12 hex digits
/// of the bot's ID (ADR-0011).
const HOST_THREAD_PREFIX: &str = "mc-";

/// How many of the threads in `names` are fleet-mc host threads.
pub(crate) fn host_threads(names: &BTreeMap<String, usize>) -> usize {
    names
        .iter()
        .filter(|(name, _)| name.starts_with(HOST_THREAD_PREFIX))
        .map(|(_, count)| count)
        .sum()
}

/// This process's threads by name, from `/proc/self/task/*/comm`. `None` on
/// every platform but Linux.
pub(crate) fn os_thread_names() -> Option<BTreeMap<String, usize>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let mut names = BTreeMap::new();
    for task in fs::read_dir("/proc/self/task").expect("/proc/self/task is unreadable") {
        // A thread that ends between the listing and the read isn't counted.
        let Ok(task) = task else { continue };
        let Ok(name) = fs::read_to_string(task.path().join("comm")) else {
            continue;
        };
        *names.entry(name.trim_end().to_owned()).or_default() += 1;
    }
    Some(names)
}

/// The thread count in the text of a `/proc/<pid>/status` file, from its
/// `Threads:` line.
pub(crate) fn threads_in_status(status: &str) -> Option<usize> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))?
        .trim()
        .parse()
        .ok()
}

/// How many threads this process has now. `None` on every platform but
/// Linux, where the count can't be read.
pub(crate) fn os_threads() -> Option<usize> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let status = fs::read_to_string("/proc/self/status").expect("/proc/self/status is unreadable");
    let threads = threads_in_status(&status).expect("/proc/self/status has no `Threads:` line");
    Some(threads)
}

mod tests {
    use super::*;

    #[test]
    fn the_thread_count_comes_from_the_threads_line() {
        let status = "Name:\tminecraft-0123\nUmask:\t0022\nThreads:\t17\nSigQ:\t0/63399\n";

        assert_eq!(threads_in_status(status), Some(17));
    }

    #[test]
    fn a_status_without_a_threads_line_has_no_count() {
        let status = "Name:\tminecraft-0123\nUmask:\t0022\n";

        assert_eq!(threads_in_status(status), None);
    }

    #[test]
    fn a_threads_line_without_a_number_has_no_count() {
        let status = "Threads:\tmany\n";

        assert_eq!(threads_in_status(status), None);
    }

    #[test]
    fn host_threads_are_the_ones_named_mc() {
        let names = BTreeMap::from([
            ("mc-0123456789ab".to_owned(), 1),
            ("mc-ba9876543210".to_owned(), 1),
            ("tokio-runtime-w".to_owned(), 2),
            ("minecraft-0123".to_owned(), 1),
        ]);

        assert_eq!(host_threads(&names), 2);
    }

    #[test]
    fn no_names_means_no_host_threads() {
        assert_eq!(host_threads(&BTreeMap::new()), 0);
    }

    /// The name reader sees a real named thread, so the clean-up test can
    /// wait for host threads to leave. The `test (ubuntu-latest)` CI job
    /// runs it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_named_thread_shows_up_by_its_name() {
        let (release, parked) = std::sync::mpsc::sync_channel::<()>(0);
        let thread = std::thread::Builder::new()
            .name("mc-p38probe".to_owned())
            .spawn(move || {
                let _ = parked.recv();
            })
            .unwrap();

        let names = os_thread_names().unwrap();
        release.send(()).unwrap();
        thread.join().unwrap();

        assert_eq!(names.get("mc-p38probe"), Some(&1));
        assert_eq!(host_threads(&names), 1);
    }

    /// The reader sees a real thread come and go, so the clean-up test's
    /// check can fail at all. The `test (ubuntu-latest)` CI job runs it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_new_thread_raises_the_os_count() {
        let before = os_threads().unwrap();
        let (release, parked) = std::sync::mpsc::sync_channel::<()>(0);
        let thread = std::thread::spawn(move || {
            let _ = parked.recv();
        });

        let during = os_threads().unwrap();
        release.send(()).unwrap();
        thread.join().unwrap();

        assert_eq!(during, before + 1);
    }
}
