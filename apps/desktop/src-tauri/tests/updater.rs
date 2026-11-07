//! The update's proof (Plan.md P12.5; ADR-0022, S24, S27, R19): the real
//! tauri-plugin-updater, on Tauri's mock runtime with the real
//! `tauri.conf.json` (its `requireSignedVersion` included), checks and
//! downloads a throwaway release from a loopback server, through the app's
//! own adapter, `PluginUpdates`. Only the public key and the endpoint are
//! the test's own.
//!
//! `scripts/updater-fixture.mjs` makes the release at test time: two key
//! pairs from the pinned Tauri CLI, signed through `just updater-sign`'s own
//! functions, so the plugin reads exactly what the maintainer's step writes.
//! No key is committed and none is real. Nothing is installed: `install` is
//! the user's rehearsal's (`just update-rehearsal`).
//!
//! - a newer release, signed for its version by the configured key, is found
//!   and downloaded byte for byte;
//! - changed bytes, another key's signature, a version raised in the
//!   manifest after signing, and a signature without a version each fail
//!   the download's checks;
//! - the app's own version is no update; a manifest that isn't JSON can't be
//!   read; a missing one is unreachable; a download over the cap is refused.
//!
//! It needs `node` and `pnpm install` (the CLI). It runs on real time, as
//! tests over real sockets do, with short bounds and no sleep.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use afkfleet_desktop::update::{PluginUpdates, UpdateFailure, Updates};
use serde_json::{Value, json};
use tauri::App;
use tauri::test::{MockRuntime, mock_builder};
use tempfile::TempDir;

/// How long one check or download may take here.
const WAIT: Duration = Duration::from_secs(30);
/// How long the fixture may take: two key pairs and three signatures.
const FIXTURE_WAIT: Duration = Duration::from_secs(120);
/// The release's version: newer than the app's 0.1.0.
const VERSION: &str = "0.9.0";
/// The installer's name in the release.
const INSTALLER: &str = "afkfleet_0.9.0_x64-setup.exe";

/// What the loopback server answers.
struct Served {
    /// The fixture's folder.
    dir: PathBuf,
    /// What `/latest.json` answers; `None` is a 404.
    manifest: Mutex<Option<Vec<u8>>>,
    /// Whether the installer's last byte is changed on its way out.
    tamper: AtomicBool,
    /// When set, the installer streams without end and with no length, and
    /// the bytes it got out go here once the client stops reading.
    endless: Mutex<Option<mpsc::Sender<u64>>>,
    /// Set when the test ends, so the server stops.
    stop: AtomicBool,
}

/// A throwaway release on loopback, and the app that updates from it.
struct Release {
    served: Arc<Served>,
    address: SocketAddr,
    app: App<MockRuntime>,
    _dir: TempDir,
}

impl Drop for Release {
    fn drop(&mut self) {
        self.served.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
    }
}

/// The repository's root.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Runs the fixture into `dir` for the server at `base`, bounded.
fn fixture(dir: &Path, base: &str) {
    let mut child = Command::new("node")
        .arg(repo().join("scripts").join("updater-fixture.mjs"))
        .arg(dir)
        .arg(base)
        .arg(VERSION)
        .current_dir(repo())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("node runs the fixture (needs pnpm install)");
    let mut stderr = child.stderr.take().unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        let _ = done.send((child.wait(), text));
    });
    let (status, text) = finished
        .recv_timeout(FIXTURE_WAIT)
        .expect("the fixture didn't finish in time");
    assert!(status.unwrap().success(), "the fixture failed: {text}");
}

/// Answers one request on `stream` from `served`.
fn answer(mut stream: TcpStream, served: &Served) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head);
    let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
    if path == format!("/{INSTALLER}")
        && let Some(sent) = served.endless.lock().unwrap().take()
    {
        let _ = sent.send(stream_without_end(stream));
        return;
    }
    let body = match path.as_str() {
        "/latest.json" => served.manifest.lock().unwrap().clone(),
        name if name == format!("/{INSTALLER}") => {
            let mut bytes = std::fs::read(served.dir.join(INSTALLER)).unwrap();
            if served.tamper.load(Ordering::SeqCst) {
                *bytes.last_mut().unwrap() ^= 0xff;
            }
            Some(bytes)
        }
        _ => None,
    };
    let reply = match body {
        Some(body) => {
            let mut reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            reply.extend(body);
            reply
        }
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    let _ = stream.write_all(&reply);
}

/// Streams zeros with no length until the client stops reading, or 512 MiB
/// went out; how many bytes it got out.
fn stream_without_end(mut stream: TcpStream) -> u64 {
    const MOST: u64 = 512 * 1024 * 1024;
    let chunk = vec![0_u8; 64 * 1024];
    let size = u64::try_from(chunk.len()).unwrap();
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut sent = 0_u64;
    if stream
        .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return sent;
    }
    while sent < MOST && stream.write_all(&chunk).is_ok() {
        sent += size;
    }
    sent
}

