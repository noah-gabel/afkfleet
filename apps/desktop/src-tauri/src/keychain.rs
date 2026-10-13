//! The refresh token in the OS keychain (Plan.md P8.4, ADR-0007, ADR-0017):
//! fleet-client's [`SessionStore`] over keyring-core.
//!
//! - **One entry per server origin.** The service is the app's identifier
//!   and the account is the origin (`https://example.com/`), so a client can
//!   only ever load a refresh token that came from its own server. On Windows
//!   the Credential Manager shows it as `<origin>.<identifier>`, a generic
//!   credential with `Local` persistence: it stays on this machine and never
//!   roams.
//! - **Off the runtime, in order, per entry.** Every call runs in a blocking
//!   thread. It first takes its entry's turn, a tokio mutex taken in async
//!   code, whose guard moves into the thread. So a call that timed out while
//!   waiting for its turn never runs, and one that has started finishes
//!   before the next starts, even after fleet-client's 5 s timeout gave up on
//!   it: the Windows store doesn't order calls on one entry from different
//!   threads (ADR-0017).
//! - **One turn per origin, not per store.** [`KeychainFactory`] keeps each
//!   origin's turn and hands it to every store it builds for that origin, and
//!   its clones share them. So two stores of one entry wait for each other
//!   too, as the old and the new client do when only the CA changes: a late
//!   save of the old client's can't land after the new client's discard.
//! - **No content in errors.** keyring-core's errors can hold the stored
//!   bytes, so they become a [`KeychainError`] that names only what failed.

use core::future::Future;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError};

use fleet_client::{ServerUrl, SessionStore, StoreError, StoreOp};
use keyring_core::{CredentialStore, Entry};
use secrecy::{ExposeSecret as _, SecretString};
use tauri::async_runtime::{Mutex, spawn_blocking};
use thiserror::Error;

/// The Windows credential persistence of every entry: this machine only.
const PERSISTENCE: &str = "Local";

/// Whose turn it is on one keychain entry: held from before a call's thread
/// starts until it ends.
type Turn = Arc<Mutex<()>>;

/// Why the keychain failed, without any of the stored content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KeychainError {
    /// The OS keychain can't be reached.
    #[error("the keychain can't be accessed")]
    NoAccess,
    /// The OS keychain failed.
    #[error("the keychain failed")]
    Platform,
    /// The stored value isn't a token this app wrote.
    #[error("the stored value can't be read")]
    BadData,
    /// The entry's service, account or value broke a limit of the keychain.
    #[error("the entry can't be stored in the keychain")]
    Invalid,
    /// Anything else keyring-core reported.
    #[error("the keychain refused the call")]
    Other,
    /// The blocking call ended without an answer.
    #[error("the keychain call ended without an answer")]
    Interrupted,
}

impl From<keyring_core::Error> for KeychainError {
    fn from(error: keyring_core::Error) -> Self {
        // keyring-core's error is non-exhaustive, and some variants hold the
        // stored bytes: only the kind is kept.
        match error {
            keyring_core::Error::NoStorageAccess(_) => Self::NoAccess,
            keyring_core::Error::PlatformFailure(_) => Self::Platform,
            keyring_core::Error::BadEncoding(_) | keyring_core::Error::BadDataFormat(..) => {
                Self::BadData
            }
            keyring_core::Error::TooLong(..) | keyring_core::Error::Invalid(..) => Self::Invalid,
            _ => Self::Other,
        }
    }
}

/// One keychain entry's blocking calls. [`Entry`] is the real one; tests
/// use fakes.
pub(crate) trait Slot: Send + Sync + 'static {
    /// The stored token, or `None` when there's no entry.
    fn get(&self) -> Result<Option<SecretString>, KeychainError>;
    /// Stores `token`, replacing any stored one.
    fn set(&self, token: &SecretString) -> Result<(), KeychainError>;
    /// Removes the entry; none is fine.
    fn delete(&self) -> Result<(), KeychainError>;
}

