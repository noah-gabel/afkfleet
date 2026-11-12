//! The crash cases (Plan.md P13.5; ADR-0033): the real server or the real
//! agent killed by the operating system at an exact point, then started
//! again on the files it left.
//!
//! A container can be killed, but not at a chosen statement, and the
//! production images can neither put the clock 40 days back (a renewal) nor
//! run a retention pass on demand. So here the part to kill runs in a child
//! process of this test binary ([`child`]), the rest of the fleet in the
//! case's own process on the loopback harness, and the child stops where its
//! [`Trip`] says and waits for its kill: `Child::kill`, which is `SIGKILL` on
//! Unix and `TerminateProcess` on Windows. Nothing of the child gets to
//! close, flush or roll back.
//!
//! - [`renewal`]: the server killed between signing a renewed certificate
//!   and its commit, and right after the commit whose answer never reaches
//!   the agent; the agent killed once its certificate file is replaced and
//!   before it uses it.
//! - [`backup`]: the `backup` command killed while its file is being
//!   written.
//! - [`retention`]: the server killed in the middle of a retention run.
//! - [`ingest`]: the server killed in the middle of an ingest batch that
//!   holds chat and a bot's sticky state (ADR-0019, U16's crash case).
//! - [`refresh`]: the server killed while it refreshes an account's
//!   Microsoft token, with the case's own Microsoft (ADR-0019, S64).
//!
//! After every kill a case checks what must hold after any fault
//! ([`after_the_fault`]): SQLite's integrity check on the file as the kill
//! left it, the Owner's new login with the same authenticator (the vault
//! opens what it sealed), and a backup that passes what `restore` checks.
//! Each case ends with [`World::end`]: no account ran twice, and no bot ran
//! where the server didn't put it.
//!
//! The cases run on real time, as every loopback case does; each wait is
//! bounded by [`PATIENCE`] and wakes on the child's next line or the log's.

mod backup;
mod child;
mod ingest;
mod refresh;
mod renewal;
mod retention;

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use fleet_core::audit::RecordedValue;
use fleet_core::system::Clock as _;
use fleet_server::cli::vault;
use fleet_server::config::{DatabaseConfig, ServerConfig};
use fleet_server::infra::sqlite::integrity::{Integrity, check};
use fleet_server::infra::sqlite::{self, DatabaseOptions};
use fleet_server::infra::system::SystemClock;
use fleet_server::ports::audit::AuditRecord;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use self::child::{
    AGENT_CAPACITY, AGENT_NAME, AGENT_SEED, AGENT_URL, CLOCK_AHEAD_MS, DIR, OPTIONS, ROLE, SAYS,
    TRIP, Trip, encode_options,
};
use crate::harness::agent::Agent;
use crate::harness::logs::PATIENCE;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// The child's test, by its full name, as the binary's own argument.
const CHILD_TEST: &str = "crash::child::crash_child";

/// A child process the case kills; see the [module docs](self). Dropping
/// it kills it too, so a case that fails leaves none behind.
#[derive(Debug)]
pub(crate) struct Killable {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    /// What it said so far, for a failure's message.
    said: Vec<String>,
    /// When it started: its clock ran on from the case's since then.
    started: std::time::Instant,
}

impl Killable {
    /// Starts this test binary again as the child `role` in `dir`, stopping
    /// at `trip`, with `more` variables.
    fn spawn(role: &str, dir: &Path, trip: &Trip, more: &[(&str, String)]) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env(ROLE, role)
            .env(DIR, dir)
            .env(TRIP, trip.encode())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Its panics, beside the case's own output.
            .stderr(Stdio::inherit());
        for (name, value) in more {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel(64);
        // Ends when the child's stdout closes, at its kill.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // Anywhere in the line: the test harness writes the child
                // test's name before its first line, with no line break.
                if let Some((_, said)) = line.split_once(SAYS)
                    && sender.blocking_send(said.to_owned()).is_err()
                {
                    return;
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            said: Vec::new(),
            started: std::time::Instant::now(),
        }
    }

    /// Waits until the child says a line that starts with `start`, within
    /// [`PATIENCE`]; the rest of that line.
    pub(crate) async fn says(&mut self, start: &str) -> String {
        loop {
            let line = tokio::time::timeout(PATIENCE, self.lines.recv())
                .await
                .unwrap_or_else(|_| panic!("the child never said {start}: {:?}", self.said))
                .unwrap_or_else(|| panic!("the child ended before {start}: {:?}", self.said));
            self.said.push(line.clone());
            if let Some(rest) = line.strip_prefix(start) {
                return rest.trim().to_owned();
            }
        }
    }

    /// Whether the child said `line` by now.
    pub(crate) fn said(&mut self, line: &str) -> bool {
        while let Ok(found) = self.lines.try_recv() {
            self.said.push(found);
        }
        self.said.iter().any(|found| found == line)
    }

    /// Lets a held commit through.
    pub(crate) fn go(&mut self) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(b"GO\n").unwrap();
        stdin.flush().unwrap();
    }

    /// Kills the child, which must still run: `SIGKILL` or
    /// `TerminateProcess`, then waits until it's gone. `world`'s clock
    /// moves past the child's, which ran on from it while the child lived:
    /// the next server never starts behind the one that died.
    pub(crate) fn kill(mut self, world: &World) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "the child ended by itself before its kill: {:?}",
            self.said
        );
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        assert!(!status.success(), "a killed process doesn't end with 0");
        world
            .server
            .clock()
            .advance(self.started.elapsed() + core::time::Duration::from_secs(1));
    }
}

