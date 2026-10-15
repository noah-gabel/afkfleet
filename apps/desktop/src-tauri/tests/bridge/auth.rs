//! Logging in and out, registering, and stepping up through the bridge.

use std::sync::Arc;

use afkfleet_desktop::keychain::KeychainFactory;
use afkfleet_desktop::profile::PROFILE_FILE;
use afkfleet_desktop::state::AppState;
use fleet_api_types::app::{CommandErrorCode, LoginStepKind, LogoutResponse};
use fleet_api_types::auth::StepUpRequest;
use fleet_api_types::invites::CreateInviteRequest;
use fleet_api_types::users::UserRole;
use fleet_api_types::{ErrorCode, SecretText};
use keyring_core::api::CredentialStoreApi as _;

use crate::support::{
    Bridge, OWNER, PASSWORD, RecordingOpener, Server, app_of, assert_no_token, is_api, is_invalid,
    login, register, step_up_request,
};

#[tokio::test]
async fn a_first_login_sets_up_2fa_and_starts_a_session_whose_token_only_the_keychain_holds() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;

    let step = bridge.state.login(login(OWNER, PASSWORD)).await.unwrap();
    let enrollment = bridge.state.mfa_enroll().await.unwrap();
    let app = app_of(&enrollment);
    let codes = bridge.state.mfa_confirm(server.code(&app)).await.unwrap();
    server.next_step();
    let again = bridge.state.login(login(OWNER, PASSWORD)).await.unwrap();
    bridge.state.login_mfa(server.code(&app)).await.unwrap();
    let me = bridge.state.me().await.unwrap();

    assert_eq!(step.step, LoginStepKind::SetupRequired);
    assert!(step.expires_in > 0);
    assert_eq!(codes.recovery_codes.len(), 10);
    assert_eq!(again.step, LoginStepKind::MfaRequired);
    assert_eq!(me.username.as_str(), OWNER);
    let stored = bridge.keychain.stored(&server.url).await.unwrap();
    assert!(stored.starts_with("afk_rt_"));
    for answer in [
        serde_json::to_value(step).unwrap(),
        serde_json::to_value(again).unwrap(),
        serde_json::to_value(&me).unwrap(),
    ] {
        assert_no_token(&answer);
    }
    server.stop().await;
}

#[tokio::test]
async fn a_wrong_password_is_the_servers_401_invalid_credentials() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;

    let refused = bridge
        .state
        .login(login(OWNER, "not the password at all"))
        .await
        .unwrap_err();

    assert!(
        is_api(&refused, 401, ErrorCode::InvalidCredentials),
        "{refused:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn a_wrong_code_keeps_the_step_for_another_try() {
    let server = Server::start().await;
    let (bridge, app) = server.owner().await;
    bridge.state.logout().await;
    server.next_step();

    bridge.state.login(login(OWNER, PASSWORD)).await.unwrap();
    let wrong = bridge
        .state
        .login_mfa(SecretText::new("000000".to_owned()))
        .await
        .unwrap_err();
    let right = bridge.state.login_mfa(server.code(&app)).await;

    assert!(
        is_api(&wrong, 401, ErrorCode::InvalidCredentials),
        "{wrong:?}"
    );
    assert!(right.is_ok(), "{right:?}");
    server.stop().await;
}

#[tokio::test]
async fn the_sixth_login_a_minute_is_rate_limited_with_its_wait() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;

    for n in 1..=5 {
        let refused = bridge
            .state
            .login(login(&format!("nobody{n}"), PASSWORD))
            .await
            .unwrap_err();
        assert!(
            is_api(&refused, 401, ErrorCode::InvalidCredentials),
            "{refused:?}"
        );
    }
    let limited = bridge
        .state
        .login(login("nobody6", PASSWORD))
        .await
        .unwrap_err();

    assert!(is_api(&limited, 429, ErrorCode::RateLimited), "{limited:?}");
    assert_eq!(limited.api.unwrap().retry_after_secs, Some(12));
    server.stop().await;
}

#[tokio::test]
async fn a_login_while_logged_in_is_already_logged_in() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;

    let refused = bridge
        .state
        .login(login(OWNER, PASSWORD))
        .await
        .unwrap_err();

    assert_eq!(refused.code, CommandErrorCode::AlreadyLoggedIn);
    server.stop().await;
}

#[tokio::test]
async fn a_code_without_a_pending_step_is_no_pending_step() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;

    let refused = bridge
        .state
        .login_mfa(SecretText::new("123456".to_owned()))
        .await
        .unwrap_err();
    let enroll = bridge.state.mfa_enroll().await.unwrap_err();

    assert_eq!(refused.code, CommandErrorCode::NoPendingStep);
    assert_eq!(enroll.code, CommandErrorCode::NoPendingStep);
    server.stop().await;
}

#[tokio::test]
async fn an_empty_or_overlong_code_is_invalid_input_before_any_request() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;
    bridge.state.login(login(OWNER, PASSWORD)).await.unwrap();

    let empty = bridge
        .state
        .mfa_confirm(SecretText::new(String::new()))
        .await
        .unwrap_err();
    let long = bridge
        .state
        .login_mfa(SecretText::new("1".repeat(65)))
        .await
        .unwrap_err();
    // The setup step survived: nothing reached the server.
    let enrolled = bridge.state.mfa_enroll().await;

    assert!(is_invalid(&empty, "code"), "{empty:?}");
    assert!(is_invalid(&long, "code"), "{long:?}");
    assert!(enrolled.is_ok(), "{enrolled:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_login_request_that_breaks_a_rule_is_invalid_input() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;

    let refused = bridge
        .state
        .login(login(&"a".repeat(33), PASSWORD))
        .await
        .unwrap_err();

    assert!(is_invalid(&refused, "username"), "{refused:?}");
    server.stop().await;
}