impl Slot for Entry {
    fn get(&self) -> Result<Option<SecretString>, KeychainError> {
        match self.get_password() {
            Ok(token) => Ok(Some(SecretString::from(token))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set(&self, token: &SecretString) -> Result<(), KeychainError> {
        self.set_password(token.expose_secret())
            .map_err(KeychainError::from)
    }

    fn delete(&self) -> Result<(), KeychainError> {
        match self.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// The refresh token of one server origin, in the OS keychain; see the
/// [module docs](self).
#[derive(Clone)]
pub struct KeychainStore {
    slot: Arc<dyn Slot>,
    /// The entry's turn, shared by every store of that entry.
    turn: Turn,
}

impl KeychainStore {
    /// A store over `slot` that takes `turn`, its entry's.
    pub(crate) fn with_turn(slot: Arc<dyn Slot>, turn: Turn) -> Self {
        Self { slot, turn }
    }

    /// Runs `call` on the slot in a blocking thread, in turn.
    async fn run<R: Send + 'static>(
        &self,
        op: StoreOp,
        call: impl FnOnce(&dyn Slot) -> Result<R, KeychainError> + Send + 'static,
    ) -> Result<R, StoreError> {
        let slot = Arc::clone(&self.slot);
        // Taken here, in async code: a call whose future is dropped while it
        // waits (the client's timeout) never reaches the thread. The guard
        // moves into the thread, so the next call starts only after this one
        // ended, even when its caller stopped waiting.
        let turn = Arc::clone(&self.turn).lock_owned().await;
        spawn_blocking(move || {
            let _turn = turn;
            call(slot.as_ref())
        })
        .await
        .unwrap_or(Err(KeychainError::Interrupted))
        .map_err(|error| StoreError::new(op, error))
    }
}

impl SessionStore for KeychainStore {
    fn load(&self) -> impl Future<Output = Result<Option<SecretString>, StoreError>> + Send {
        self.run(StoreOp::Load, Slot::get)
    }

    fn save(
        &self,
        refresh_token: &SecretString,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let token = refresh_token.clone();
        self.run(StoreOp::Save, move |slot| slot.set(&token))
    }

    fn clear(&self) -> impl Future<Output = Result<(), StoreError>> + Send {
        self.run(StoreOp::Clear, Slot::delete)
    }
}

/// Only that it's a keychain store; never the slot or a token.
impl core::fmt::Debug for KeychainStore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeychainStore").finish_non_exhaustive()
    }
}

/// Builds the [`KeychainStore`] of a server origin; see the
/// [module docs](self).
#[derive(Clone)]
pub struct KeychainFactory {
    store: Arc<CredentialStore>,
    /// The entries' service: the app's identifier.
    service: String,
    /// The `persistence` modifier, for the Windows store only: keyring-core's
    /// mock store refuses every modifier.
    persistence: Option<&'static str>,
    /// Each entry's turn, by origin, shared by every store built for it and
    /// by every clone of this factory.
    turns: Arc<std::sync::Mutex<HashMap<String, Turn>>>,
}

impl KeychainFactory {
    /// Entries in the Windows Credential Manager under `service`, with
    /// `Local` persistence.
    #[must_use]
    pub fn windows(store: Arc<windows_native_keyring_store::Store>, service: &str) -> Self {
        Self {
            store,
            service: service.to_owned(),
            persistence: Some(PERSISTENCE),
            turns: Arc::default(),
        }
    }

    /// Entries in `store` under `service`, without modifiers: for
    /// keyring-core's mock store in tests.
    #[must_use]
    pub fn new(store: Arc<CredentialStore>, service: &str) -> Self {
        Self {
            store,
            service: service.to_owned(),
            persistence: None,
            turns: Arc::default(),
        }
    }

    /// The turn of `origin`'s entry.
    pub(crate) fn turn_for(&self, origin: &ServerUrl) -> Turn {
        // Held only for this lookup, never across an await. A thread that
        // panicked while holding it left a whole map behind, so a poisoned
        // lock is used as it is.
        let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(turns.entry(origin.as_str().to_owned()).or_default())
    }

    /// The entry of `origin`.
    fn entry(&self, origin: &ServerUrl) -> Result<Entry, KeychainError> {
        let modifiers: Option<HashMap<&str, &str>> = self
            .persistence
            .map(|persistence| HashMap::from([("persistence", persistence)]));
        self.store
            .build(&self.service, origin.as_str(), modifiers.as_ref())
            .map_err(KeychainError::from)
    }
}

/// Only the service; never a store's content.
impl core::fmt::Debug for KeychainFactory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeychainFactory")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

/// Builds the [`SessionStore`] of a server origin, so each server's refresh
/// token lives in an entry of its own.
pub trait StoreFactory: Send + Sync + 'static {
    /// The store it builds.
    type Store: SessionStore;

    /// The store of `origin`'s refresh token.
    ///
    /// # Errors
    /// [`StoreError`] when the keychain refuses the entry, e.g. an origin
    /// longer than its account limit.
    fn for_origin(&self, origin: &ServerUrl) -> Result<Self::Store, StoreError>;
}

impl StoreFactory for KeychainFactory {
    type Store = KeychainStore;

