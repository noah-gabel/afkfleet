//! What the tests share: test directories and configs, the binary run as a
//! child process, the server run in-process with a signal source the test
//! feeds, and the server's log layer writing into a buffer.

use core::time::Duration;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;

use axum::Router;
use chrono::DateTime;
use fleet_core::auth::LockoutPolicy;
use fleet_core::system::Clock;
use fleet_server::app::lockout::LockoutService;
use fleet_server::cli::housekeeping::Housekeeping;
use fleet_server::cli::serve::App;
use fleet_server::config::{
    AuthConfig, DatabaseConfig, HttpConfig, LogConfig, LogFormat, RateLimitConfig, ServerConfig,
    VaultConfig,
};
use fleet_server::http::rate_limit::RateLimits;
use fleet_server::http::rate_limit::TrustedProxies;
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::vault::VaultKeys;
use fleet_server::infra::sqlite::Database;
use fleet_startup::signals::{ShutdownSignals, Signal};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::ManualClock;
use secrecy::SecretBox;
use serde_json::Value;
use tokio::net::TcpListener;
use tracing_subscriber::layer::SubscriberExt as _;

pub(crate) const BINARY: &str = env!("CARGO_BIN_EXE_afkfleet-server");

/// How long any one run of the binary may take.
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// An empty directory of its own for the test `name`, under Cargo's temp
/// directory for tests.
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("serve")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A TOML literal string for `path`, so Windows backslashes stay.
pub(crate) fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

/// Writes `body` as `server.toml` in `dir` and returns its path.
pub(crate) fn write_config(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("server.toml");
    fs::write(&path, body).unwrap();
    path
}

/// The tests' vault key: 32 bytes of 0x5A as standard base64 with padding
/// (`Buffer.alloc(32, 0x5a).toString("base64")` in Node).
pub(crate) const TEST_VAULT_KEY: &str = "WlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlo=";

/// Writes the vault key `vault-key-1` into `dir` (32 bytes of 0x5A as
/// standard base64, owner-only on Unix) and returns its path.
pub(crate) fn vault_key(dir: &Path) -> PathBuf {
    let path = dir.join("vault-key-1");
    fs::write(&path, format!("{TEST_VAULT_KEY}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

/// The `[vault]` section naming `dir`'s vault key, which it writes.
pub(crate) fn vault_section(dir: &Path) -> String {
    format!(
        "[vault]\nmaster_key_files = {{ 1 = {} }}\nactive_key_id = 1\n",
        toml_path(&vault_key(dir))
    )
}

/// A config for a server bound to `bind` with its database and vault key in
/// `dir`, JSON logs at `info`.
pub(crate) fn config_body(dir: &Path, bind: SocketAddr) -> String {
    format!(
        "[http]\nbind = \"{bind}\"\n[database]\npath = {}\n[log]\nformat = \"json\"\n{}",
        toml_path(&dir.join("afkfleet.db")),
        vault_section(dir)
    )
}

/// The keys of the vault [`server_config`] names, loaded as main.rs does.
pub(crate) fn vault_keys(config: &ServerConfig) -> VaultKeys {
    VaultKeys::load(&config.vault).unwrap()
}

/// The in-process equivalent of [`config_body`]: `bind` is only what the
/// healthcheck would ask, since tests hand `run` a bound listener.
pub(crate) fn server_config(dir: &Path, bind: SocketAddr, dev_mode: bool) -> ServerConfig {
    ServerConfig {
        dev_mode,
        http: HttpConfig {
            bind,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 65_536,
            trusted_proxies: TrustedProxies::default(),
        },
        database: DatabaseConfig {
            path: dir.join("afkfleet.db"),
        },
        log: LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        },
        auth: AuthConfig::default(),
        rate_limits: RateLimitConfig::default(),
        vault: VaultConfig {
            key_files: BTreeMap::from([(1, vault_key(dir))]),
            active_key_id: 1,
        },
    }
}

pub(crate) fn path_arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// A listener on a free loopback port, and its address.
pub(crate) async fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

/// A loopback address nothing listens on: a free port, released again.
pub(crate) fn closed_port() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

/// Every line of `text` that's a JSON object, parsed.
pub(crate) fn json_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

/// The JSON log lines of `text` whose message is `message`.
pub(crate) fn lines_with<'a>(lines: &'a [Value], message: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|line| line["message"] == message)
        .collect()
}

