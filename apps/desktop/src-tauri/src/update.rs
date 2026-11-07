//! The app's signed updates (Plan.md P12.5; ADR-0022, U1, U27, U28, S24, S27,
//! R20, R21): two commands of the app's own, `check_update` and
//! `install_update`, over tauri-plugin-updater, whose own commands the webview
//! never gets.
//!
//! - **On only where it belongs.** Updates are on in a release build whose
//!   updater points at the release's manifest on GitHub alone, and in a debug
//!   build whose updater points only at this PC's loopback (the rehearsal,
//!   `just update-rehearsal`, R20). Everywhere else (`tauri dev`, a debug
//!   `app-build`, the check build, every test) a check answers `off` and asks
//!   nothing. The decision reads the build's own endpoints, never its
//!   identifier ([`updates_on`]).
//! - **The webview learns versions only:** never a release's notes, date or
//!   address. A check keeps the release it found, replacing an earlier one,
//!   and `install_update` takes no argument: it installs exactly that kept
//!   release, taken out first, so a failure leaves nothing kept.
//! - **Bounded:** a check 20 s (5 s to connect), a download 5 minutes and at
//!   most 64 MiB plus the one chunk that passes it (the app reads the body
//!   itself and stops there, then the plugin checks the signature; PR #99's
//!   review), each also under an outer timeout; the install's start 30 s,
//!   claimed once, so a start the main thread reaches only after that never
//!   runs (S30). One check or install at a time: another call meanwhile is
//!   `update_busy`.
//! - **The session survives the restart.** Before installing, the live events
//!   watch stops, every running command finishes (the connection's write
//!   guard), and fleet-client's refresh in flight saves its rotated token, so
//!   the next start resumes the session with the newest token. The plugin
//!   checks the signature again, starts the installer (passive: a progress
//!   bar, then the app restarts) and the app exits. A failure gives
//!   everything back; one after the watch stopped answers `update_install`,
//!   the one code the UI starts its watch again for (S29).
//! - **A release without a Windows entry:** tauri-plugin-updater compares
//!   the versions first (since 2.14.0), so a release that isn't newer is no
//!   update whatever its platforms, and a newer one without this platform
//!   can't be read (`update_manifest`).
//!
//! The signature is checked against the public key in `tauri.conf.json`, which
//! also binds it to the version (`requireSignedVersion`); the private key
//! never leaves the maintainer's PC (U1). `tests/updater.rs` proves the plugin's
//! checks against a throwaway release; `scripts/tauri-hardening.mjs` pins the
//! configuration.

use core::future::Future;
use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};

use fleet_api_types::BoundedText;
use fleet_api_types::app::{
    AppVersion, CommandError, CommandErrorCode, UpdateCheck, VERSION_MAX_CHARS,
};
use reqwest::header::{ACCEPT, HeaderValue};
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Error, Update, UpdaterExt as _};

use crate::keychain::StoreFactory;
use crate::live::Exclusive;
use crate::state::AppState;

/// Where the released app finds its updates, as `tauri.conf.json` names it:
/// the one endpoint a release build's updates are on for.
pub const RELEASE_ENDPOINT: &str =
    "https://github.com/noah-gabel/afkfleet/releases/latest/download/latest.json";

/// How long a check's request may take.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a check's or a download's connection may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a download may take.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// The largest download: an installer is a few MiB.
pub const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// How long the installer's start may take on the main thread.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a refresh in flight may take to save its token before the
/// install goes on without it.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The outer bounds' margin over the plugin's own timeouts.
const SLACK: Duration = Duration::from_secs(5);

/// Whether updates are on for an app whose updater configuration (its merged
/// config's `plugins.updater`) is `updater`: in a release build whose one
/// endpoint is [`RELEASE_ENDPOINT`], or in a debug build whose endpoints are
/// all plain http to an IPv4 loopback address (the rehearsal's, R20).
/// Anything else is off, the check build's empty list and a configuration
/// that can't be read among them. It reads where a check would go, never the
/// app's identifier, so no build holds the real identifier but in its own
/// configuration, which the check exe's guard relies on (ADR-0020, U87).
#[must_use]
pub fn updates_on(updater: Option<&serde_json::Value>, release_build: bool) -> bool {
    let Some(listed) = updater
        .and_then(|config| config.get("endpoints"))
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let endpoints: Option<Vec<&str>> = listed.iter().map(serde_json::Value::as_str).collect();
    match endpoints.as_deref() {
        None | Some([]) => false,
        Some(endpoints) if release_build => endpoints == [RELEASE_ENDPOINT],
        Some(endpoints) => endpoints.iter().all(|endpoint| loopback_http(endpoint)),
    }
}

/// Whether `endpoint` is plain http to an IPv4 loopback address.
fn loopback_http(endpoint: &str) -> bool {
    tauri::Url::parse(endpoint).is_ok_and(|url| {
        url.scheme() == "http"
            && url
                .host_str()
                .and_then(|host| host.parse::<std::net::Ipv4Addr>().ok())
                .is_some_and(|address| address.is_loopback())
    })
}

/// Why an update step failed. It never carries the plugin's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpdateFailure {
    /// GitHub (or the rehearsal's server) couldn't be reached, or answered
    /// another status, or didn't answer in time.
    #[error("GitHub couldn't be reached")]
    Unreachable,
    /// The manifest can't be read: not JSON, a newer release without a
    /// Windows entry, or a version that isn't one or is too long. A release
    /// that isn't newer is no update, whatever its platforms.
    #[error("the update's manifest can't be read")]
    Manifest,
    /// The download failed its checks: its signature, the version it was
    /// signed for, or its size.
    #[error("the download failed its checks")]
    Invalid,
    /// The install failed after the live watch stopped: the installer
    /// couldn't be written or started, or the plugin's second check of the
    /// downloaded bytes failed.
    #[error("the installer couldn't be written or started")]
    Install,
}