    fn for_origin(&self, origin: &ServerUrl) -> Result<Self::Store, StoreError> {
        let entry = self
            .entry(origin)
            .map_err(|error| StoreError::new(StoreOp::Load, error))?;
        Ok(KeychainStore::with_turn(
            Arc::new(entry),
            self.turn_for(origin),
        ))
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::sync::Mutex as StdMutex;
    use std::sync::mpsc;

    use rstest::rstest;
    use tokio::time::timeout;

    use super::*;

    /// How long a test waits for something that must happen.
    const WAIT: Duration = Duration::from_secs(5);

    /// What a [`FakeSlot`] was asked to do.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Get,
        Set(String),
        Delete,
    }

    /// A slot that records its calls and can hold the first one until it's
    /// released.
    struct FakeSlot {
        calls: StdMutex<Vec<Call>>,
        value: StdMutex<Option<String>>,
        /// Signals that a call has started.
        entered: StdMutex<mpsc::Sender<()>>,
        /// When set, the next call blocks until it receives.
        hold: StdMutex<Option<mpsc::Receiver<()>>>,
        error: StdMutex<Option<KeychainError>>,
    }

    impl FakeSlot {
        fn record(&self, call: Call) -> Result<(), KeychainError> {
            self.calls.lock().unwrap().push(call);
            let _ = self.entered.lock().unwrap().send(());
            if let Some(hold) = self.hold.lock().unwrap().take() {
                // Bounded, so a broken test fails instead of hanging.
                let _ = hold.recv_timeout(WAIT);
            }
            self.error.lock().unwrap().take().map_or(Ok(()), Err)
        }
    }

    impl Slot for FakeSlot {
        fn get(&self) -> Result<Option<SecretString>, KeychainError> {
            self.record(Call::Get)?;
            Ok(self.value.lock().unwrap().clone().map(SecretString::from))
        }

        fn set(&self, token: &SecretString) -> Result<(), KeychainError> {
            self.record(Call::Set(token.expose_secret().to_owned()))?;
            *self.value.lock().unwrap() = Some(token.expose_secret().to_owned());
            Ok(())
        }

        fn delete(&self) -> Result<(), KeychainError> {
            self.record(Call::Delete)?;
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    /// A fake slot and the signal of each call's start.
    fn fake_slot() -> (Arc<FakeSlot>, mpsc::Receiver<()>) {
        let (entered, started) = mpsc::channel();
        let slot = Arc::new(FakeSlot {
            calls: StdMutex::new(Vec::new()),
            value: StdMutex::new(None),
            entered: StdMutex::new(entered),
            hold: StdMutex::new(None),
            error: StdMutex::new(None),
        });
        (slot, started)
    }

    /// A store over a fake slot, the slot, and the signal of each call's
    /// start.
    fn fake() -> (KeychainStore, Arc<FakeSlot>, mpsc::Receiver<()>) {
        let (slot, started) = fake_slot();
        let store =
            KeychainStore::with_turn(Arc::clone(&slot) as Arc<dyn Slot>, Arc::new(Mutex::new(())));
        (store, slot, started)
    }

    /// Releases a held slot when dropped, so a failing test never leaves a
    /// blocking thread that the runtime's shutdown would wait for.
    struct Release(Option<mpsc::Sender<()>>);

    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() {
                let _ = release.send(());
            }
        }
    }

    fn token(text: &str) -> SecretString {
        SecretString::from(text)
    }

