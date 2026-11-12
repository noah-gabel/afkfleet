//! The process a crash case kills (ADR-0033): this test binary, started
//! again by the case with [`ROLE`] set, running one real part of the fleet
//! on the files the case left, until the case kills it.
//!
//! [`crash_child`] is an ordinary test that does nothing unless [`ROLE`] is
//! set, so every other run of the binary passes it at once. With a role it
//! never returns: it says on stdout where it is, one `CRASH …` line each
//! ([`say`]), and runs until the operating system ends it.
//!
//! - **`server`:** serve's own wiring on the case's database and PKI
//!   ([`Server::resume`]); `LISTENING <port>` once the gRPC port is bound.
//! - **`agent`:** one run of the managed agent from its identity files;
//!   `RUNNING` once it started.
//! - **`backup`:** the `backup` command's own function, once; `FINISHED` if
//!   nothing killed it first.
//!
//! **A trip** ([`Trip`]) is where the child stops for its kill, and says
//! `TRIPPED`:
//! - [`Trip::Commit`]: SQLite's commit hook on the server's one write
//!   connection, for the n-th transaction that changed a table. The hook
//!   runs before anything of the transaction is durable; there the
//!   connection's thread parks for good, so the process is killed with the
//!   transaction written and not committed. With `hold`, it waits for the
//!   case's `GO` on stdin instead and lets the commit through (`RELEASED`),
//!   for a kill right after it.
//! - [`Trip::Appears`] and [`Trip::Changes`]: a thread of the child's own
//!   that looks at a folder or a file until a name ending in a suffix is
//!   there, or the file's bytes differ, and says so. It only yields between
//!   looks: the child lives a few seconds, and the kill must come within
//!   the milliseconds the watched step takes.
//!
//! Nothing here is in the server's or the agent's code: the hooks are set on
//! the connection from outside, as any embedder of SQLite may.

use core::sync::atomic::{AtomicBool, Ordering};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleet_server::infra::sqlite::Database;

use crate::harness::agent::{RunOptions, run_from_files};
use crate::harness::server::{Ahead, Limits, Server, ServerOptions, config_in};

/// The variable that makes [`crash_child`] a child: its role.
pub(crate) const ROLE: &str = "AFKFLEET_CRASH_ROLE";
/// The folder the child works in: the case's, or an agent's own.
pub(crate) const DIR: &str = "AFKFLEET_CRASH_DIR";
/// Where the child stops: a [`Trip`] as [`Trip::encode`] writes it.
pub(crate) const TRIP: &str = "AFKFLEET_CRASH_TRIP";
/// The server's options, as [`encode_options`] writes them, and what the
/// server child does once it listens.
pub(crate) const OPTIONS: &str = "AFKFLEET_CRASH_OPTIONS";
/// How far ahead of the real clock the server's or the backup's clock
/// runs, in milliseconds: where the case's own clock stands.
pub(crate) const CLOCK_AHEAD_MS: &str = "AFKFLEET_CRASH_CLOCK_AHEAD_MS";
/// The agent's name, its `[control_plane] url`, its seed and its capacity.
pub(crate) const AGENT_NAME: &str = "AFKFLEET_CRASH_AGENT_NAME";
pub(crate) const AGENT_URL: &str = "AFKFLEET_CRASH_AGENT_URL";
pub(crate) const AGENT_SEED: &str = "AFKFLEET_CRASH_AGENT_SEED";
pub(crate) const AGENT_CAPACITY: &str = "AFKFLEET_CRASH_AGENT_CAPACITY";

/// The start of every line the child says.
pub(crate) const SAYS: &str = "CRASH ";

/// The option that has the server child start one retention run once it
/// listens.
const RETENTION_RUN: &str = "retention_run";

/// Where a child stops for its kill; see the [module docs](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trip {
    /// Nowhere: the case kills it when it likes.
    Never,
    /// Before the `nth` commit of a transaction that changed `table`.
    Commit {
        table: String,
        nth: usize,
        /// Whether the commit goes through once the case says `GO`.
        hold: bool,
    },
    /// Once `dir` holds a name that ends in `suffix`.
    Appears { dir: PathBuf, suffix: String },
    /// Once `file`'s bytes differ from what they were at the start.
    Changes { file: PathBuf },
    /// Once a name ending in `suffix` is beside `file`, or `file`'s bytes
    /// differ: a file's replacement through a temporary one, caught while
    /// it's written when the watcher gets to look in time, and right after
    /// it otherwise.
    Replaces { file: PathBuf, suffix: String },
}

