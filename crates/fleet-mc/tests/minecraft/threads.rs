//! The process's OS thread count, for the clean-up test (Plan.md P3.8;
//! ADR-0011). Only Linux has a portable count without `unsafe` or a new
//! crate: the `Threads:` line of `/proc/self/status`.

use std::fs;

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
