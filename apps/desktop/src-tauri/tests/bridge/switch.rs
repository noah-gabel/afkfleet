//! The server profile through the bridge: setting it, the switch rules, and
//! the state after a restart.

use core::time::Duration;
use std::fs;
use std::sync::Arc;

use fleet_api_types::app::{
    CommandErrorCode, LogoutResponse, ProfileUnreadable, SERVER_URL_MAX_BYTES,
    ServerProfileRequest, ServerProfileResponse, ServerProfileState,
};
use fleet_client::{MemoryStore, SessionStore as _};
use secrecy::SecretString;
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::support::{Bridge, OWNER, Server, WAIT, assert_no_token};

/// The profile the bridge shows, or `None`.
async fn current(bridge: &Bridge) -> Option<String> {
    match bridge.state.server_profile().await {
        ServerProfileState::Set { profile } => Some(profile.url),
        ServerProfileState::NotSet | ServerProfileState::Unreadable { .. } => None,
    }
}

/// A refresh token of a real session of the Owner on `server`, made by
/// another device and never used, so it's still the session's current one.
async fn live_token(server: &Server) -> String {
    let bridge = Bridge::on(server).await;
    server.first_login(&bridge, OWNER).await;
    bridge.keychain.stored(&server.url).await.unwrap()
}

/// Whether `token` still refreshes on `server`: it was never sent before,
/// since a sent one is replaced, and sending a replaced one again is reuse.
async fn still_refreshes(server: &Server, token: &str) -> bool {
    let store = MemoryStore::new();
    store.save(&SecretString::from(token)).await.unwrap();
    server.client_over(store).me().await.is_ok()
}

#[tokio::test]
async fn a_new_state_has_no_profile_and_a_set_one_shows_its_origin() {
    let server = Server::start().await;
    let bridge = Bridge::new();

    let before = bridge.state.server_profile().await;
    let answer = bridge
        .state
        .set_server_profile(server.profile())
        .await
        .unwrap();
    let checked = bridge.state.check_server().await;

    assert_eq!(before, ServerProfileState::NotSet);
    assert!(
        matches!(&answer, ServerProfileResponse::Switched { profile, logout: LogoutResponse::Ended } if profile.url == server.url.as_str()),
        "{answer:?}"
    );
    assert_eq!(current(&bridge).await.as_deref(), Some(server.url.as_str()));
    assert!(checked.is_ok(), "{checked:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_refused_request_changes_nothing() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;
    let request = |url: &str, ca_pem: Option<&str>| ServerProfileRequest {
        url: url.to_owned(),
        ca_pem: ca_pem.map(str::to_owned),
    };

    let plain = bridge
        .state
        .set_server_profile(request("http://192.0.2.10", None))
        .await
        .unwrap_err();
    let ca = bridge
        .state
        .set_server_profile(request("https://example.com", Some("no certificate")))
        .await
        .unwrap_err();
    let long = bridge
        .state
        .set_server_profile(request(&"a".repeat(SERVER_URL_MAX_BYTES + 1), None))
        .await
        .unwrap_err();

    assert_eq!(plain.code, CommandErrorCode::InvalidServerUrl);
    assert_eq!(ca.code, CommandErrorCode::InvalidCa);
    assert_eq!(long.code, CommandErrorCode::InvalidInput);
    assert_eq!(current(&bridge).await.as_deref(), Some(server.url.as_str()));
    server.stop().await;
}

#[tokio::test]
async fn an_unreachable_server_changes_nothing() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    // Bound, then dropped: nothing listens there any more.
    let gone = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };

    let refused = bridge
        .state
        .set_server_profile(ServerProfileRequest {
            url: format!("http://{gone}"),
            ca_pem: None,
        })
        .await
        .unwrap_err();
    let me = bridge.state.me().await;

    assert_eq!(refused.code, CommandErrorCode::Unreachable);
    assert_eq!(current(&bridge).await.as_deref(), Some(server.url.as_str()));
    assert!(me.is_ok(), "{me:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_slow_health_check_never_blocks_the_other_commands() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let bridge = Arc::new(bridge);
    // Accepts connections and never answers.
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", silent.local_addr().unwrap());

    let setting = tokio::spawn({
        let bridge = Arc::clone(&bridge);
        async move {
            bridge
                .state
                .set_server_profile(ServerProfileRequest { url, ca_pem: None })
                .await
        }
    });
    let _accepted = timeout(WAIT, silent.accept()).await.unwrap().unwrap();
    let me = timeout(WAIT, bridge.state.me()).await;
    let profile = timeout(WAIT, bridge.state.server_profile()).await;
    setting.abort();

    assert!(matches!(me, Ok(Ok(_))), "{me:?}");
    assert!(
        matches!(profile, Ok(ServerProfileState::Set { .. })),
        "{profile:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn an_identical_profile_keeps_the_session_and_changes_nothing() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let spelled_differently = ServerProfileRequest {
        url: server.url.as_str().trim_end_matches('/').to_owned(),
        ca_pem: None,
    };

    let answer = bridge
        .state
        .set_server_profile(spelled_differently)
        .await
        .unwrap();
    let me = bridge.state.me().await;

    assert!(
        matches!(&answer, ServerProfileResponse::Unchanged { profile } if profile.url == server.url.as_str()),
        "{answer:?}"
    );
    assert!(me.is_ok(), "{me:?}");
    assert!(bridge.keychain.stored(&server.url).await.is_some());
    server.stop().await;
}

