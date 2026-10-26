//! The live events watch (Plan.md P11.8; ADR-0020, U29, S59, S61, S64,
//! R82): `watch_events` over the real server's socket, its updates as the
//! UI's channel gets them, and how logouts, revokes, newer watches and
//! server switches end it.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use fleet_api_types::SecretText;
use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::app::{
    CommandErrorCode, LiveDisconnect, LiveRefetchReason, LiveUpdateMessage, ServerProfileRequest,
    ServerProfileResponse,
};
use fleet_api_types::events::EventMessageKind;
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::timeout;

use crate::support::{Bridge, OWNER, Server, WAIT, assert_no_token};

/// What a watch's channel got: each update's JSON, as the webview reads it.
#[derive(Clone)]
struct Updates {
    sent: Arc<Mutex<Vec<String>>>,
    count: Arc<watch::Sender<usize>>,
}

impl Updates {
    fn new() -> Self {
        Self {
            sent: Arc::default(),
            count: Arc::new(watch::Sender::new(0)),
        }
    }

    /// A channel that keeps what it's sent.
    fn channel(&self) -> Channel<LiveUpdateMessage> {
        let updates = self.clone();
        Channel::new(move |body| {
            let InvokeResponseBody::Json(json) = body else {
                panic!("an update that isn't JSON");
            };
            updates.sent.lock().unwrap().push(json);
            updates.count.send_modify(|count| *count += 1);
            Ok(())
        })
    }

    /// Every update so far, read back.
    fn all(&self) -> Vec<LiveUpdateMessage> {
        self.raw()
            .iter()
            .map(|json| serde_json::from_str(json).unwrap())
            .collect()
    }

    /// Every update so far, as JSON.
    fn raw(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    /// Waits until the updates so far meet `condition`.
    async fn until(&self, what: &str, condition: impl Fn(&[LiveUpdateMessage]) -> bool) {
        let mut count = self.count.subscribe();
        timeout(WAIT, count.wait_for(|_| condition(&self.all())))
            .await
            .unwrap_or_else(|_| panic!("{what}: {:?}", self.all()))
            .unwrap();
    }

    /// Waits for the socket's `ready`, its `n`th.
    async fn ready(&self, n: usize) {
        self.until("ready", |updates| {
            updates.iter().filter(|update| is_ready(update)).count() >= n
        })
        .await;
    }
}

fn is_ready(update: &LiveUpdateMessage) -> bool {
    matches!(
        update,
        LiveUpdateMessage::Refetch {
            reason: LiveRefetchReason::Ready
        }
    )
}

fn is_event(update: &LiveUpdateMessage, kind: EventMessageKind) -> bool {
    matches!(update, LiveUpdateMessage::Event { event } if event.kind.known() == Some(kind))
}

/// A new offline account of the bridge's user, whose bot every socket of
/// theirs is told about.
async fn new_bot(bridge: &Bridge, name: &str) {
    bridge
        .state
        .create_offline_account(CreateOfflineAccountRequest {
            name: SecretText::new(name.to_owned()),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_watch_says_ready_forwards_a_bot_frame_and_ends_at_logout_with_no_session() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let updates = Updates::new();

    let acting = async {
        updates.ready(1).await;
        new_bot(&bridge, "AfkBot1").await;
        updates
            .until("the new bot's frame", |all| {
                all.iter()
                    .any(|update| is_event(update, EventMessageKind::Bot))
            })
            .await;
        bridge.state.logout().await;
    };
    let (watched, ()) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(updates.channel()), acting)
    })
    .await
    .unwrap();

    assert_eq!(watched.unwrap_err().code, CommandErrorCode::NoSession);
    assert!(is_ready(&updates.all()[0]), "{:?}", updates.all());
    server.stop().await;
}