#[tokio::test]
async fn without_a_session_me_is_no_session() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;

    let refused = bridge.state.me().await.unwrap_err();

    assert_eq!(refused.code, CommandErrorCode::NoSession);
    server.stop().await;
}

#[tokio::test]
async fn a_logout_ends_the_session_here_and_on_the_server() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;

    let answer = bridge.state.logout().await;
    let me = bridge.state.me().await.unwrap_err();

    assert_eq!(answer, LogoutResponse::Ended);
    assert_eq!(me.code, CommandErrorCode::NoSession);
    assert_eq!(bridge.keychain.stored(&server.url).await, None);
    server.stop().await;
}

#[tokio::test]
async fn a_logout_the_server_cant_confirm_still_ends_the_session_here() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let url = server.url.clone();
    server.stop().await;

    let answer = bridge.state.logout().await;

    assert!(
        matches!(&answer, LogoutResponse::Unconfirmed { error } if error.code == CommandErrorCode::Unreachable),
        "{answer:?}"
    );
    assert_eq!(bridge.keychain.stored(&url).await, None);
}

/// The start screen's way out (Plan.md P8.6, ADR-0017): a keychain entry the
/// app can't read makes every call answer `store`, and a logout removes it,
/// after which a login works. It pins existing behaviour: `Core::logout`
/// clears the store even when loading the token failed.
#[tokio::test]
async fn a_logout_removes_a_keychain_entry_that_cant_be_read() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mock = keyring_core::mock::Store::new().unwrap();
    let factory = KeychainFactory::new(mock.clone(), "io.example.test");
    let state = AppState::load(
        dir.path().join(PROFILE_FILE),
        factory,
        Arc::new(RecordingOpener::default()),
    );
    state.set_server_profile(server.profile()).await.unwrap();
    // Not UTF-8, so the keychain answers BadData for it.
    mock.build("io.example.test", server.url.as_str(), None)
        .unwrap()
        .set_secret(&[0xff, 0xfe, 0xfd])
        .unwrap();

    let me = state.me().await.unwrap_err();
    let answer = state.logout().await;
    let left = mock
        .build("io.example.test", server.url.as_str(), None)
        .unwrap()
        .get_secret();
    let step = state.login(login(OWNER, PASSWORD)).await;

    assert_eq!(me.code, CommandErrorCode::Store);
    assert_eq!(answer, LogoutResponse::Ended);
    assert!(
        matches!(left, Err(keyring_core::Error::NoEntry)),
        "{left:?}"
    );
    assert!(
        matches!(&step, Ok(step) if step.step == LoginStepKind::SetupRequired),
        "{step:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn a_logout_without_a_server_or_a_session_is_ended() {
    let none = Bridge::new();
    let server = Server::start().await;
    let set = Bridge::on(&server).await;

    assert_eq!(none.state.logout().await, LogoutResponse::Ended);
    assert_eq!(set.state.logout().await, LogoutResponse::Ended);
    server.stop().await;
}

#[tokio::test]
async fn a_step_up_with_the_wrong_password_is_403_and_the_right_one_passes() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;

    let wrong = bridge
        .state
        .step_up(StepUpRequest {
            password: SecretText::new("not the password at all".to_owned()),
        })
        .await
        .unwrap_err();
    let right = bridge
        .state
        .step_up(StepUpRequest {
            password: SecretText::new(PASSWORD.to_owned()),
        })
        .await;

    assert!(is_api(&wrong, 403, ErrorCode::Forbidden), "{wrong:?}");
    assert!(right.is_ok(), "{right:?}");
    server.stop().await;
}

#[tokio::test]
async fn an_invitee_registers_then_logs_in_and_sets_up_2fa() {
    let server = Server::start().await;
    let (owner, _app) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let invite = owner
        .state
        .create_invite(CreateInviteRequest {
            role: UserRole::Member,
        })
        .await
        .unwrap();
    let invitee = Bridge::on(&server).await;

    let registered = invitee
        .state
        .register(register(invite.code.expose(), "bob"))
        .await
        .unwrap();
    server.first_login(&invitee, "bob").await;
    let me = invitee.state.me().await.unwrap();

    assert_eq!(registered.username.as_str(), "bob");
    assert_eq!(me.username.as_str(), "bob");
    server.stop().await;
}

#[tokio::test]
async fn a_register_request_that_breaks_a_rule_is_invalid_input() {
    let server = Server::start().await;
    let bridge = Bridge::on(&server).await;

    let refused = bridge
        .state
        .register(register("afk_iv_unknown", &"b".repeat(33)))
        .await
        .unwrap_err();

    assert!(is_invalid(&refused, "username"), "{refused:?}");
    server.stop().await;
}

#[tokio::test]
async fn without_a_server_every_command_is_no_server() {
    let bridge = Bridge::new();

    let errors = [
        bridge
            .state
            .login(login(OWNER, PASSWORD))
            .await
            .unwrap_err(),
        bridge
            .state
            .login_mfa(SecretText::new("123456".to_owned()))
            .await
            .unwrap_err(),
        bridge.state.me().await.unwrap_err(),
        bridge.state.mfa_enroll().await.unwrap_err(),
        bridge.state.mfa_reset().await.unwrap_err(),
        bridge.state.check_server().await.unwrap_err(),
        bridge
            .state
            .register(register("afk_iv_unknown", "bob"))
            .await
            .unwrap_err(),
        bridge
            .state
            .step_up(StepUpRequest {
                password: SecretText::new(PASSWORD.to_owned()),
            })
            .await
            .unwrap_err(),
    ];

    for error in errors {
        assert_eq!(error.code, CommandErrorCode::NoServer, "{error:?}");
    }
}