impl Release {
    /// The fixture's release on a fresh loopback server, `/latest.json`
    /// answering its signed manifest, and the app over its public key.
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        fixture(dir.path(), &base);
        let served = Arc::new(Served {
            dir: dir.path().to_path_buf(),
            manifest: Mutex::new(Some(std::fs::read(dir.path().join("latest.json")).unwrap())),
            tamper: AtomicBool::new(false),
            endless: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let serving = Arc::clone(&served);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if serving.stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    let serving = Arc::clone(&serving);
                    std::thread::spawn(move || answer(stream, &serving));
                }
            }
        });
        let pubkey = std::fs::read_to_string(dir.path().join("pubkey")).unwrap();
        let mut context = tauri::generate_context!(test = true);
        let updater = context.config_mut().plugins.0.get_mut("updater").unwrap();
        updater["pubkey"] = json!(pubkey.trim());
        updater["endpoints"] = json!([format!("{base}/latest.json")]);
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap();
        Self {
            served,
            address,
            app,
            _dir: dir,
        }
    }

    /// The real adapter over this app.
    fn updates(&self) -> PluginUpdates<MockRuntime> {
        PluginUpdates::new(self.app.handle().clone())
    }

    /// Serves the fixture's manifest `name` as `/latest.json`, with
    /// `change` applied to its JSON.
    fn serve(&self, name: &str, change: impl FnOnce(&mut Value)) {
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(self.served.dir.join(name)).unwrap()).unwrap();
        change(&mut manifest);
        *self.served.manifest.lock().unwrap() = Some(serde_json::to_vec(&manifest).unwrap());
    }
}

/// A check, bounded.
async fn check(
    updates: &PluginUpdates<MockRuntime>,
) -> Result<Option<tauri_plugin_updater::Update>, UpdateFailure> {
    tokio::time::timeout(WAIT, updates.check()).await.unwrap()
}

/// A check that must find the release, then its download, bounded.
async fn download(updates: &PluginUpdates<MockRuntime>) -> Result<Vec<u8>, UpdateFailure> {
    let found = check(updates)
        .await
        .unwrap()
        .expect("the check found no release");
    tokio::time::timeout(WAIT, updates.download(&found))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_signed_release_is_found_and_downloaded_byte_for_byte() {
    let release = Release::new();
    let updates = release.updates();

    let found = check(&updates).await.unwrap().expect("no release found");
    let bytes = tokio::time::timeout(WAIT, updates.download(&found))
        .await
        .unwrap();

    assert_eq!(PluginUpdates::<MockRuntime>::version(&found), VERSION);
    assert_eq!(
        bytes.unwrap(),
        std::fs::read(release.served.dir.join(INSTALLER)).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_bytes_fail_the_downloads_checks() {
    let release = Release::new();
    release.served.tamper.store(true, Ordering::SeqCst);

    assert_eq!(
        download(&release.updates()).await,
        Err(UpdateFailure::Invalid)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_keys_signature_fails_the_downloads_checks() {
    let release = Release::new();
    release.serve("other.json", |_| {});

    assert_eq!(
        download(&release.updates()).await,
        Err(UpdateFailure::Invalid)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_version_raised_in_the_manifest_after_signing_fails_the_downloads_checks() {
    let release = Release::new();
    release.serve("latest.json", |manifest| {
        manifest["version"] = json!("0.9.1");
    });

    assert_eq!(
        download(&release.updates()).await,
        Err(UpdateFailure::Invalid)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signature_without_a_version_fails_the_downloads_checks() {
    let release = Release::new();
    release.serve("unversioned.json", |_| {});

    assert_eq!(
        download(&release.updates()).await,
        Err(UpdateFailure::Invalid)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_apps_own_version_is_no_update() {
    let release = Release::new();
    release.serve("latest.json", |manifest| {
        manifest["version"] = json!("0.1.0");
    });

    assert!(check(&release.updates()).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manifest_that_isnt_json_cant_be_read() {
    let release = Release::new();
    *release.served.manifest.lock().unwrap() = Some(b"not json".to_vec());

    assert_eq!(
        check(&release.updates()).await.map(|found| found.is_some()),
        Err(UpdateFailure::Manifest)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_manifest_is_unreachable() {
    let release = Release::new();
    *release.served.manifest.lock().unwrap() = None;

    assert_eq!(
        check(&release.updates()).await.map(|found| found.is_some()),
        Err(UpdateFailure::Unreachable)
    );
}

/// The real cap stops a download while it streams: a body with no length
/// that never ends is refused once it passes 64 MiB, and the server sees the
/// client stop reading, so the whole of it is never held (PR #98's review).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cap_stops_a_download_while_it_streams_past_64_mib() {
    let release = Release::new();
    let (sent, got) = mpsc::channel();
    *release.served.endless.lock().unwrap() = Some(sent);
    let updates = release.updates();

    let started = std::time::Instant::now();
    let refused = download(&updates).await;
    let elapsed = started.elapsed();
    let streamed = got
        .recv_timeout(Duration::from_secs(10))
        .expect("the server never saw the client stop reading");

    assert_eq!(refused, Err(UpdateFailure::Invalid));
    assert!(elapsed < WAIT, "it ran until the bound: {elapsed:?}");
    let cap = afkfleet_desktop::update::MAX_DOWNLOAD_BYTES;
    assert!(streamed > cap, "it stopped before the cap: {streamed}");
    assert!(
        streamed < 4 * cap,
        "it read far past the cap before stopping: {streamed}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_download_over_the_cap_is_refused() {
    let release = Release::new();
    let updates = PluginUpdates::with_cap(release.app.handle().clone(), 1024);

    assert_eq!(download(&updates).await, Err(UpdateFailure::Invalid));
}
