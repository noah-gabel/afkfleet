//! [`SessionStore`]: where the refresh token lives (ADR-0007, ADR-0017).
//!
//! The client never keeps a copy of the refresh token: a refresh loads it,
//! sends it and saves the new one, and a login saves it. In the desktop app
//! the store is the OS keychain (P8.4); [`MemoryStore`] is for tests and for
//! callers without a keychain.
//!
//! A rotated refresh token must be written before anything else happens:
//! sending the replaced one later is reuse, which ends the session. If a
//! save fails, the client clears the store, so a restart can't send a
//! replaced token, and the session ends at the next refresh.

use core::error::Error;
use core::fmt;
use core::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use secrecy::SecretString;
use thiserror::Error;

/// Where the session's refresh token lives; see the [module docs](self).
///
/// The client bounds each call with [`STORE_TIMEOUT`](crate::STORE_TIMEOUT)
/// and never holds a lock across one. An implementation does its own
/// blocking work off the async runtime.
pub trait SessionStore: Send + Sync + 'static {
    /// The stored refresh token, if there is one.
    ///
    /// # Errors
    /// [`StoreError`] when the store can't be read.
    fn load(&self) -> impl Future<Output = Result<Option<SecretString>, StoreError>> + Send;

    /// Stores `refresh_token`, replacing any stored one.
    ///
    /// # Errors
    /// [`StoreError`] when it can't be written.
    fn save(
        &self,
        refresh_token: &SecretString,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Removes the stored refresh token; nothing stored is fine.
    ///
    /// # Errors
    /// [`StoreError`] when it can't be removed.
    fn clear(&self) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// What a [`StoreError`] was doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOp {
    /// [`SessionStore::load`].
    Load,
    /// [`SessionStore::save`].
    Save,
    /// [`SessionStore::clear`].
    Clear,
}

impl fmt::Display for StoreOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Load => "read the refresh token",
            Self::Save => "save the refresh token",
            Self::Clear => "remove the refresh token",
        })
    }
}

/// A failed [`SessionStore`] call. Its source is the store's own error,
/// which never holds the token.
#[derive(Debug, Error)]
#[error("the session store can't {op}")]
pub struct StoreError {
    op: StoreOp,
    #[source]
    source: Box<dyn Error + Send + Sync>,
}

impl StoreError {
    /// The store's `source` error while it did `op`.
    pub fn new(op: StoreOp, source: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self {
            op,
            source: source.into(),
        }
    }

    /// What the store was doing.
    #[must_use]
    pub fn op(&self) -> StoreOp {
        self.op
    }

    /// The call took longer than [`STORE_TIMEOUT`](crate::STORE_TIMEOUT).
    pub(crate) fn timed_out(op: StoreOp) -> Self {
        Self::new(op, StoreTimedOut)
    }
}

/// A store call that took too long.
#[derive(Debug, Error)]
#[error("it didn't answer within 5 s")]
struct StoreTimedOut;

/// A [`SessionStore`] in memory: for tests, and for callers without a
/// keychain. Clones share one token.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    token: Arc<Mutex<Option<SecretString>>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The token slot. A thread that panicked while holding it left a whole
    /// value behind, so a poisoned lock is used as it is.
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<SecretString>> {
        self.token.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SessionStore for MemoryStore {
    fn load(&self) -> impl Future<Output = Result<Option<SecretString>, StoreError>> + Send {
        core::future::ready(Ok(self.slot().clone()))
    }

    fn save(
        &self,
        refresh_token: &SecretString,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        *self.slot() = Some(refresh_token.clone());
        core::future::ready(Ok(()))
    }

    fn clear(&self) -> impl Future<Output = Result<(), StoreError>> + Send {
        *self.slot() = None;
        core::future::ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret as _;

    use super::*;

    fn token(text: &str) -> SecretString {
        SecretString::from(text)
    }

    #[tokio::test]
    async fn a_new_store_holds_nothing() {
        let store = MemoryStore::new();

        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_saved_token_is_loaded_back_by_every_clone() {
        let store = MemoryStore::new();
        let clone = store.clone();

        store.save(&token("afk_rt_one")).await.unwrap();
        clone.save(&token("afk_rt_two")).await.unwrap();

        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.expose_secret(), "afk_rt_two");
    }

    #[tokio::test]
    async fn clearing_removes_the_token_and_clearing_nothing_is_fine() {
        let store = MemoryStore::new();
        store.save(&token("afk_rt_one")).await.unwrap();

        store.clear().await.unwrap();
        store.clear().await.unwrap();

        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn debug_never_shows_the_token() {
        let store = MemoryStore::new();
        store.save(&token("afk_rt_secret")).await.unwrap();

        let shown = format!("{store:?}");

        assert!(!shown.contains("afk_rt_secret"), "{shown}");
        assert!(shown.contains("REDACTED"), "{shown}");
    }

    #[test]
    fn an_error_names_what_the_store_was_doing() {
        let shown = [StoreOp::Load, StoreOp::Save, StoreOp::Clear]
            .map(|op| StoreError::new(op, "denied").to_string());

        assert_eq!(
            shown,
            [
                "the session store can't read the refresh token",
                "the session store can't save the refresh token",
                "the session store can't remove the refresh token",
            ]
        );
        assert_eq!(StoreError::timed_out(StoreOp::Save).op(), StoreOp::Save);
    }
}