impl Trip {
    /// The trip as one variable's value: its parts between `|`, which no
    /// path here holds.
    pub(crate) fn encode(&self) -> String {
        match self {
            Self::Never => "never".to_owned(),
            Self::Commit { table, nth, hold } => format!("commit|{table}|{nth}|{hold}"),
            Self::Appears { dir, suffix } => format!("appears|{}|{suffix}", dir.display()),
            Self::Changes { file } => format!("changes|{}", file.display()),
            Self::Replaces { file, suffix } => format!("replaces|{}|{suffix}", file.display()),
        }
    }

    /// What [`Trip::encode`] wrote.
    fn decode(text: &str) -> Self {
        let parts: Vec<&str> = text.split('|').collect();
        match parts.as_slice() {
            ["never"] => Self::Never,
            ["commit", table, nth, hold] => Self::Commit {
                table: (*table).to_owned(),
                nth: nth.parse().unwrap(),
                hold: hold.parse().unwrap(),
            },
            ["appears", dir, suffix] => Self::Appears {
                dir: PathBuf::from(dir),
                suffix: (*suffix).to_owned(),
            },
            ["changes", file] => Self::Changes {
                file: PathBuf::from(file),
            },
            ["replaces", file, suffix] => Self::Replaces {
                file: PathBuf::from(file),
                suffix: (*suffix).to_owned(),
            },
            other => panic!("not a trip: {other:?}"),
        }
    }
}

/// What the server child needs of `options`, and whether it starts a
/// retention run, as one variable's value.
pub(crate) fn encode_options(options: ServerOptions, retention_run: bool) -> String {
    let mut words = Vec::new();
    if options.retention_by_hand {
        words.push("retention_by_hand");
    }
    if options.backups {
        words.push("backups");
    }
    if options.limits == Limits::LoadSetup {
        words.push("load_setup");
    }
    if retention_run {
        words.push(RETENTION_RUN);
    }
    words.join(",")
}

/// What [`encode_options`] wrote: the options, and whether to start a
/// retention run.
fn decode_options(text: &str) -> (ServerOptions, bool) {
    let has = |word: &str| text.split(',').any(|found| found == word);
    (
        ServerOptions {
            retention_by_hand: has("retention_by_hand"),
            backups: has("backups"),
            limits: if has("load_setup") {
                Limits::LoadSetup
            } else {
                Limits::Production
            },
            ..ServerOptions::default()
        },
        has(RETENTION_RUN),
    )
}

/// Says `line` on stdout, at once: the case reads it there.
fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{SAYS}{line}").unwrap();
    out.flush().unwrap();
}

fn variable(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("the child needs {name}"))
}

/// How far ahead of the real clock this child's clock runs
/// ([`CLOCK_AHEAD_MS`]).
fn ahead() -> core::time::Duration {
    core::time::Duration::from_millis(variable(CLOCK_AHEAD_MS).parse().unwrap())
}

/// The child; see the [module docs](self). Without [`ROLE`], nothing.
#[test]
fn crash_child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let dir = PathBuf::from(variable(DIR));
    let trip = Trip::decode(&variable(TRIP));
    // One thread, as every case's own runtime: the server's and the
    // agent's tasks run on it.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        match role.as_str() {
            "server" => server(&dir, trip).await,
            "agent" => agent(&dir, trip).await,
            "backup" => backup(&dir, trip).await,
            other => panic!("not a role: {other}"),
        }
    });
}

/// The `server` role: serve until killed.
async fn server(dir: &Path, trip: Trip) -> ! {
    let (options, retention_run) = decode_options(&variable(OPTIONS));
    let server = Server::resume(dir, options, ahead()).await;
    match trip {
        Trip::Commit { table, nth, hold } => {
            trip_at_commit(server.database(), table, nth, hold).await;
        }
        other => watch(other),
    }
    say(&format!("LISTENING {}", server.grpc().port()));
    if retention_run {
        let _ = server.retention_run().await;
        say("FINISHED");
    }
    core::future::pending().await
}