    // Real time: the blocking calls run on Tauri's runtime, outside this
    // test's, so paused time would fire the bounds early (ADR-0017).
    #[tokio::test]
    async fn a_call_that_times_out_waiting_for_its_turn_never_runs() {
        let (store, slot, started) = fake();
        let (release, held) = mpsc::channel();
        *slot.hold.lock().unwrap() = Some(held);
        let mut release = Release(Some(release));

        let first = tokio::spawn({
            let store = store.clone();
            async move { store.save(&token("afk_rt_1")).await }
        });
        // Waited for off the runtime: this test's runtime has one thread.
        tokio::task::spawn_blocking(move || started.recv_timeout(WAIT))
            .await
            .unwrap()
            .unwrap();
        let second = timeout(Duration::from_millis(100), store.save(&token("afk_rt_2"))).await;
        let third = tokio::spawn({
            let store = store.clone();
            async move { store.clear().await }
        });
        drop(release.0.take());
        let first = timeout(WAIT, first).await.unwrap().unwrap();
        let third = timeout(WAIT, third).await.unwrap().unwrap();

        assert!(second.is_err(), "the second call didn't wait for its turn");
        assert!(first.is_ok() && third.is_ok(), "{first:?} {third:?}");
        assert_eq!(
            *slot.calls.lock().unwrap(),
            [Call::Set("afk_rt_1".to_owned()), Call::Delete]
        );
        assert_eq!(*slot.value.lock().unwrap(), None);
    }

    #[test]
    fn stores_for_one_origin_share_its_turn_and_other_origins_dont() {
        let factory = mock_factory();
        let example = origin("https://example.com");

        let first = factory.for_origin(&example).unwrap();
        let respelled = factory
            .for_origin(&origin("https://EXAMPLE.com:443"))
            .unwrap();
        let from_a_clone = factory.clone().for_origin(&example).unwrap();
        let other = factory.for_origin(&origin("https://example.org")).unwrap();

        assert!(Arc::ptr_eq(&first.turn, &respelled.turn));
        assert!(Arc::ptr_eq(&first.turn, &from_a_clone.turn));
        assert!(!Arc::ptr_eq(&first.turn, &other.turn));
    }

    // Real time, like the test above. Two stores of one entry, as the old and
    // the new client hold when only the CA changes: a late save on the old
    // store must end before the new store's clear starts.
    #[tokio::test]
    async fn a_held_call_on_one_store_makes_another_store_of_its_origin_wait() {
        let factory = mock_factory();
        let example = origin("https://example.com");
        let (old_slot, old_started) = fake_slot();
        let (new_slot, _new_started) = fake_slot();
        let old = KeychainStore::with_turn(
            Arc::clone(&old_slot) as Arc<dyn Slot>,
            factory.turn_for(&example),
        );
        let new = KeychainStore::with_turn(
            Arc::clone(&new_slot) as Arc<dyn Slot>,
            factory.turn_for(&example),
        );
        let (release, held) = mpsc::channel();
        *old_slot.hold.lock().unwrap() = Some(held);
        let mut release = Release(Some(release));

        let late_save = tokio::spawn(async move { old.save(&token("afk_rt_old")).await });
        tokio::task::spawn_blocking(move || old_started.recv_timeout(WAIT))
            .await
            .unwrap()
            .unwrap();
        let mut clear = tokio::spawn(async move { new.clear().await });
        let waited = timeout(Duration::from_millis(100), &mut clear).await;
        let new_calls_while_held = new_slot.calls.lock().unwrap().clone();

        assert!(waited.is_err(), "the other store's call didn't wait");
        assert_eq!(new_calls_while_held, []);
        drop(release.0.take());
        let saved = timeout(WAIT, late_save).await.unwrap().unwrap();
        let cleared = timeout(WAIT, clear).await.unwrap().unwrap();
        assert!(saved.is_ok() && cleared.is_ok(), "{saved:?} {cleared:?}");
        assert_eq!(*new_slot.calls.lock().unwrap(), [Call::Delete]);
    }