/// Kills the binary unless the test took it out first.
pub(crate) struct Running(pub(crate) Option<Child>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The binary with `args`, its pipes captured, and none of the test's own
/// `AFKFLEET_SERVER__…` variables.
pub(crate) fn server(args: &[&str]) -> Command {
    let mut command = Command::new(BINARY);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AFKFLEET_SERVER__") {
            command.env_remove(key);
        }
    }
    command
}

/// Reads `pipe` to its end on a thread; the text arrives once it closes.
pub(crate) fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    receiver
}

/// How a finished run of the binary ended.
pub(crate) struct Finished {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Runs the binary with `args` until it exits, within the deadline.
pub(crate) fn finish(args: &[&str]) -> Finished {
    let mut child = server(args).spawn().unwrap();
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let mut running = Running(Some(child));
    let stdout = stdout
        .recv_timeout(DEADLINE)
        .expect("the binary should exit before the deadline");
    let stderr = stderr.recv_timeout(DEADLINE).unwrap();
    let status = running.0.take().unwrap().wait().unwrap();
    Finished {
        status,
        stdout,
        stderr,
    }
}

/// Runs the binary with `args` until it exits, within the deadline, with
/// `stdin` piped in and closed, or, with `None`, a stdin that stays open and
/// unwritten until the binary has exited: a binary that read it would hang
/// until the deadline and fail the test.
pub(crate) fn finish_with_stdin(args: &[&str], stdin: Option<&[u8]>) -> Finished {
    let mut child = server(args).stdin(Stdio::piped()).spawn().unwrap();
    let mut pipe = child.stdin.take();
    if let Some(bytes) = stdin {
        pipe.take().unwrap().write_all(bytes).unwrap();
    }
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let mut running = Running(Some(child));
    let stdout = stdout
        .recv_timeout(DEADLINE)
        .expect("the binary should exit before the deadline");
    let stderr = stderr.recv_timeout(DEADLINE).unwrap();
    let status = running.0.take().unwrap().wait().unwrap();
    drop(pipe);
    Finished {
        status,
        stdout,
        stderr,
    }
}

/// [`finish`] on a blocking thread, for a test whose runtime also runs an
/// in-process server.
pub(crate) async fn finish_beside(args: Vec<String>) -> Finished {
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        finish(&args)
    })
    .await
    .unwrap()
}

/// Delivers the signals the test sends; waits forever once the sender is
/// gone, like a real source whose streams ended.
#[derive(Debug)]
pub(crate) struct ChannelSignals(tokio::sync::mpsc::Receiver<Signal>);

impl ChannelSignals {
    /// A source and the sender that feeds it.
    pub(crate) fn new() -> (tokio::sync::mpsc::Sender<Signal>, Self) {
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        (sender, Self(receiver))
    }

    /// A source that never delivers a signal.
    pub(crate) fn silent() -> Self {
        Self::new().1
    }
}

impl ShutdownSignals for ChannelSignals {
    async fn recv(&mut self) -> Signal {
        match self.0.recv().await {
            Some(signal) => signal,
            None => core::future::pending().await,
        }
    }
}

/// The server's log layer at `info`, writing into a buffer, as the default
/// subscriber of this thread. A test on tokio's current-thread runtime sees
/// everything the server logs.
pub(crate) fn capture() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let guard = tracing::subscriber::set_default(subscriber);
    (buffer, guard)
}

/// `router` with the housekeeping `serve` runs beside it: the default
/// limits and a lockout over `database`, on a clock that never moves, with
/// a fixed lockout key.
pub(crate) fn app_of(router: Router, database: &Database) -> App {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(
        DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
    ));
    let lockout = LockoutService::new(
        Arc::clone(&clock),
        Arc::new(database.clone()),
        Arc::new(HmacAttemptKeys::new(SecretBox::new(Box::new([7; 32])))),
        LockoutPolicy::default(),
    );
    App {
        router,
        housekeeping: Housekeeping::new(
            RateLimits::new(&RateLimitConfig::default(), clock),
            lockout,
        ),
    }
}

/// Aborts an in-process task when the test ends, however it ends.
pub(crate) struct AbortOnDrop<T>(pub(crate) tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