/// The Channel's crossing (ADR-0007, S59): every kind of update, a refetch,
/// an event, the server's close and a connect that fails, holds no token
/// and no ticket, whole or by its prefix.
#[tokio::test]
async fn no_update_of_any_kind_holds_a_token_or_a_ticket() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let url = server.url.clone();
    let refresh = bridge.keychain.stored(&url).await.unwrap();
    let updates = Updates::new();

    let acting = async {
        updates.ready(1).await;
        new_bot(&bridge, "AfkBot1").await;
        updates
            .until("the new bot's frame", |all| {
                all.iter()
                    .any(|update| is_event(update, EventMessageKind::Bot))
            })
            .await;
        server.stop().await;
        updates
            .until("a connect that failed", |all| {
                all.iter().any(|update| {
                    matches!(
                        update,
                        LiveUpdateMessage::Disconnected {
                            disconnect: LiveDisconnect::NotOpened { .. },
                            ..
                        }
                    )
                })
            })
            .await;
        bridge.state.logout().await;
    };
    let (watched, ()) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(updates.channel()), acting)
    })
    .await
    .unwrap();

    assert_eq!(watched.unwrap_err().code, CommandErrorCode::NoSession);
    let all = updates.all();
    assert!(all.iter().any(is_ready));
    assert!(all.iter().any(|update| matches!(
        update,
        LiveUpdateMessage::Disconnected {
            disconnect: LiveDisconnect::Closed { code: Some(1001) },
            ..
        }
    )));
    for json in updates.raw() {
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_no_token(&value);
        assert!(!json.contains("afk_wt_"), "a ticket in {json}");
        assert!(!json.contains(&refresh), "the refresh token in {json}");
    }
}

#[tokio::test]
async fn a_revoked_session_ends_the_watch_with_no_session() {
    let server = Server::start().await;
    let (bridge, app) = server.owner().await;
    let other = Bridge::on(&server).await;
    server.log_in(&other, OWNER, &app).await;
    let updates = Updates::new();

    let revoking = async {
        updates.ready(1).await;
        let page = other.state.sessions(None, None).await.unwrap();
        let watching = page.items.iter().find(|session| !session.current).unwrap();
        other.state.revoke_session(watching.id).await.unwrap();
    };
    let (watched, ()) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(updates.channel()), revoking)
    })
    .await
    .unwrap();

    assert_eq!(watched.unwrap_err().code, CommandErrorCode::NoSession);
    server.stop().await;
}

#[tokio::test]
async fn a_newer_watch_stops_the_older_one() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let (older, newer) = (Updates::new(), Updates::new());

    let starting = async {
        older.ready(1).await;
        let acting = async {
            newer.ready(1).await;
            bridge.state.logout().await;
        };
        tokio::join!(bridge.state.watch_events(newer.channel()), acting).0
    };
    let (first, second) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(older.channel()), starting)
    })
    .await
    .unwrap();

    assert_eq!(first, Ok(()), "stopped by the newer one");
    assert_eq!(second.unwrap_err().code, CommandErrorCode::NoSession);
    server.stop().await;
}

#[tokio::test]
async fn a_switch_to_another_server_stops_the_watch_and_the_next_has_no_session() {
    let old = Server::start().await;
    let new = Server::start().await;
    let (bridge, _app) = old.owner().await;
    let updates = Updates::new();

    let switching = async {
        updates.ready(1).await;
        bridge.state.set_server_profile(new.profile()).await
    };
    let (watched, switched) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(updates.channel()), switching)
    })
    .await
    .unwrap();
    let next = timeout(WAIT, bridge.state.watch_events(Updates::new().channel()))
        .await
        .unwrap();

    assert_eq!(watched, Ok(()));
    assert!(
        matches!(switched, Ok(ServerProfileResponse::Switched { .. })),
        "{switched:?}"
    );
    assert_eq!(next.unwrap_err().code, CommandErrorCode::NoSession);
    old.stop().await;
    new.stop().await;
}

/// U29: an identical profile's save stops the watch before its exclusive
/// guard, keeps the session, and a watch started on its answer goes live on
/// that session without a login.
#[tokio::test]
async fn an_identical_profiles_save_stops_the_watch_and_the_next_one_goes_live_on_the_same_session()
{
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let spelled_differently = ServerProfileRequest {
        url: server.url.as_str().trim_end_matches('/').to_owned(),
        ca_pem: None,
    };
    let (first, second) = (Updates::new(), Updates::new());

    let saving = async {
        first.ready(1).await;
        bridge.state.set_server_profile(spelled_differently).await
    };
    let (watched, saved) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(first.channel()), saving)
    })
    .await
    .unwrap();
    let again = async {
        second.ready(1).await;
        bridge.state.logout().await;
    };
    let (rewatched, ()) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(second.channel()), again)
    })
    .await
    .unwrap();

    assert_eq!(watched, Ok(()));
    assert!(
        matches!(saved, Ok(ServerProfileResponse::Unchanged { .. })),
        "{saved:?}"
    );
    assert_eq!(rewatched.unwrap_err().code, CommandErrorCode::NoSession);
    server.stop().await;
}

