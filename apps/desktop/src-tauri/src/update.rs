//! The app's signed updates (Plan.md P12.5; ADR-0022, U1, U27, U28, S24, S27,
//! R20, R21): two commands of the app's own, `check_update` and
//! `install_update`, over tauri-plugin-updater, whose own commands the webview
//! never gets.
//!
//! - **On only where it belongs.** Updates are on in a release build under
//!   the released app's identifier, and in a debug build under the
//!   rehearsal's (`just update-rehearsal`, R20). Everywhere else (`tauri dev`,
//!   the check build, every test) a check answers `off` and asks nothing.
//! - **The webview learns versions only:** never a release's notes, date or
//!   address. A check keeps the release it found, replacing an earlier one,
//!   and `install_update` takes no argument: it installs exactly that kept
//!   release, taken out first, so a failure leaves nothing kept.
//! - **Bounded:** a check 20 s (5 s to connect), a download 5 minutes and at
//!   most 64 MiB, each also under an outer timeout; the install's start 30 s.
//!   One check or install at a time: another call meanwhile is
//!   `update_busy`.
//! - **The session survives the restart.** Before installing, the live events
//!   watch stops, every running command finishes (the connection's write
//!   guard), and fleet-client's refresh in flight saves its rotated token, so
//!   the next start resumes the session with the newest token. The plugin
//!   checks the signature again, starts the installer (passive: a progress
//!   bar, then the app restarts) and the app exits. A failure gives
//!   everything back, and the UI restarts its watch.
//!
//! The signature is checked against the public key in `tauri.conf.json`, which
//! also binds it to the version (`requireSignedVersion`); the private key
//! never leaves the maintainer's PC (U1). `tests/updater.rs` proves the plugin's
//! checks against a throwaway release; `scripts/tauri-hardening.mjs` pins the
//! configuration.

use core::future::Future;
use core::time::Duration;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use fleet_api_types::BoundedText;
use fleet_api_types::app::{CommandError, CommandErrorCode, UpdateCheck, VERSION_MAX_CHARS};
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Error, Update, UpdaterExt as _};

use crate::keychain::StoreFactory;
use crate::live::Exclusive;
use crate::state::AppState;

/// The released app's identifier: the one build whose updates are on.
pub const RELEASE_IDENTIFIER: &str = "io.github.noah-gabel.afkfleet";

/// The rehearsal's identifier: its debug builds update from a loopback
/// server with a throwaway key (`just update-rehearsal`, R20).
pub const REHEARSAL_IDENTIFIER: &str = "io.github.noah-gabel.afkfleet.rehearsal";

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

/// Whether updates are on for an app with `identifier`: a release build of
/// the released app, or a debug build of the rehearsal.
#[must_use]
pub fn updates_on(identifier: &str, release_build: bool) -> bool {
    if release_build {
        identifier == RELEASE_IDENTIFIER
    } else {
        identifier == REHEARSAL_IDENTIFIER
    }
}