impl UpdateFailure {
    /// The command's error code for this failure.
    #[must_use]
    pub const fn code(self) -> CommandErrorCode {
        match self {
            Self::Unreachable => CommandErrorCode::UpdateUnreachable,
            Self::Manifest => CommandErrorCode::UpdateManifest,
            Self::Invalid => CommandErrorCode::UpdateInvalid,
            Self::Install => CommandErrorCode::UpdateInstall,
        }
    }
}

/// Where updates come from: the plugin in the app, a stand-in in tests.
pub trait Updates: Send + Sync + 'static {
    /// A release a check found, kept until it's installed.
    type Release: Send + Sync + 'static;

    /// `release`'s version, as its manifest says.
    fn version(release: &Self::Release) -> &str;

    /// Asks for the newest release: `None` when it's no newer than this app.
    ///
    /// # Errors
    /// [`UpdateFailure::Unreachable`] or [`UpdateFailure::Manifest`].
    fn check(&self) -> impl Future<Output = Result<Option<Self::Release>, UpdateFailure>> + Send;

    /// Downloads `release` and checks it: its signature by the configured
    /// key, the version it was signed for, its size.
    ///
    /// # Errors
    /// [`UpdateFailure::Unreachable`] or [`UpdateFailure::Invalid`].
    fn download(
        &self,
        release: &Self::Release,
    ) -> impl Future<Output = Result<Vec<u8>, UpdateFailure>> + Send;

    /// Installs `release` from its downloaded `bytes`. In the app, a
    /// successful install starts the installer and exits.
    ///
    /// # Errors
    /// [`UpdateFailure::Install`]; [`Updater::install`] answers
    /// `update_install` for any failure here.
    fn install(
        &self,
        release: Self::Release,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), UpdateFailure>> + Send;
}

/// The two commands' logic over a source of updates; see the
/// [module docs](self).
pub struct Updater<U: Updates> {
    /// Whether updates are on ([`updates_on`]).
    on: bool,
    /// This app's version.
    current: String,
    port: U,
    /// The release the last check found. Its guard is held only to read or
    /// replace it, never across an `.await`.
    stored: Mutex<Option<U::Release>>,
    /// One check or install at a time.
    busy: tokio::sync::Mutex<()>,
}

impl<U: Updates> Updater<U> {
    /// An updater over `port`, on or off, for an app at version `current`.
    #[must_use]
    pub fn new(port: U, on: bool, current: &str) -> Self {
        Self {
            on,
            current: current.to_owned(),
            port,
            stored: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
        }
    }

    /// The kept release's slot. A poisoned lock still holds a whole value:
    /// the guard only ever replaces it.
    fn stored(&self) -> std::sync::MutexGuard<'_, Option<U::Release>> {
        self.stored.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn current(&self) -> BoundedText<VERSION_MAX_CHARS> {
        BoundedText::new(&self.current)
    }

    /// The `app_version` command: this app's own version, at once and with
    /// no network call, never waiting for a check or an install (U37).
    #[must_use]
    pub fn app_version(&self) -> AppVersion {
        AppVersion {
            version: self.current(),
        }
    }

    /// The `check_update` command: this app's version, and the newer
    /// release's, kept for [`Updater::install`].
    ///
    /// # Errors
    /// `update_busy` while another check or install runs;
    /// `update_unreachable` or `update_manifest` for a failed check, which
    /// forgets the kept release.
    pub async fn check(&self) -> Result<UpdateCheck, CommandError> {
        if !self.on {
            return Ok(UpdateCheck::Off {
                current: self.current(),
            });
        }
        let _busy = self.busy.try_lock().map_err(|_| busy())?;
        let found = self.port.check().await.map_err(|failure| {
            *self.stored() = None;
            CommandError::new(failure.code())
        })?;
        let Some(release) = found else {
            *self.stored() = None;
            return Ok(UpdateCheck::UpToDate {
                current: self.current(),
            });
        };
        let version = U::version(&release).to_owned();
        if version.chars().count() > VERSION_MAX_CHARS {
            *self.stored() = None;
            return Err(CommandError::new(CommandErrorCode::UpdateManifest));
        }
        *self.stored() = Some(release);
        Ok(UpdateCheck::Available {
            current: self.current(),
            version: BoundedText::new(&version),
        })
    }

    /// The `install_update` command: downloads the kept release and checks
    /// it, then, once every command on `app`'s connection has finished and
    /// fleet-client's refresh in flight has saved its token, installs it. In
    /// the app a successful install exits; a failure gives the connection
    /// back.
    ///
    /// # Errors
    /// `no_update` when no checked release is kept (or updates are off);
    /// `update_busy` while another check or install runs; the download's
    /// failure as its code, `update_unreachable` or `update_invalid`, with
    /// the live watch still running; `update_install` for any failure after
    /// the watch stopped, so the UI starts it again on that code alone
    /// (ADR-0022, S29).
    pub async fn install<F: StoreFactory>(&self, app: &AppState<F>) -> Result<(), CommandError> {
        if !self.on {
            return Err(CommandError::new(CommandErrorCode::NoUpdate));
        }
        let _busy = self.busy.try_lock().map_err(|_| busy())?;
        let release = self
            .stored()
            .take()
            .ok_or_else(|| CommandError::new(CommandErrorCode::NoUpdate))?;
        let bytes = self
            .port
            .download(&release)
            .await
            .map_err(|failure| CommandError::new(failure.code()))?;
        // As a server switch does: the live watch stops, and every command
        // running on the connection finishes before the write guard is
        // granted; none starts while it's held.
        let switching = app.begin_switch();
        let conn = Exclusive::new(switching, app.conn.write().await);
        if let Ok(client) = conn.client() {
            // A refresh that outlives its bound is lost; the next start then
            // logs in again rather than hang the install.
            let _ = tokio::time::timeout(SETTLE_TIMEOUT, client.settle()).await;
        }
        let installed = self.port.install(release, bytes).await;
        drop(conn);
        installed.map_err(|_| CommandError::new(CommandErrorCode::UpdateInstall))
    }
}