/// S61: the watch stops only once the new server answered, so a save that
/// changes nothing leaves it running.
#[tokio::test]
async fn a_refused_save_leaves_the_watch_running() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let gone = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let updates = Updates::new();

    let acting = async {
        updates.ready(1).await;
        let refused = bridge
            .state
            .set_server_profile(ServerProfileRequest {
                url: gone,
                ca_pem: None,
            })
            .await;
        new_bot(&bridge, "AfkBot1").await;
        updates
            .until("the new bot's frame on the same watch", |all| {
                all.iter()
                    .any(|update| is_event(update, EventMessageKind::Bot))
            })
            .await;
        bridge.state.logout().await;
        refused
    };
    let (watched, refused) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(updates.channel()), acting)
    })
    .await
    .unwrap();

    assert_eq!(refused.unwrap_err().code, CommandErrorCode::Unreachable);
    assert_eq!(watched.unwrap_err().code, CommandErrorCode::NoSession);
    assert_eq!(
        updates
            .all()
            .iter()
            .filter(|update| is_ready(update))
            .count(),
        1,
        "one socket all along"
    );
    server.stop().await;
}

/// A watch whose connect waits inside its session's refresh holds no
/// guard the switch needs: the switch stops it at once, and completes once
/// the refresh is let go.
#[tokio::test]
async fn a_switch_stops_a_watch_whose_connect_waits_and_then_completes() {
    let old = Server::start().await;
    let new = Server::start().await;
    let (bridge, _app) = old.owner().await;
    // A restart over the same profile file and keychain: the access token
    // lived only in memory, so the watch's first ticket loads the refresh
    // token first.
    let Bridge {
        keychain,
        dir,
        state,
        ..
    } = bridge;
    drop(state);
    let restarted = Arc::new(Bridge::over(dir, keychain));
    let hold = restarted.keychain.store(&old.url).hold_next_load();

    let watching = tokio::spawn({
        let restarted = Arc::clone(&restarted);
        let updates = Updates::new();
        async move { restarted.state.watch_events(updates.channel()).await }
    });
    timeout(WAIT, hold.entered.notified()).await.unwrap();
    let switching = tokio::spawn({
        let restarted = Arc::clone(&restarted);
        let request = new.profile();
        async move { restarted.state.set_server_profile(request).await }
    });
    let watched = timeout(WAIT, watching).await.unwrap().unwrap();
    hold.release.notify_one();
    let switched = timeout(WAIT, switching).await.unwrap().unwrap();

    assert_eq!(watched, Ok(()), "stopped while its connect waited");
    assert!(
        matches!(switched, Ok(ServerProfileResponse::Switched { .. })),
        "{switched:?}"
    );
    old.stop().await;
    new.stop().await;
}

/// S64: a channel that refuses an update never delivers again, so the
/// watch stops at the first refusal; the next watch runs as any.
#[tokio::test]
async fn a_channel_that_refuses_an_update_stops_the_watch() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let tries = Arc::new(AtomicU32::new(0));
    let refusing = Channel::new({
        let tries = Arc::clone(&tries);
        move |_| {
            tries.fetch_add(1, Ordering::SeqCst);
            Err(tauri::Error::WebviewNotFound)
        }
    });

    let refused = timeout(WAIT, bridge.state.watch_events(refusing))
        .await
        .unwrap();
    let updates = Updates::new();
    let again = async {
        updates.ready(1).await;
        bridge.state.logout().await;
    };
    let (rewatched, ()) = timeout(WAIT, async {
        tokio::join!(bridge.state.watch_events(updates.channel()), again)
    })
    .await
    .unwrap();

    assert_eq!(refused, Ok(()));
    assert_eq!(
        tries.load(Ordering::SeqCst),
        1,
        "only the first update was tried"
    );
    assert_eq!(rewatched.unwrap_err().code, CommandErrorCode::NoSession);
    server.stop().await;
}
