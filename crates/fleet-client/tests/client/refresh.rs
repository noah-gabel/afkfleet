//! The server's refresh rules through the client (ADR-0016): a transparent
//! refresh, one refresh for many requests, no grace window, the idle
//! expiry, and logout after a restart.

use core::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fleet_api_types::PageQuery;
use fleet_client::{ClientError, MemoryStore, SessionStore, StoreError};
use secrecy::SecretString;
use tokio::task::JoinSet;

use crate::support::{OWNER, Server, stored};

/// Past the access token's 15 minutes.
const ACCESS_EXPIRED: Duration = Duration::from_mins(16);

/// A [`MemoryStore`] that counts its saves. Clones share both.
#[derive(Clone, Default)]
struct CountingStore {
    inner: MemoryStore,
    saves: Arc<AtomicUsize>,
}

impl SessionStore for CountingStore {
    async fn load(&self) -> Result<Option<SecretString>, StoreError> {
        self.inner.load().await
    }

    async fn save(&self, refresh_token: &SecretString) -> Result<(), StoreError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        self.inner.save(refresh_token).await
    }

    async fn clear(&self) -> Result<(), StoreError> {
        self.inner.clear().await
    }
}

#[tokio::test]
async fn an_expired_access_token_is_refreshed_without_the_caller_noticing() {
    let server = Server::start().await;
    let (owner, store, _) = server.owner().await;
    let before = stored(&store).await;

    server.advance(ACCESS_EXPIRED);
    let me = owner.me().await;

    assert!(me.is_ok(), "{me:?}");
    let after = stored(&store).await;
    assert!(after.is_some());
    assert_ne!(after, before);
    server.stop().await;
}

#[tokio::test]
async fn eight_requests_after_the_expiry_refresh_once_and_the_session_lives_on() {
    let server = Server::start().await;
    let store = CountingStore::default();
    let owner = server.client_over(store.clone());
    server.first_login(&owner, OWNER).await;
    let saves = store.saves.load(Ordering::SeqCst);

    server.advance(ACCESS_EXPIRED);
    let mut requests = JoinSet::new();
    for _ in 0..8 {
        let client = owner.clone();
        requests.spawn(async move { client.me().await });
    }
    let answers = requests.join_all().await;

    for answer in &answers {
        assert!(answer.is_ok(), "{answer:?}");
    }
    assert_eq!(store.saves.load(Ordering::SeqCst), saves + 1);
    owner.me().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn a_lost_refresh_answer_is_reuse_and_ends_the_whole_session() {
    let server = Server::start().await;
    let (owner, store, _) = server.owner().await;
    let replaced = store.load().await.unwrap().unwrap();
    server.advance(ACCESS_EXPIRED);
    owner.me().await.unwrap();
    let current = store.load().await.unwrap().unwrap();

    store.save(&replaced).await.unwrap();
    server.advance(ACCESS_EXPIRED);
    let reused = owner.me().await.unwrap_err();
    let (later, later_store) = server.client();
    later_store.save(&current).await.unwrap();
    let revoked = later.me().await.unwrap_err();

    assert!(matches!(reused, ClientError::NoSession), "{reused:?}");
    assert_eq!(stored(&store).await, None);
    assert!(matches!(revoked, ClientError::NoSession), "{revoked:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_session_idle_for_eight_days_ends() {
    let server = Server::start().await;
    let (owner, store, _) = server.owner().await;

    server.advance(Duration::from_hours(8 * 24));
    let ended = owner.me().await.unwrap_err();

    assert!(matches!(ended, ClientError::NoSession), "{ended:?}");
    assert_eq!(stored(&store).await, None);
    server.stop().await;
}

#[tokio::test]
async fn logout_after_a_restart_revokes_the_session_on_the_server() {
    let server = Server::start().await;
    let (_, store, app) = server.owner().await;
    let (other, _) = server.client();
    server.log_in(&other, OWNER, &app).await;

    let restarted = server.client_over(store.clone());
    restarted.logout().await.unwrap();
    let sessions = other
        .sessions(&PageQuery {
            before: None,
            limit: None,
        })
        .await
        .unwrap();

    assert_eq!(stored(&store).await, None);
    assert_eq!(sessions.items.len(), 1);
    assert!(sessions.items[0].current);
    other.me().await.unwrap();
    server.stop().await;
}