    #[tokio::test]
    async fn stores_of_different_origins_never_wait_for_each_other() {
        let factory = mock_factory();
        let (held_slot, held_started) = fake_slot();
        let (free_slot, _free_started) = fake_slot();
        let held_store = KeychainStore::with_turn(
            Arc::clone(&held_slot) as Arc<dyn Slot>,
            factory.turn_for(&origin("https://example.com")),
        );
        let free_store = KeychainStore::with_turn(
            Arc::clone(&free_slot) as Arc<dyn Slot>,
            factory.turn_for(&origin("https://example.org")),
        );
        let (release, held) = mpsc::channel();
        *held_slot.hold.lock().unwrap() = Some(held);
        let mut release = Release(Some(release));

        let held_call = tokio::spawn(async move { held_store.save(&token("afk_rt_1")).await });
        tokio::task::spawn_blocking(move || held_started.recv_timeout(WAIT))
            .await
            .unwrap()
            .unwrap();
        let free_call = timeout(WAIT, free_store.clear()).await;
        drop(release.0.take());
        let held_call = timeout(WAIT, held_call).await.unwrap().unwrap();

        assert!(matches!(free_call, Ok(Ok(()))), "{free_call:?}");
        assert!(held_call.is_ok(), "{held_call:?}");
    }

    #[tokio::test]
    async fn a_saved_token_is_loaded_and_cleared() {
        let (store, slot, _started) = fake();

        store.save(&token("afk_rt_1")).await.unwrap();
        let loaded = store.load().await.unwrap();
        store.clear().await.unwrap();

        assert_eq!(loaded.unwrap().expose_secret(), "afk_rt_1");
        assert_eq!(
            store
                .load()
                .await
                .unwrap()
                .map(|t| t.expose_secret().to_owned()),
            None
        );
        assert_eq!(slot.calls.lock().unwrap().len(), 4);
    }

    #[rstest]
    #[case::load(StoreOp::Load)]
    #[case::save(StoreOp::Save)]
    #[case::clear(StoreOp::Clear)]
    #[tokio::test]
    async fn a_failing_call_names_its_operation_and_nothing_stored(#[case] op: StoreOp) {
        let (store, slot, _started) = fake();
        *slot.value.lock().unwrap() = Some("afk_rt_secret".to_owned());
        *slot.error.lock().unwrap() = Some(KeychainError::NoAccess);

        let error = match op {
            StoreOp::Load => store.load().await.map(|_| ()).unwrap_err(),
            StoreOp::Save => store.save(&token("afk_rt_new")).await.unwrap_err(),
            StoreOp::Clear => store.clear().await.unwrap_err(),
        };

        assert_eq!(error.op(), op);
        let shown = format!("{error} {error:?} {}", chain(&error));
        assert!(!shown.contains("afk_rt_"), "{shown}");
        assert!(shown.contains("can't be accessed"), "{shown}");
    }

    /// Every message in `error`'s source chain.
    fn chain(error: &dyn std::error::Error) -> String {
        let mut text = error.to_string();
        let mut source = error.source();
        while let Some(next) = source {
            text.push_str(": ");
            text.push_str(&next.to_string());
            source = next.source();
        }
        text
    }

