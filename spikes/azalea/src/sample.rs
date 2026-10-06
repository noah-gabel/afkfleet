//! Process resource sampling from `/proc/self` (Linux only; std only).
//! On other platforms every sample is `None`.

use std::{collections::BTreeMap, fs, process::Command};

#[derive(Clone, Debug)]
pub struct Sample {
    pub rss_kb: u64,
    pub threads: u64,
    pub fds: usize,
    /// utime + stime, in clock ticks.
    pub cpu_ticks: u64,
    /// Thread names (digits and spaces stripped) → count.
    pub thread_names: BTreeMap<String, usize>,
}

pub fn sample() -> Option<Sample> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let field = |key: &str| -> Option<u64> {
        status
            .lines()
            .find(|l| l.starts_with(key))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the `(comm)`: index 0 is field 3 (state), so utime (14)
    // and stime (15) are at 11 and 12.
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let mut thread_names = BTreeMap::new();
    for task in fs::read_dir("/proc/self/task").ok()?.flatten() {
        let name = fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        let name: String = name
            .trim()
            .trim_end_matches(|c: char| {
                c.is_ascii_digit() || c == ' ' || c == '-' || c == '(' || c == ')'
            })
            .to_owned();
        *thread_names.entry(name).or_insert(0) += 1;
    }
    Some(Sample {
        rss_kb: field("VmRSS:")?,
        threads: field("Threads:")?,
        fds: fs::read_dir("/proc/self/fd").ok()?.count(),
        cpu_ticks: utime + stime,
        thread_names,
    })
}

/// Clock ticks per second (`getconf CLK_TCK`; std can't call `sysconf`
/// without `unsafe`). Falls back to 100.
pub fn clk_tck() -> u64 {
    Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(100)
}

pub fn brief(s: &Option<Sample>) -> String {
    match s {
        Some(s) => format!(
            "rss={} MiB threads={} fds={} names={:?}",
            s.rss_kb / 1024,
            s.threads,
            s.fds,
            s.thread_names
        ),
        None => "n/a (not Linux)".to_owned(),
    }
}