/// The `agent` role: one run until killed.
async fn agent(dir: &Path, trip: Trip) -> ! {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    watch(trip);
    let _run = run_from_files(
        dir,
        &variable(AGENT_NAME),
        &variable(AGENT_URL),
        variable(AGENT_SEED).parse().unwrap(),
        RunOptions::capacity(variable(AGENT_CAPACITY).parse().unwrap()),
    );
    say("RUNNING");
    core::future::pending().await
}

/// The `backup` role: the command's own function, once.
async fn backup(dir: &Path, trip: Trip) -> ! {
    let (options, _) = decode_options(&variable(OPTIONS));
    let config = config_in(dir, options);
    watch(trip);
    say("RUNNING");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let _ =
        fleet_server::cli::backup::run(&config, Arc::new(Ahead(ahead())), &mut out, &mut err).await;
    say("FINISHED");
    core::future::pending().await
}

/// Starts the thread that says `TRIPPED` once `trip`'s file is there or
/// changed; nothing for the trips that aren't about files.
fn watch(trip: Trip) {
    let seen: Box<dyn Fn() -> bool + Send> = match trip {
        Trip::Never => return,
        Trip::Commit { .. } => panic!("a commit trip belongs to the server's connection"),
        Trip::Appears { dir, suffix } => Box::new(move || holds(&dir, &suffix)),
        Trip::Changes { file } => {
            let before = std::fs::read(&file).unwrap();
            Box::new(move || std::fs::read(&file).is_ok_and(|now| now != before))
        }
        Trip::Replaces { file, suffix } => {
            let before = std::fs::read(&file).unwrap();
            let dir = file.parent().unwrap().to_owned();
            Box::new(move || {
                holds(&dir, &suffix) || std::fs::read(&file).is_ok_and(|now| now != before)
            })
        }
    };
    std::thread::spawn(move || {
        while !seen() {
            std::thread::yield_now();
        }
        say("TRIPPED");
    });
}

/// Whether `dir` holds a name that ends in `suffix`.
fn holds(dir: &Path, suffix: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(suffix))
        })
    })
}

/// Sets SQLite's hooks on `database`'s one write connection, so the `nth`
/// transaction that changes `table` stops right before it commits; see the
/// [module docs](self).
async fn trip_at_commit(database: &Database, table: String, nth: usize, hold: bool) {
    let mut connection = database.write_pool().acquire().await.unwrap();
    let mut handle = connection.lock_handle().await.unwrap();
    let touched = Arc::new(AtomicBool::new(false));
    handle.set_update_hook({
        let touched = Arc::clone(&touched);
        move |update| {
            if update.table == table {
                touched.store(true, Ordering::SeqCst);
            }
        }
    });
    handle.set_rollback_hook({
        let touched = Arc::clone(&touched);
        move || touched.store(false, Ordering::SeqCst)
    });
    let mut seen = 0_usize;
    handle.set_commit_hook(move || {
        if !touched.swap(false, Ordering::SeqCst) {
            return true;
        }
        seen += 1;
        if seen != nth {
            return true;
        }
        say("TRIPPED");
        if hold {
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "GO", "the case says GO or kills");
            say("RELEASED");
            return true;
        }
        // The connection's own thread: the transaction stays written and
        // uncommitted until the process is killed.
        loop {
            std::thread::park();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_trip_reads_back_as_it_was_written() {
        for trip in [
            Trip::Never,
            Trip::Commit {
                table: "agent_certs".to_owned(),
                nth: 2,
                hold: true,
            },
            Trip::Appears {
                dir: PathBuf::from("backups"),
                suffix: ".partial".to_owned(),
            },
            Trip::Changes {
                file: PathBuf::from("agent.crt"),
            },
            Trip::Replaces {
                file: PathBuf::from("agent.crt"),
                suffix: ".renew.tmp".to_owned(),
            },
        ] {
            assert_eq!(Trip::decode(&trip.encode()), trip);
        }
    }

    #[test]
    fn the_server_s_options_read_back_as_they_were_written() {
        let options = ServerOptions {
            retention_by_hand: true,
            backups: true,
            limits: Limits::LoadSetup,
            ..ServerOptions::default()
        };

        let (read, run) = decode_options(&encode_options(options, true));
        let (plain, no_run) = decode_options(&encode_options(ServerOptions::default(), false));

        assert!(read.retention_by_hand && read.backups && run);
        assert_eq!(read.limits, Limits::LoadSetup);
        assert!(!plain.retention_by_hand && !plain.backups && !no_run);
        assert_eq!(plain.limits, Limits::Production);
    }
}