    #[rstest]
    #[case::no_access(
        keyring_core::Error::NoStorageAccess("locked".into()),
        KeychainError::NoAccess
    )]
    #[case::platform(
        keyring_core::Error::PlatformFailure("broken".into()),
        KeychainError::Platform
    )]
    #[case::bad_encoding(
        keyring_core::Error::BadEncoding(b"afk_rt_secret".to_vec()),
        KeychainError::BadData
    )]
    #[case::bad_data(
        keyring_core::Error::BadDataFormat(b"afk_rt_secret".to_vec(), "bad".into()),
        KeychainError::BadData
    )]
    #[case::too_long(
        keyring_core::Error::TooLong("user".to_owned(), 513),
        KeychainError::Invalid
    )]
    #[case::invalid(
        keyring_core::Error::Invalid("persistence".to_owned(), "bad".to_owned()),
        KeychainError::Invalid
    )]
    #[case::other(keyring_core::Error::NoDefaultStore, KeychainError::Other)]
    fn keyring_errors_keep_only_what_failed(
        #[case] error: keyring_core::Error,
        #[case] expected: KeychainError,
    ) {
        let mapped = KeychainError::from(error);

        assert_eq!(mapped, expected);
        assert!(!format!("{mapped} {mapped:?}").contains("afk_rt_"));
    }

    /// A factory over keyring-core's in-memory mock store.
    fn mock_factory() -> KeychainFactory {
        KeychainFactory::new(keyring_core::mock::Store::new().unwrap(), "io.example.test")
    }

    fn origin(text: &str) -> ServerUrl {
        ServerUrl::parse(text).unwrap()
    }

    #[tokio::test]
    async fn an_entry_saves_loads_and_clears_through_keyring_core() {
        let factory = mock_factory();
        let store = factory.for_origin(&origin("https://example.com")).unwrap();

        let empty = store.load().await.unwrap();
        store.save(&token("afk_rt_1")).await.unwrap();
        let again = factory.for_origin(&origin("https://example.com")).unwrap();
        let loaded = again.load().await.unwrap();
        again.clear().await.unwrap();
        again.clear().await.unwrap();

        assert!(empty.is_none());
        assert_eq!(loaded.unwrap().expose_secret(), "afk_rt_1");
        assert!(store.load().await.unwrap().is_none());
    }

    #[rstest]
    #[case::uppercase_host("https://EXAMPLE.com")]
    #[case::default_port("https://example.com:443")]
    #[case::trailing_slash("https://example.com/")]
    #[tokio::test]
    async fn spellings_of_one_origin_share_its_entry(#[case] spelling: &str) {
        let factory = mock_factory();
        factory
            .for_origin(&origin("https://example.com"))
            .unwrap()
            .save(&token("afk_rt_1"))
            .await
            .unwrap();

        let loaded = factory
            .for_origin(&origin(spelling))
            .unwrap()
            .load()
            .await
            .unwrap();

        assert_eq!(loaded.unwrap().expose_secret(), "afk_rt_1");
    }

    #[rstest]
    #[case::other_port("https://example.com:8443")]
    #[case::other_host("https://example.org")]
    #[case::plain_http_loopback("http://127.0.0.1:8080")]
    #[tokio::test]
    async fn another_origin_never_sees_the_token(#[case] other: &str) {
        let factory = mock_factory();
        factory
            .for_origin(&origin("https://example.com"))
            .unwrap()
            .save(&token("afk_rt_1"))
            .await
            .unwrap();

        let loaded = factory
            .for_origin(&origin(other))
            .unwrap()
            .load()
            .await
            .unwrap();

        assert!(loaded.is_none());
    }

    #[test]
    fn debug_shows_neither_slot_nor_token() {
        let (store, _slot, _started) = fake();

        assert_eq!(format!("{store:?}"), "KeychainStore { .. }");
        assert_eq!(
            format!("{:?}", mock_factory()),
            r#"KeychainFactory { service: "io.example.test", .. }"#
        );
    }

    /// Deletes the real test entry when dropped, even after a failure.
    struct Cleanup(Entry);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.delete_credential();
        }
    }

    /// The real Windows Credential Manager, under a test-only service, so
    /// the app's own entries are never touched (CLAUDE.md rule 12).
    #[tokio::test]
    async fn the_windows_store_saves_loads_and_clears_with_local_persistence() {
        let service = format!("io.github.noah-gabel.afkfleet.test-{}", std::process::id());
        let factory = KeychainFactory::windows(
            windows_native_keyring_store::Store::new().unwrap(),
            &service,
        );
        let example = origin("https://example.com");
        let _cleanup = Cleanup(factory.entry(&example).unwrap());
        let store = factory.for_origin(&example).unwrap();

        store.save(&token("afk_rt_test-only")).await.unwrap();
        let loaded = store.load().await.unwrap();
        let attributes = factory.entry(&example).unwrap().get_attributes().unwrap();
        store.clear().await.unwrap();

        assert_eq!(loaded.unwrap().expose_secret(), "afk_rt_test-only");
        assert_eq!(
            attributes.get("persistence").map(String::as_str),
            Some("Local")
        );
        assert_eq!(
            attributes.get("target_name").map(String::as_str),
            Some(format!("https://example.com/.{service}").as_str())
        );
        assert!(store.load().await.unwrap().is_none());
    }
}