/// Whether it's on and the app's version; never the kept release.
impl<U: Updates> core::fmt::Debug for Updater<U> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Updater")
            .field("on", &self.on)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

/// `update_busy`.
fn busy() -> CommandError {
    CommandError::new(CommandErrorCode::UpdateBusy)
}

/// The real updates: tauri-plugin-updater, registered on `app`, with the
/// configuration's endpoint, public key and `requireSignedVersion`.
pub struct PluginUpdates<R: Runtime> {
    app: AppHandle<R>,
    /// The largest download.
    cap: u64,
    /// Told the length of each chunk of a download's body as it's read.
    on_chunk: Option<ChunkObserver>,
}

/// What a download tells of each chunk of its body: its length.
pub type ChunkObserver = Arc<dyn Fn(usize) + Send + Sync>;

impl<R: Runtime> PluginUpdates<R> {
    /// Over `app`'s registered plugin, downloads capped at
    /// [`MAX_DOWNLOAD_BYTES`].
    #[must_use]
    pub fn new(app: AppHandle<R>) -> Self {
        Self::with_cap(app, MAX_DOWNLOAD_BYTES)
    }

    /// Over `app`'s registered plugin, downloads capped at `cap` bytes; the
    /// update's proof uses a small cap.
    #[must_use]
    pub const fn with_cap(app: AppHandle<R>, cap: u64) -> Self {
        Self {
            app,
            cap,
            on_chunk: None,
        }
    }

    /// The same, telling `on_chunk` each chunk's length as a download reads
    /// it: the cap's proof counts what was read.
    #[must_use]
    pub fn counting(self, on_chunk: ChunkObserver) -> Self {
        Self {
            on_chunk: Some(on_chunk),
            ..self
        }
    }
}