#[tokio::test]
async fn switching_logs_out_of_the_old_server_and_leaves_no_token_for_it() {
    let old = Server::start().await;
    let new = Server::start().await;
    let (bridge, app) = old.owner().await;
    let other_device = Bridge::on(&old).await;
    old.log_in(&other_device, OWNER, &app).await;

    let answer = bridge
        .state
        .set_server_profile(new.profile())
        .await
        .unwrap();
    let sessions_on_old = other_device.state.sessions(None, None).await.unwrap();
    let me_on_new = bridge.state.me().await.unwrap_err();

    assert!(
        matches!(&answer, ServerProfileResponse::Switched { profile, logout: LogoutResponse::Ended } if profile.url == new.url.as_str()),
        "{answer:?}"
    );
    assert_eq!(
        sessions_on_old.items.len(),
        1,
        "the old session is still live"
    );
    assert_eq!(bridge.keychain.stored(&old.url).await, None);
    assert_eq!(me_on_new.code, CommandErrorCode::NoSession);
    assert_eq!(current(&bridge).await.as_deref(), Some(new.url.as_str()));
    old.stop().await;
    new.stop().await;
}

#[tokio::test]
async fn a_token_stored_for_one_origin_is_never_used_by_a_client_of_another() {
    let a = Server::start().await;
    let b = Server::start().await;
    let token = live_token(&a).await;
    let bridge = Bridge::new();
    bridge.keychain.plant(&a.url, &token).await;

    bridge.state.set_server_profile(b.profile()).await.unwrap();
    let me = bridge.state.me().await.unwrap_err();

    assert_eq!(me.code, CommandErrorCode::NoSession);
    assert_eq!(
        bridge.keychain.stored(&a.url).await.as_deref(),
        Some(token.as_str())
    );
    assert!(still_refreshes(&a, &token).await);
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn without_a_profile_a_token_stored_for_the_new_origin_is_dropped_unsent() {
    let server = Server::start().await;
    let token = live_token(&server).await;
    let bridge = Bridge::new();
    bridge.keychain.plant(&server.url, &token).await;

    bridge
        .state
        .set_server_profile(server.profile())
        .await
        .unwrap();
    let me = bridge.state.me().await.unwrap_err();

    assert_eq!(me.code, CommandErrorCode::NoSession);
    assert_eq!(bridge.keychain.stored(&server.url).await, None);
    assert!(still_refreshes(&server, &token).await);
    server.stop().await;
}

#[tokio::test]
async fn after_an_unreadable_profile_a_token_stored_for_the_new_origin_is_dropped_unsent() {
    let server = Server::start().await;
    let token = live_token(&server).await;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("server-profile.json"), b"{ broken").unwrap();
    let bridge = Bridge::over(dir, crate::support::TestFactory::default());
    bridge.keychain.plant(&server.url, &token).await;

    let before = bridge.state.server_profile().await;
    bridge
        .state
        .set_server_profile(server.profile())
        .await
        .unwrap();

    assert_eq!(
        before,
        ServerProfileState::Unreadable {
            reason: ProfileUnreadable::Json
        }
    );
    assert_eq!(bridge.keychain.stored(&server.url).await, None);
    assert!(still_refreshes(&server, &token).await);
    server.stop().await;
}

#[tokio::test]
async fn a_switch_whose_new_entry_cant_be_cleared_keeps_the_old_profile_logged_out() {
    let old = Server::start().await;
    let new = Server::start().await;
    let (bridge, _app) = old.owner().await;
    bridge.keychain.store(&new.url).fail_clears();

    let refused = bridge
        .state
        .set_server_profile(new.profile())
        .await
        .unwrap_err();
    let me = bridge.state.me().await.unwrap_err();

    assert_eq!(refused.code, CommandErrorCode::Store);
    assert_eq!(current(&bridge).await.as_deref(), Some(old.url.as_str()));
    assert_eq!(me.code, CommandErrorCode::NoSession);
    assert_eq!(bridge.keychain.stored(&old.url).await, None);
    old.stop().await;
    new.stop().await;
}

#[tokio::test]
async fn a_switch_whose_profile_cant_be_written_keeps_the_old_profile_logged_out() {
    let old = Server::start().await;
    let new = Server::start().await;
    let (bridge, _app) = old.owner().await;
    // A folder where the temporary file would go: the write fails.
    fs::create_dir(bridge.dir.path().join("server-profile.json.tmp")).unwrap();

    let refused = bridge
        .state
        .set_server_profile(new.profile())
        .await
        .unwrap_err();
    let me = bridge.state.me().await.unwrap_err();

    assert_eq!(refused.code, CommandErrorCode::ProfileSave);
    assert_eq!(current(&bridge).await.as_deref(), Some(old.url.as_str()));
    assert_eq!(me.code, CommandErrorCode::NoSession);
    old.stop().await;
    new.stop().await;
}