impl Drop for Killable {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `record`'s metadata value `key`, as text.
pub(crate) fn metadata(record: &AuditRecord, key: &str) -> Option<String> {
    record
        .metadata
        .entries()
        .iter()
        .find(|(found, _)| found == key)
        .map(|(_, value)| match value {
            RecordedValue::Text(text) => text.clone(),
            RecordedValue::Int(n) => n.to_string(),
            other => format!("{other:?}"),
        })
}

/// How far the case's clock stands ahead of the real one, in whole
/// milliseconds, for a child's [`CLOCK_AHEAD_MS`].
fn clock_ahead(world: &World) -> String {
    world
        .server
        .clock()
        .now()
        .signed_duration_since(SystemClock.now())
        .num_milliseconds()
        .max(0)
        .to_string()
}

/// The folder that holds the case's database and PKI.
fn folder(world: &World) -> PathBuf {
    world
        .server
        .config()
        .database
        .path
        .parent()
        .unwrap()
        .to_owned()
}

/// The case's server, stopped, serving again in a child process as
/// `options` say (the same ones the case's own server runs with), stopping
/// at `trip`; with `retention_run`, it starts one retention run once it
/// listens. The agents' proxies dial it from now on.
pub(crate) async fn serve_in_child(
    world: &World,
    options: ServerOptions,
    trip: Trip,
    retention_run: bool,
) -> Killable {
    let mut child = Killable::spawn(
        "server",
        &folder(world),
        &trip,
        &[
            (OPTIONS, encode_options(options, retention_run)),
            (CLOCK_AHEAD_MS, clock_ahead(world)),
        ],
    );
    let port: u16 = child.says("LISTENING ").await.parse().unwrap();
    world
        .server
        .moved_to(SocketAddr::from(([127, 0, 0, 1], port)));
    child
}

/// The `backup` command in a child process, on the case's database with its
/// server stopped, stopping at `trip`.
pub(crate) async fn back_up_in_child(
    world: &World,
    options: ServerOptions,
    trip: Trip,
) -> Killable {
    let mut child = Killable::spawn(
        "backup",
        &folder(world),
        &trip,
        &[
            (OPTIONS, encode_options(options, false)),
            (CLOCK_AHEAD_MS, clock_ahead(world)),
        ],
    );
    child.says("RUNNING").await;
    child
}

/// One run of `agent`, which isn't running here, in a child process with
/// room for `capacity` bots, stopping at `trip`. It dials the agent's own
/// proxy, in this process.
pub(crate) async fn run_in_child(agent: &Agent, capacity: usize, trip: Trip) -> Killable {
    assert!(!agent.runs(), "the agent runs in one process at a time");
    let mut child = Killable::spawn(
        "agent",
        agent.dir(),
        &trip,
        &[
            (AGENT_NAME, agent.name.clone()),
            (AGENT_URL, agent.proxy.url()),
            (AGENT_SEED, agent.seed().to_string()),
            (AGENT_CAPACITY, capacity.to_string()),
        ],
    );
    child.says("RUNNING").await;
    child
}

/// The database file passes SQLite's integrity check, as it is now: with
/// the server dead, that's the file and the `-wal` the kill left.
pub(crate) async fn assert_sound(config: &DatabaseConfig) {
    let found = check(config, &DatabaseOptions::default()).await;
    assert!(matches!(found, Ok(Integrity::Sound)), "{found:?}");
}

/// What must hold after any fault, once the case's server serves again
/// (with backups on): the database is sound; the Owner logs in anew with
/// the same authenticator, so the vault opens the secret it sealed; and a
/// backup taken now passes what `restore` checks ([`assert_restorable`]).
pub(crate) async fn after_the_fault(world: &mut World) {
    assert_sound(&world.server.config().database).await;
    world.owner.log_in_again(&world.server).await;
    let taken = world
        .server
        .backups()
        .take(&CancellationToken::new())
        .await
        .unwrap();
    assert_restorable(world.server.config(), &taken.written.file).await;
}

/// `backup` passes what `setup.sh restore` checks before it lets a server
/// start on it: as the database, every migration is this server's
/// (`migrate`), SQLite finds it sound, and `vault status` finds every
/// secret sealed with a key the server has. On a copy, never the backup
/// itself.
pub(crate) async fn assert_restorable(config: &ServerConfig, backup: &Path) {
    let dir = tempfile::tempdir().unwrap();
    let restored = DatabaseConfig {
        path: dir.path().join("afkfleet.db"),
        backups: None,
    };
    std::fs::copy(backup, &restored.path).unwrap();
    sqlite::open_migrated(&restored, DatabaseOptions::default())
        .await
        .unwrap()
        .close()
        .await;
    assert_sound(&restored).await;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = vault::status(
        &ServerConfig {
            database: restored,
            ..config.clone()
        },
        &mut out,
        &mut err,
    )
    .await;
    assert_eq!(
        status,
        vault::Exit::Done,
        "{}",
        String::from_utf8_lossy(&err)
    );
}