impl<R: Runtime> PluginUpdates<R> {
    /// `release`'s installer, fetched as tauri-plugin-updater 2.14.1's own
    /// `Update::download` fetches it (the manifest's URL, its headers with
    /// `Accept: application/octet-stream`, the plugin's user agent, the
    /// download's timeout, the check's connect timeout, the update's proxy
    /// fields, reqwest's redirects, a failure for any answer but 2xx), but
    /// read under the cap: a `Content-Length` over it is refused before the
    /// body, and the chunk that passes it is the last one read, whatever the
    /// machine's load, with everything so far dropped (PR #99's review). The
    /// plugin's own download reads on while chunks are ready, so its callback
    /// could only signal.
    async fn fetch(&self, release: &Update) -> Result<Vec<u8>, UpdateFailure> {
        let mut headers = release.headers.clone();
        if !headers.contains_key(ACCEPT) {
            headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));
        }
        let mut client = reqwest::Client::builder()
            .user_agent(PLUGIN_USER_AGENT)
            .timeout(DOWNLOAD_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT);
        if release.no_proxy {
            client = client.no_proxy();
        } else if let Some(proxy) = &release.proxy {
            let proxy =
                reqwest::Proxy::all(proxy.as_str()).map_err(|_| UpdateFailure::Unreachable)?;
            client = client.proxy(proxy);
        }
        let client = client.build().map_err(|_| UpdateFailure::Unreachable)?;
        // Bounded by the client's own timeout, as every read below is.
        let mut response = client
            .get(release.download_url.clone())
            .headers(headers)
            .send()
            .await
            .map_err(|_| UpdateFailure::Unreachable)?;
        if !response.status().is_success() {
            return Err(UpdateFailure::Unreachable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.cap)
        {
            return Err(UpdateFailure::Invalid);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| UpdateFailure::Unreachable)?
        {
            if let Some(observer) = &self.on_chunk {
                observer(chunk.len());
            }
            let held = u64::try_from(bytes.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
            if held > self.cap {
                // Dropping the response closes its connection; the bytes so
                // far go with `bytes`.
                return Err(UpdateFailure::Invalid);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

/// The user agent tauri-plugin-updater 2.14.1 sends with its own download
/// (`UPDATER_USER_AGENT`): the app's fetch is the plugin's request, read
/// under the cap. A test holds it equal to Cargo.lock's version.
pub const PLUGIN_USER_AGENT: &str = "tauri-plugin-updater/2.14.1";

/// Only the cap.
impl<R: Runtime> core::fmt::Debug for PluginUpdates<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PluginUpdates")
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

impl<R: Runtime> Updates for PluginUpdates<R> {
    type Release = Update;

    fn version(release: &Update) -> &str {
        &release.version
    }

    async fn check(&self) -> Result<Option<Update>, UpdateFailure> {
        let updater = self
            .app
            .updater_builder()
            .timeout(CHECK_TIMEOUT)
            .configure_client(|client| client.connect_timeout(CONNECT_TIMEOUT))
            .build()
            .map_err(|error| check_failure(&error))?;
        tokio::time::timeout(CHECK_TIMEOUT + SLACK, updater.check())
            .await
            .map_err(|_| UpdateFailure::Unreachable)?
            .map_err(|error| check_failure(&error))
    }

    async fn download(&self, release: &Update) -> Result<Vec<u8>, UpdateFailure> {
        let bytes = tokio::time::timeout(DOWNLOAD_TIMEOUT + SLACK, self.fetch(release))
            .await
            .map_err(|_| UpdateFailure::Unreachable)??;
        // The plugin's own signature check, where its download runs it: the
        // committed key, bound to the version. `install` checks it again.
        release.verify(&bytes).map_err(|_| UpdateFailure::Invalid)?;
        Ok(bytes)
    }

    async fn install(&self, release: Update, bytes: Vec<u8>) -> Result<(), UpdateFailure> {
        // On the main thread, as Windows' shell calls want (ADR-0018's
        // opener runs there too). On success the plugin starts the installer
        // and the app exits.
        // Any failure here comes after the live watch stopped, so it's
        // `Install` alone: the plugin's second check of the same bytes too
        // (S29).
        on_main_thread(
            |job| self.app.run_on_main_thread(job).is_ok(),
            move || release.install(&bytes).map_err(|_| UpdateFailure::Install),
            INSTALL_TIMEOUT,
        )
        .await
    }
}

/// A job for the main thread.
type MainJob = Box<dyn FnOnce() + Send + 'static>;

/// Runs `work` on the main thread through `dispatch`, which answers whether
/// the main thread took the job, and waits at most `bound` for the main
/// thread to start it (ADR-0022, S30).
///
/// The work is claimed once: by the main thread to run it, or by the bound
/// to drop it, whichever comes first. A job the main thread reaches only
/// after the bound finds nothing to run, so an install the UI was told
/// failed can't exit the app later. A job the main thread took in time is
/// waited for to its end, however long it runs.
async fn on_main_thread<D, W>(dispatch: D, work: W, bound: Duration) -> Result<(), UpdateFailure>
where
    D: FnOnce(MainJob) -> bool,
    W: FnOnce() -> Result<(), UpdateFailure> + Send + 'static,
{
    let claim = Arc::new(Mutex::new(Some(work)));
    let taken = Arc::clone(&claim);
    let (sent, mut answered) = tokio::sync::oneshot::channel();
    let job: MainJob = Box::new(move || {
        if let Some(work) = take(&taken) {
            let _ = sent.send(work());
        }
    });
    if !dispatch(job) {
        return Err(UpdateFailure::Install);
    }
    if let Ok(answer) = tokio::time::timeout(bound, &mut answered).await {
        return answer.unwrap_or(Err(UpdateFailure::Install));
    }
    if take(&claim).is_some() {
        // The main thread never reached it: dropped, never run.
        return Err(UpdateFailure::Install);
    }
    // No timeout: the main thread took the work in time and is installing.
    // Its answer comes once the plugin returns (on success the app exits
    // first); answering sooner would give the connection back while the
    // installer can still exit the app.
    answered.await.unwrap_or(Err(UpdateFailure::Install))
}

/// Takes the work out of `claim`. A poisoned lock still holds a whole value:
/// the guard is held only to take it.
fn take<T>(claim: &Mutex<Option<T>>) -> Option<T> {
    claim.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// A failed check: no answer, or one that can't be read. The plugin reads the
/// manifest with reqwest's JSON decoder, so a body that isn't JSON comes back
/// as reqwest's decode error.
fn check_failure(error: &Error) -> UpdateFailure {
    match error {
        Error::Reqwest(failure) if failure.is_decode() => UpdateFailure::Manifest,
        Error::Reqwest(_) | Error::Network(_) | Error::Http(_) | Error::ReleaseNotFound => {
            UpdateFailure::Unreachable
        }
        _ => UpdateFailure::Manifest,
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    use fleet_api_types::app::{ServerProfileRequest, ServerProfileResponse};
    use fleet_client::{
        Client, ClientConfig, LiveStop, MemoryStore, ServerUrl, SessionStore as _, StoreError,
        Trust, VerifiedLink,
    };
    use rstest::rstest;
    use secrecy::{ExposeSecret, SecretString};
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Notify;

    use super::*;
    use crate::keychain::KeychainFactory;
    use crate::profile::PROFILE_FILE;
    use crate::state::{LinkOpener, OpenError};

    /// How long a step over a loopback socket may take here.
    const WAIT: Duration = Duration::from_secs(10);

    /// A release the stand-in found: its version.
    struct Found(String);

    /// The stand-in for the plugin: answers what the test scripted, and
    /// counts and records its calls.
    struct StandIn {
        found: Mutex<Result<Option<String>, UpdateFailure>>,
        downloaded: Mutex<Result<(), UpdateFailure>>,
        installed: Mutex<Result<(), UpdateFailure>>,
        /// When set, a check waits for it before answering.
        gate: Mutex<Option<Arc<Notify>>>,
        checks: AtomicUsize,
        downloads: AtomicUsize,
        installs: Mutex<Vec<String>>,
    }

    impl Default for StandIn {
        fn default() -> Self {
            Self {
                found: Mutex::new(Ok(None)),
                downloaded: Mutex::new(Ok(())),
                installed: Mutex::new(Ok(())),
                gate: Mutex::new(None),
                checks: AtomicUsize::new(0),
                downloads: AtomicUsize::new(0),
                installs: Mutex::new(Vec::new()),
            }
        }
    }

    impl StandIn {
        fn finds(&self, found: Result<Option<&str>, UpdateFailure>) {
            *self.found.lock().unwrap() = found.map(|version| version.map(str::to_owned));
        }
    }

    impl Updates for StandIn {
        type Release = Found;

        fn version(release: &Found) -> &str {
            &release.0
        }

        async fn check(&self) -> Result<Option<Found>, UpdateFailure> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            let gate = self.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            self.found
                .lock()
                .unwrap()
                .clone()
                .map(|found| found.map(Found))
        }

        fn download(
            &self,
            _release: &Found,
        ) -> impl Future<Output = Result<Vec<u8>, UpdateFailure>> + Send {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            core::future::ready((*self.downloaded.lock().unwrap()).map(|()| b"setup".to_vec()))
        }

        fn install(
            &self,
            release: Found,
            bytes: Vec<u8>,
        ) -> impl Future<Output = Result<(), UpdateFailure>> + Send {
            assert_eq!(bytes, b"setup");
            self.installs.lock().unwrap().push(release.0);
            core::future::ready(*self.installed.lock().unwrap())
        }
    }

    /// An opener the update never uses.
    struct NeverOpens;

    impl LinkOpener for NeverOpens {
        fn open(&self, _link: &VerifiedLink) -> Result<(), OpenError> {
            panic!("an update opened a link")
        }
    }

    /// The app's state with no server set up, over keyring-core's mock store.
    fn app(dir: &TempDir) -> AppState<KeychainFactory> {
        let factory =
            KeychainFactory::new(keyring_core::mock::Store::new().unwrap(), "io.example.test");
        AppState::load(dir.path().join(PROFILE_FILE), factory, Arc::new(NeverOpens))
    }

    fn on(stand_in: StandIn) -> Updater<StandIn> {
        Updater::new(stand_in, true, "0.1.0")
    }

    fn code<T: core::fmt::Debug>(result: Result<T, CommandError>) -> CommandErrorCode {
        result.unwrap_err().code
    }

    fn text(version: &str) -> BoundedText<{ fleet_api_types::app::VERSION_MAX_CHARS }> {
        BoundedText::new(version)
    }

    /// The rehearsal's endpoint (`scripts/update-rehearsal.mjs`'s overlay).
    const REHEARSAL: &str = "http://127.0.0.1:47123/latest.json";

    /// An updater configuration with `endpoints`.
    fn updater_with(endpoints: &[&str]) -> Value {
        json!({ "endpoints": endpoints })
    }

    #[rstest]
    #[case::the_release(&[RELEASE_ENDPOINT], true, true)]
    #[case::a_debug_build_of_the_app(&[RELEASE_ENDPOINT], false, false)]
    #[case::the_debug_rehearsal(&[REHEARSAL], false, true)]
    #[case::a_released_rehearsal(&[REHEARSAL], true, false)]
    #[case::the_check_build(&[], false, false)]
    #[case::a_released_check_build(&[], true, false)]
    #[case::a_release_from_elsewhere(&["https://example.com/latest.json"], true, false)]
    #[case::a_debug_build_reaching_another_host(&["http://192.0.2.10/latest.json"], false, false)]
    #[case::a_debug_build_by_name(&["http://localhost:47123/latest.json"], false, false)]
    #[case::a_debug_build_over_https(&["https://127.0.0.1:47123/latest.json"], false, false)]
    #[case::a_debug_build_with_the_release_too(&[REHEARSAL, RELEASE_ENDPOINT], false, false)]
    #[case::the_release_twice(&[RELEASE_ENDPOINT, RELEASE_ENDPOINT], true, false)]
    fn updates_are_on_only_for_the_release_or_the_debug_rehearsal(
        #[case] endpoints: &[&str],
        #[case] release_build: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(
            updates_on(Some(&updater_with(endpoints)), release_build),
            expected
        );
    }

    #[rstest]
    #[case::no_updater_config(None)]
    #[case::no_endpoints(Some(json!({})))]
    #[case::endpoints_that_arent_a_list(Some(json!({ "endpoints": REHEARSAL })))]
    #[case::an_endpoint_that_isnt_text(Some(json!({ "endpoints": [7] })))]
    #[case::the_release_beside_one_that_isnt_text(Some(json!({ "endpoints": [RELEASE_ENDPOINT, 7] })))]
    #[case::the_rehearsal_beside_one_that_isnt_text(Some(json!({ "endpoints": [REHEARSAL, 7] })))]
    #[case::an_endpoint_that_isnt_a_url(Some(json!({ "endpoints": ["not a url"] })))]
    fn a_config_it_cant_read_is_off_in_every_build(#[case] updater: Option<Value>) {
        for release_build in [true, false] {
            assert!(
                !updates_on(updater.as_ref(), release_build),
                "{release_build}"
            );
        }
    }

    /// The committed configuration, so the constant can't drift from it: on in
    /// a release build alone, and off in both once the check build's overlay
    /// is laid over it.
    #[test]
    fn the_committed_config_is_on_in_a_release_alone_and_off_for_the_check_build() {
        let base: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let check: Value = serde_json::from_str(include_str!("../tauri.check.conf.json")).unwrap();
        let updater = &base["plugins"]["updater"];
        let mut checked = updater.clone();
        for (key, value) in check["plugins"]["updater"].as_object().unwrap() {
            checked[key] = value.clone();
        }

        assert!(updates_on(Some(updater), true));
        assert!(!updates_on(Some(updater), false));
        assert!(!updates_on(Some(&checked), true));
        assert!(!updates_on(Some(&checked), false));
    }

    #[rstest]
    #[case::unreachable(UpdateFailure::Unreachable, CommandErrorCode::UpdateUnreachable)]
    #[case::manifest(UpdateFailure::Manifest, CommandErrorCode::UpdateManifest)]
    #[case::invalid(UpdateFailure::Invalid, CommandErrorCode::UpdateInvalid)]
    #[case::install(UpdateFailure::Install, CommandErrorCode::UpdateInstall)]
    fn each_failure_is_its_own_code(
        #[case] failure: UpdateFailure,
        #[case] expected: CommandErrorCode,
    ) {
        assert_eq!(failure.code(), expected);
    }

    #[tokio::test]
    async fn an_off_build_answers_off_and_never_asks() {
        let dir = TempDir::new().unwrap();
        let updater = Updater::new(StandIn::default(), false, "0.1.0");

        let checked = updater.check().await.unwrap();
        let installed = updater.install(&app(&dir)).await;

        assert_eq!(
            checked,
            UpdateCheck::Off {
                current: text("0.1.0")
            }
        );
        assert_eq!(code(installed), CommandErrorCode::NoUpdate);
        assert_eq!(updater.port.checks.load(Ordering::SeqCst), 0);
        assert_eq!(updater.port.downloads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_check_answers_up_to_date_or_the_newer_version() {
        let updater = on(StandIn::default());

        let none = updater.check().await.unwrap();
        updater.port.finds(Ok(Some("0.2.0")));
        let found = updater.check().await.unwrap();

        assert_eq!(
            none,
            UpdateCheck::UpToDate {
                current: text("0.1.0")
            }
        );
        assert_eq!(
            found,
            UpdateCheck::Available {
                current: text("0.1.0"),
                version: text("0.2.0"),
            }
        );
    }

    #[tokio::test]
    async fn a_failed_check_answers_its_code() {
        let updater = on(StandIn::default());

        updater.port.finds(Err(UpdateFailure::Unreachable));
        let unreachable = updater.check().await;
        updater.port.finds(Err(UpdateFailure::Manifest));
        let manifest = updater.check().await;

        assert_eq!(code(unreachable), CommandErrorCode::UpdateUnreachable);
        assert_eq!(code(manifest), CommandErrorCode::UpdateManifest);
    }

    #[tokio::test]
    async fn a_version_over_the_limit_is_a_manifest_error_and_nothing_is_kept() {
        let dir = TempDir::new().unwrap();
        let updater = on(StandIn::default());
        let long = "1".repeat(65);
        updater.port.finds(Ok(Some(&long)));

        let checked = updater.check().await;
        let installed = updater.install(&app(&dir)).await;

        assert_eq!(code(checked), CommandErrorCode::UpdateManifest);
        assert_eq!(code(installed), CommandErrorCode::NoUpdate);
        assert_eq!(updater.port.downloads.load(Ordering::SeqCst), 0);
    }

    /// The download sends the plugin's own user agent: the version Cargo.lock
    /// pins, so the fetch stays the plugin's request after a bump.
    #[test]
    fn the_downloads_user_agent_is_the_locked_plugins() {
        let lock = include_str!("../../../../Cargo.lock");
        let at = lock.find("name = \"tauri-plugin-updater\"\n").unwrap();
        let version = lock
            .get(at..)
            .and_then(|rest| rest.lines().nth(1))
            .and_then(|line| line.strip_prefix("version = \""))
            .and_then(|line| line.strip_suffix('"'))
            .unwrap();

        assert_eq!(
            super::PLUGIN_USER_AGENT,
            format!("tauri-plugin-updater/{version}")
        );
    }

    #[tokio::test]
    async fn install_installs_the_checked_release_once() {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let updater = on(StandIn::default());
        updater.port.finds(Ok(Some("0.2.0")));
        updater.check().await.unwrap();

        let first = updater.install(&app).await;
        let second = updater.install(&app).await;

        assert_eq!(first, Ok(()));
        assert_eq!(code(second), CommandErrorCode::NoUpdate);
        assert_eq!(*updater.port.installs.lock().unwrap(), ["0.2.0"]);
    }

    #[tokio::test]
    async fn a_newer_check_replaces_the_kept_release_and_one_finding_none_forgets_it() {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let updater = on(StandIn::default());

        updater.port.finds(Ok(Some("0.2.0")));
        updater.check().await.unwrap();
        updater.port.finds(Ok(Some("0.3.0")));
        updater.check().await.unwrap();
        updater.install(&app).await.unwrap();
        updater.port.finds(Ok(Some("0.4.0")));
        updater.check().await.unwrap();
        updater.port.finds(Ok(None));
        updater.check().await.unwrap();
        let forgotten = updater.install(&app).await;

        assert_eq!(*updater.port.installs.lock().unwrap(), ["0.3.0"]);
        assert_eq!(code(forgotten), CommandErrorCode::NoUpdate);
    }

    #[tokio::test]
    async fn a_failed_download_installs_nothing_and_forgets_the_release() {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let updater = on(StandIn::default());
        updater.port.finds(Ok(Some("0.2.0")));
        updater.check().await.unwrap();
        *updater.port.downloaded.lock().unwrap() = Err(UpdateFailure::Invalid);

        let failed = updater.install(&app).await;
        let again = updater.install(&app).await;

        assert_eq!(code(failed), CommandErrorCode::UpdateInvalid);
        assert_eq!(code(again), CommandErrorCode::NoUpdate);
        assert!(updater.port.installs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_install_answers_its_code_and_frees_the_connection() {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let updater = on(StandIn::default());
        updater.port.finds(Ok(Some("0.2.0")));
        updater.check().await.unwrap();
        *updater.port.installed.lock().unwrap() = Err(UpdateFailure::Install);

        let failed = updater.install(&app).await;

        assert_eq!(code(failed), CommandErrorCode::UpdateInstall);
        assert!(app.conn.try_read().is_ok(), "the connection stayed locked");
    }

    #[tokio::test]
    async fn install_waits_for_a_running_command_before_it_installs() {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let updater = on(StandIn::default());
        updater.port.finds(Ok(Some("0.2.0")));
        updater.check().await.unwrap();
        let running = app.conn.read().await;

        let mut installing = pin!(updater.install(&app));
        let mut context = Context::from_waker(Waker::noop());
        let first = installing.as_mut().poll(&mut context);
        let before = updater.port.installs.lock().unwrap().len();
        drop(running);
        let installed = installing.await;

        assert!(
            matches!(first, Poll::Pending),
            "it didn't wait for the command"
        );
        assert_eq!(before, 0);
        assert_eq!(installed, Ok(()));
        assert_eq!(*updater.port.installs.lock().unwrap(), ["0.2.0"]);
    }

    #[tokio::test]
    async fn a_call_while_another_runs_is_busy() {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let updater = on(StandIn::default());
        let gate = Arc::new(Notify::new());
        *updater.port.gate.lock().unwrap() = Some(Arc::clone(&gate));
        updater.port.finds(Ok(Some("0.2.0")));

        let mut checking = pin!(updater.check());
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            checking.as_mut().poll(&mut context),
            Poll::Pending
        ));
        let second = updater.check().await;
        let install = updater.install(&app).await;
        gate.notify_one();
        let first = checking.await;

        assert_eq!(code(second), CommandErrorCode::UpdateBusy);
        assert_eq!(code(install), CommandErrorCode::UpdateBusy);
        assert!(first.is_ok(), "{first:?}");
        assert_eq!(updater.port.checks.load(Ordering::SeqCst), 1);
    }

    /// The app's version comes from the app itself: on or off, at once, even
    /// while a check holds the one-at-a-time lock, and never asking (U37).
    #[tokio::test]
    async fn the_apps_version_answers_at_once_on_or_off_even_while_a_check_runs() {
        let off = Updater::new(StandIn::default(), false, "0.1.0");
        let updater = on(StandIn::default());
        let gate = Arc::new(Notify::new());
        *updater.port.gate.lock().unwrap() = Some(Arc::clone(&gate));

        let mut checking = pin!(updater.check());
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            checking.as_mut().poll(&mut context),
            Poll::Pending
        ));
        let during = updater.app_version();
        gate.notify_one();
        let _ = checking.await;

        assert_eq!(during.version.as_str(), "0.1.0");
        assert_eq!(off.app_version().version.as_str(), "0.1.0");
        assert_eq!(off.port.checks.load(Ordering::SeqCst), 0);
        assert_eq!(updater.port.checks.load(Ordering::SeqCst), 1);
    }

    /// The stop of a live events watch nobody drives.
    fn live_stop() -> LiveStop {
        let client = Client::new(
            ClientConfig {
                server: ServerUrl::parse("https://example.com").unwrap(),
                trust: Trust::System,
            },
            MemoryStore::new(),
        )
        .unwrap();
        client.live_events().stopper()
    }

    /// `app` with a running watch: its stop.
    fn watching<F: StoreFactory>(app: &AppState<F>) -> LiveStop {
        let watch = live_stop();
        let _ = app.live.lock().unwrap().register(watch.clone());
        watch
    }

    /// An updater on that found 0.2.0, kept for the install.
    async fn found() -> Updater<StandIn> {
        let updater = on(StandIn::default());
        updater.port.finds(Ok(Some("0.2.0")));
        updater.check().await.unwrap();
        updater
    }

    /// Rust stops the live events watch only once the download is checked,
    /// and every failure after that answers `update_install`: the one code
    /// the UI starts its watch again for (ADR-0022, S29).
    #[rstest]
    #[case::the_second_signature_check(UpdateFailure::Invalid)]
    #[case::the_installer(UpdateFailure::Install)]
    #[tokio::test]
    async fn an_install_that_fails_after_the_watch_stopped_answers_update_install(
        #[case] failure: UpdateFailure,
    ) {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let watch = watching(&app);
        let updater = found().await;
        *updater.port.installed.lock().unwrap() = Err(failure);

        let failed = updater.install(&app).await;

        assert!(watch.is_stopped(), "the watch kept running");
        assert_eq!(code(failed), CommandErrorCode::UpdateInstall);
    }

    #[rstest]
    #[case::unreachable(UpdateFailure::Unreachable, CommandErrorCode::UpdateUnreachable)]
    #[case::invalid(UpdateFailure::Invalid, CommandErrorCode::UpdateInvalid)]
    #[tokio::test]
    async fn a_failed_download_leaves_the_watch_running(
        #[case] failure: UpdateFailure,
        #[case] expected: CommandErrorCode,
    ) {
        let dir = TempDir::new().unwrap();
        let app = app(&dir);
        let watch = watching(&app);
        let updater = found().await;
        *updater.port.downloaded.lock().unwrap() = Err(failure);

        let failed = updater.install(&app).await;

        assert!(!watch.is_stopped(), "a failed download stopped the watch");
        assert_eq!(code(failed), expected);
    }

    /// Every client of the app shares one store, so a test can put a refresh
    /// token where the switched-to client finds it.
    struct Shared(MemoryStore);

    impl StoreFactory for Shared {
        type Store = MemoryStore;

        fn for_origin(&self, _origin: &ServerUrl) -> Result<MemoryStore, StoreError> {
            Ok(self.0.clone())
        }
    }

    /// A loopback stand-in for the server: `/health/live` answers at once;
    /// `POST /api/v1/auth/refresh` says it arrived, then answers a new pair
    /// once released; anything else is a 404. One request per connection.
    struct Server {
        url: String,
        refresh_arrived: Arc<Notify>,
        release: Arc<Notify>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl Server {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let (refresh_arrived, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
            let (arrived, released) = (Arc::clone(&refresh_arrived), Arc::clone(&release));
            let task = tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let request = read_request(&socket).await;
                    let answer = if request.starts_with("GET /health/live ") {
                        reply("200 OK", "")
                    } else if request.starts_with("POST /api/v1/auth/refresh ") {
                        arrived.notify_one();
                        released.notified().await;
                        reply(
                            "200 OK",
                            r#"{"access_token":"afk_at_1","refresh_token":"afk_rt_1","access_expires_in":900,"refresh_expires_in":604800}"#,
                        )
                    } else {
                        reply("404 Not Found", "")
                    };
                    write_all(&socket, &answer).await;
                }
            });
            Self {
                url,
                refresh_arrived,
                release,
                task,
            }
        }
    }

    /// An HTTP/1.1 answer with `status` and the JSON `body`, closing the
    /// connection.
    fn reply(status: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// One request off `socket`, its body read to its length, as text.
    async fn read_request(socket: &TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut total = None;
        while total.is_none_or(|total| bytes.len() < total) {
            if total.is_none()
                && let Some(end) = bytes.windows(4).position(|four| four == b"\r\n\r\n")
            {
                let head = String::from_utf8_lossy(bytes.get(..end).unwrap()).to_lowercase();
                let body = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map_or(0, |length| length.trim().parse::<usize>().unwrap());
                total = Some(end + 4 + body);
                continue;
            }
            socket.readable().await.unwrap();
            let mut chunk = [0_u8; 1024];
            match socket.try_read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => bytes.extend_from_slice(chunk.get(..read).unwrap()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Writes all of `bytes` to `socket`.
    async fn write_all(socket: &TcpStream, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            socket.writable().await.unwrap();
            match socket.try_write(bytes) {
                Ok(written) => bytes = bytes.get(written..).unwrap(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
        }
    }

    /// On a live session the install stops the live events watch, then waits
    /// for fleet-client's refresh in flight to save its rotated token before
    /// it installs, so the next start resumes with the newest token
    /// (ADR-0022, S27; PR #98's review).
    #[tokio::test]
    async fn an_install_on_a_live_session_stops_its_watch_and_waits_for_the_refresh_in_flight() {
        let dir = TempDir::new().unwrap();
        let store = MemoryStore::new();
        let app = AppState::load(
            dir.path().join(PROFILE_FILE),
            Shared(store.clone()),
            Arc::new(NeverOpens),
        );
        let server = Server::start().await;
        let switched = tokio::time::timeout(
            WAIT,
            app.set_server_profile(ServerProfileRequest {
                url: server.url.clone(),
                ca_pem: None,
            }),
        )
        .await
        .unwrap();
        assert!(
            matches!(switched, Ok(ServerProfileResponse::Switched { .. })),
            "{switched:?}"
        );
        store.save(&SecretString::from("afk_rt_0")).await.unwrap();
        let watch = watching(&app);
        // With no access token, a call refreshes first, and the refresh is in
        // flight until the server answers.
        let client = app.conn.read().await.client().unwrap().clone();
        let calling = tokio::spawn(async move { client.me().await.is_ok() });
        tokio::time::timeout(WAIT, server.refresh_arrived.notified())
            .await
            .unwrap();
        let updater = found().await;

        let mut installing = pin!(updater.install(&app));
        let mut context = Context::from_waker(Waker::noop());
        let first = installing.as_mut().poll(&mut context);
        let stopped = watch.is_stopped();
        let before = updater.port.installs.lock().unwrap().len();
        server.release.notify_one();
        // Before the await: a finished future can't be polled again.
        assert!(
            matches!(first, Poll::Pending),
            "it didn't wait for the refresh"
        );
        let installed = tokio::time::timeout(WAIT, installing).await.unwrap();
        let stored = store.load().await.unwrap();

        assert!(stopped, "the watch kept running");
        assert_eq!(before, 0);
        assert_eq!(installed, Ok(()));
        assert_eq!(*updater.port.installs.lock().unwrap(), ["0.2.0"]);
        assert_eq!(
            stored.as_ref().map(ExposeSecret::expose_secret),
            Some("afk_rt_1")
        );
        let _ = tokio::time::timeout(WAIT, calling).await.unwrap();
    }

    /// A stand-in main thread: it keeps the job it's given, to run it when
    /// the test says.
    #[derive(Default)]
    struct HeldMain(Mutex<Option<MainJob>>);

    impl HeldMain {
        fn hold(&self, job: MainJob) -> bool {
            *self.0.lock().unwrap() = Some(job);
            true
        }
    }

    /// Counts the installs it's asked for.
    fn counting(
        installs: &Arc<AtomicUsize>,
    ) -> impl FnOnce() -> Result<(), UpdateFailure> + Send + use<> {
        let installs = Arc::clone(installs);
        move || {
            installs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The bound gives up on a start the main thread hasn't reached, and that
    /// start never installs when the main thread reaches it later, so a
    /// failure the UI was told about can't exit the app afterwards
    /// (ADR-0022, S30).
    #[tokio::test(start_paused = true)]
    async fn an_install_the_main_thread_takes_after_its_bound_never_runs() {
        let main = HeldMain::default();
        let installs = Arc::new(AtomicUsize::new(0));

        let answer =
            on_main_thread(|job| main.hold(job), counting(&installs), INSTALL_TIMEOUT).await;
        let late = main.0.lock().unwrap().take().unwrap();
        late();

        assert_eq!(answer, Err(UpdateFailure::Install));
        assert_eq!(
            installs.load(Ordering::SeqCst),
            0,
            "the late start installed"
        );
    }

    /// A start the main thread took in time is waited for past the bound: it
    /// may still exit the app, so the connection isn't given back before it
    /// answers (S30).
    #[tokio::test(start_paused = true)]
    async fn an_install_the_main_thread_took_in_time_is_waited_for_past_its_bound() {
        let (entering, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let thread = Mutex::new(None);

        let mut installing = pin!(on_main_thread(
            |job| {
                *thread.lock().unwrap() = Some(std::thread::spawn(job));
                // Back once the main thread runs it, so it took the start.
                entered.recv_timeout(WAIT).unwrap();
                true
            },
            move || {
                entering.send(()).unwrap();
                released.recv_timeout(WAIT).unwrap();
                Ok(())
            },
            INSTALL_TIMEOUT,
        ));
        let mut context = Context::from_waker(Waker::noop());
        let started = installing.as_mut().poll(&mut context);
        tokio::time::advance(INSTALL_TIMEOUT + Duration::from_secs(1)).await;
        let past = installing.as_mut().poll(&mut context);
        release.send(()).unwrap();
        // Before the await: a finished future can't be polled again.
        assert!(started.is_pending());
        assert!(
            past.is_pending(),
            "it gave up on a start the main thread runs"
        );
        // No tokio timeout: paused time would jump past it while the thread
        // answers. The thread's own real-time bound ends the wait instead (its
        // panic drops the answer's sender).
        let answer = installing.await;
        thread.lock().unwrap().take().unwrap().join().unwrap();

        assert_eq!(answer, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn an_install_answered_in_time_is_its_answer() {
        let installs = Arc::new(AtomicUsize::new(0));
        let run = |job: MainJob| {
            job();
            true
        };

        let installed = on_main_thread(run, counting(&installs), INSTALL_TIMEOUT).await;
        let failed = on_main_thread(run, || Err(UpdateFailure::Install), INSTALL_TIMEOUT).await;

        assert_eq!(installed, Ok(()));
        assert_eq!(failed, Err(UpdateFailure::Install));
        assert_eq!(installs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_start_the_main_thread_refuses_is_an_install_failure() {
        let installs = Arc::new(AtomicUsize::new(0));

        let answer = on_main_thread(|_job| false, counting(&installs), INSTALL_TIMEOUT).await;

        assert_eq!(answer, Err(UpdateFailure::Install));
        assert_eq!(installs.load(Ordering::SeqCst), 0);
    }
}