#[tokio::test]
async fn a_switch_waits_for_a_call_in_flight_and_the_next_call_reaches_the_new_server() {
    let old = Server::start().await;
    let new = Server::start().await;
    let (bridge, _app) = old.owner().await;
    // A restart over the same profile file and keychain: the access token
    // lived only in memory, so the next call loads the refresh token first.
    let Bridge {
        keychain,
        dir,
        state,
    } = bridge;
    drop(state);
    let restarted = Arc::new(Bridge::over(dir, keychain));
    let hold = restarted.keychain.store(&old.url).hold_next_load();

    // The call in flight on the old client, held inside its keychain load.
    let in_flight = tokio::spawn({
        let restarted = Arc::clone(&restarted);
        async move { restarted.state.me().await }
    });
    timeout(WAIT, hold.entered.notified()).await.unwrap();
    let mut switching = tokio::spawn({
        let restarted = Arc::clone(&restarted);
        let request = new.profile();
        async move { restarted.state.set_server_profile(request).await }
    });
    let waited = timeout(Duration::from_millis(300), &mut switching).await;
    hold.release.notify_one();
    let me_old = timeout(WAIT, in_flight).await.unwrap().unwrap();
    let switched = timeout(WAIT, switching).await.unwrap().unwrap();
    let next = restarted.state.me().await.unwrap_err();

    assert!(
        waited.is_err(),
        "the switch didn't wait for the call in flight"
    );
    assert!(me_old.is_ok(), "{me_old:?}");
    assert!(
        matches!(&switched, Ok(ServerProfileResponse::Switched { profile, .. }) if profile.url == new.url.as_str()),
        "{switched:?}"
    );
    assert_eq!(next.code, CommandErrorCode::NoSession);
    assert_eq!(current(&restarted).await.as_deref(), Some(new.url.as_str()));
    old.stop().await;
    new.stop().await;
}

#[tokio::test]
async fn after_a_restart_the_saved_profile_and_session_resume() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let Bridge {
        keychain,
        dir,
        state,
    } = bridge;
    drop(state);

    let restarted = Bridge::over(dir, keychain);
    let me = restarted.state.me().await;

    assert_eq!(
        current(&restarted).await.as_deref(),
        Some(server.url.as_str())
    );
    assert!(me.is_ok(), "{me:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_saved_profile_whose_store_cant_be_built_is_unreadable_client() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;
    let Bridge {
        keychain,
        dir,
        state,
    } = bridge;
    drop(state);
    keychain.refuse(&server.url);

    let restarted = Bridge::over(dir, keychain);

    assert_eq!(
        restarted.state.server_profile().await,
        ServerProfileState::Unreadable {
            reason: ProfileUnreadable::Client
        }
    );
    server.stop().await;
}

#[tokio::test]
async fn a_refused_store_for_the_new_origin_changes_nothing() {
    let server = Server::start().await;
    let bridge = Bridge::new();
    bridge.keychain.refuse(&server.url);

    let refused = bridge
        .state
        .set_server_profile(server.profile())
        .await
        .unwrap_err();

    assert_eq!(refused.code, CommandErrorCode::Store);
    assert_eq!(
        bridge.state.server_profile().await,
        ServerProfileState::NotSet
    );
    server.stop().await;
}

#[tokio::test]
async fn no_answer_of_a_whole_session_holds_a_token() {
    let server = Server::start().await;
    let bridge = Bridge::new();

    let set = bridge
        .state
        .set_server_profile(server.profile())
        .await
        .unwrap();
    let step = bridge
        .state
        .login(crate::support::login(OWNER, crate::support::PASSWORD))
        .await
        .unwrap();
    let enroll = bridge.state.mfa_enroll().await.unwrap();
    let app = crate::support::app_of(&enroll);
    let confirm = bridge.state.mfa_confirm(server.code(&app)).await.unwrap();
    server.next_step();
    let again = bridge
        .state
        .login(crate::support::login(OWNER, crate::support::PASSWORD))
        .await
        .unwrap();
    bridge.state.login_mfa(server.code(&app)).await.unwrap();
    let me = bridge.state.me().await.unwrap();
    let sessions = bridge.state.sessions(None, None).await.unwrap();
    let state = bridge.state.server_profile().await;
    let logout = bridge.state.logout().await;

    assert_no_token(&set);
    assert_no_token(&step);
    assert_no_token(&enroll);
    assert_no_token(&confirm);
    assert_no_token(&again);
    assert_no_token(&me);
    assert_no_token(&sessions);
    assert_no_token(&state);
    assert_no_token(&logout);
    server.stop().await;
}