/// Why an update step failed. It never carries the plugin's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpdateFailure {
    /// GitHub (or the rehearsal's server) couldn't be reached, or answered
    /// another status, or didn't answer in time.
    #[error("GitHub couldn't be reached")]
    Unreachable,
    /// The manifest can't be read: not JSON, no Windows release, or a version
    /// that isn't one or is too long.
    #[error("the update's manifest can't be read")]
    Manifest,
    /// The download failed its checks: its signature, the version it was
    /// signed for, or its size.
    #[error("the download failed its checks")]
    Invalid,
    /// The installer couldn't be written or started.
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
    /// [`UpdateFailure::Invalid`] when the bytes fail their checks again, or
    /// [`UpdateFailure::Install`].
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
    /// `update_busy` while another check or install runs; the download's or
    /// the install's failure as its code.
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
        installed.map_err(|failure| CommandError::new(failure.code()))
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
}

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
        Self { app, cap }
    }
}

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
        // The plugin gives each found update no timeout of its own: the
        // download sets one.
        let mut update = release.clone();
        update.timeout = Some(DOWNLOAD_TIMEOUT);
        let cap = self.cap;
        let total = Arc::new(AtomicU64::new(0));
        let over = Arc::new(tokio::sync::Notify::new());
        let (counted, signal) = (Arc::clone(&total), Arc::clone(&over));
        let downloading = update.download(
            move |chunk, length| {
                let chunk = u64::try_from(chunk).unwrap_or(u64::MAX);
                let sum = counted
                    .fetch_add(chunk, Ordering::Relaxed)
                    .saturating_add(chunk);
                if sum > cap || length.is_some_and(|length| length > cap) {
                    signal.notify_one();
                }
            },
            || {},
        );
        let bounded = async {
            tokio::select! {
                // Cancel-safe for what matters: dropping the download drops
                // its request and the bytes so far, and nothing is installed.
                downloaded = downloading => downloaded.map_err(|error| download_failure(&error)),
                () = over.notified() => Err(UpdateFailure::Invalid),
            }
        };
        let bytes = tokio::time::timeout(DOWNLOAD_TIMEOUT + SLACK, bounded)
            .await
            .map_err(|_| UpdateFailure::Unreachable)??;
        // The signal stops a large download early; a body that arrives in one
        // read finishes before it's seen, so the whole is measured too.
        if u64::try_from(bytes.len()).map_or(true, |length| length > cap) {
            return Err(UpdateFailure::Invalid);
        }
        Ok(bytes)
    }

    async fn install(&self, release: Update, bytes: Vec<u8>) -> Result<(), UpdateFailure> {
        // On the main thread, as Windows' shell calls want (ADR-0018's
        // opener runs there too). On success the plugin starts the installer
        // and the app exits.
        let (sent, answered) = tokio::sync::oneshot::channel();
        self.app
            .run_on_main_thread(move || {
                let _ = sent.send(release.install(&bytes));
            })
            .map_err(|_| UpdateFailure::Install)?;
        match tokio::time::timeout(INSTALL_TIMEOUT, answered).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(install_failure(&error)),
            Ok(Err(_)) | Err(_) => Err(UpdateFailure::Install),
        }
    }
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

/// A failed download: no answer, or bytes that fail their checks.
fn download_failure(error: &Error) -> UpdateFailure {
    match error {
        Error::Reqwest(_) | Error::Network(_) | Error::Http(_) => UpdateFailure::Unreachable,
        _ => UpdateFailure::Invalid,
    }
}

/// A failed install: the bytes failed their checks again, or the installer
/// couldn't be written or started.
fn install_failure(error: &Error) -> UpdateFailure {
    match error {
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => UpdateFailure::Invalid,
        _ => UpdateFailure::Install,
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    use fleet_client::VerifiedLink;
    use rstest::rstest;
    use tempfile::TempDir;
    use tokio::sync::Notify;

    use super::*;
    use crate::keychain::KeychainFactory;
    use crate::profile::PROFILE_FILE;
    use crate::state::{LinkOpener, OpenError};

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

    #[rstest]
    #[case::the_release(RELEASE_IDENTIFIER, true, true)]
    #[case::a_debug_build_of_the_app(RELEASE_IDENTIFIER, false, false)]
    #[case::the_debug_rehearsal(REHEARSAL_IDENTIFIER, false, true)]
    #[case::a_released_rehearsal(REHEARSAL_IDENTIFIER, true, false)]
    #[case::the_check_build("io.github.noah-gabel.afkfleet.check", false, false)]
    #[case::a_released_check_build("io.github.noah-gabel.afkfleet.check", true, false)]
    #[case::another_app("io.example.other", true, false)]
    fn updates_are_on_only_for_the_release_or_the_debug_rehearsal(
        #[case] identifier: &str,
        #[case] release_build: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(updates_on(identifier, release_build), expected);
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
}
