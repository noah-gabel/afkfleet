//! The user's own account and the admins' routes through the bridge.

use fleet_api_types::auth::NEW_PASSWORD_MAX_CHARS;
use fleet_api_types::invites::{CreateInviteRequest, InviteState};
use fleet_api_types::me::PasswordChangeRequest;
use fleet_api_types::page::PAGE_MAX;
use fleet_api_types::users::{UserPatchRequest, UserRole};
use fleet_api_types::{ErrorCode, SecretText, UserId};

use crate::support::{
    Bridge, Server, app_of, assert_no_token, is_api, is_invalid, register, step_up_request,
};

#[tokio::test]
async fn the_session_list_marks_the_current_session_which_cant_be_revoked_here() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;

    let page = bridge.state.sessions(None, Some(10)).await.unwrap();
    let current = page.items.iter().find(|session| session.current).unwrap();
    let refused = bridge.state.revoke_session(current.id).await.unwrap_err();

    assert_eq!(page.items.len(), 1);
    assert!(is_api(&refused, 409, ErrorCode::Conflict), "{refused:?}");
    server.stop().await;
}

#[tokio::test]
async fn another_session_of_the_user_is_revoked() {
    let server = Server::start().await;
    let (first, app) = server.owner().await;
    let second = Bridge::on(&server).await;
    server.log_in(&second, "owner", &app).await;

    let page = first.state.sessions(None, None).await.unwrap();
    let other = page.items.iter().find(|session| !session.current).unwrap();
    first.state.revoke_session(other.id).await.unwrap();
    let gone = second.state.me().await.unwrap_err();

    assert_eq!(page.items.len(), 2);
    assert_eq!(gone.code, fleet_api_types::app::CommandErrorCode::NoSession);
    server.stop().await;
}

#[tokio::test]
async fn a_page_size_out_of_range_is_invalid_input_before_any_request() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;

    let zero = bridge.state.sessions(None, Some(0)).await.unwrap_err();
    let over = bridge
        .state
        .users(None, Some(PAGE_MAX + 1))
        .await
        .unwrap_err();
    let invites = bridge.state.invites(None, Some(0)).await.unwrap_err();

    assert!(is_invalid(&zero, "limit"), "{zero:?}");
    assert!(is_invalid(&over, "limit"), "{over:?}");
    assert!(is_invalid(&invites, "limit"), "{invites:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_new_password_needs_a_step_up_first() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;
    let change = || PasswordChangeRequest {
        new_password: SecretText::new("another strong passphrase here".to_owned()),
    };

    let refused = bridge.state.change_password(change()).await.unwrap_err();
    bridge.state.step_up(step_up_request()).await.unwrap();
    let changed = bridge.state.change_password(change()).await;

    assert!(
        is_api(&refused, 403, ErrorCode::StepUpRequired),
        "{refused:?}"
    );
    assert!(changed.is_ok(), "{changed:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_new_password_that_breaks_a_rule_is_invalid_input() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;

    let refused = bridge
        .state
        .change_password(PasswordChangeRequest {
            new_password: SecretText::new("a".repeat(NEW_PASSWORD_MAX_CHARS + 1)),
        })
        .await
        .unwrap_err();

    assert!(is_invalid(&refused, "new_password"), "{refused:?}");
    server.stop().await;
}

#[tokio::test]
async fn replacing_the_authenticator_starts_a_setup_step_in_the_session() {
    let server = Server::start().await;
    let (bridge, _app) = server.owner().await;

    let refused = bridge.state.mfa_reset().await.unwrap_err();
    bridge.state.step_up(step_up_request()).await.unwrap();
    let step = bridge.state.mfa_reset().await.unwrap();
    let enrollment = bridge.state.mfa_enroll().await.unwrap();
    let codes = bridge
        .state
        .mfa_confirm(server.code(&app_of(&enrollment)))
        .await
        .unwrap();
    let me = bridge.state.me().await.unwrap();

    assert!(
        is_api(&refused, 403, ErrorCode::StepUpRequired),
        "{refused:?}"
    );
    assert!(step.expires_in > 0);
    assert_eq!(codes.recovery_codes.len(), 10);
    assert_eq!(me.recovery_codes_left, 10);
    assert_no_token(&step);
    server.stop().await;
}

#[tokio::test]
async fn the_owner_manages_invites_and_users() {
    let server = Server::start().await;
    let (owner, _app) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let member = CreateInviteRequest {
        role: UserRole::Member,
    };

    let used = owner.state.create_invite(member).await.unwrap();
    let spare = owner.state.create_invite(member).await.unwrap();
    let invitee = Bridge::on(&server).await;
    let bob = invitee
        .state
        .register(register(used.code.expose(), "bob"))
        .await
        .unwrap();
    owner.state.revoke_invite(spare.invite.id).await.unwrap();
    let revoke_used = owner.state.revoke_invite(used.invite.id).await.unwrap_err();
    let listed = owner.state.invites(None, Some(10)).await.unwrap();
    let users = owner.state.users(None, None).await.unwrap();
    let disabled = owner
        .state
        .patch_user(
            bob.id,
            UserPatchRequest {
                role: None,
                disabled: Some(true),
            },
        )
        .await
        .unwrap();

    assert!(
        is_api(&revoke_used, 409, ErrorCode::Conflict),
        "{revoke_used:?}"
    );
    let state_of = |id| {
        listed
            .items
            .iter()
            .find(|invite| invite.id == id)
            .and_then(|invite| invite.state.known())
    };
    assert_eq!(state_of(used.invite.id), Some(InviteState::Used));
    assert_eq!(state_of(spare.invite.id), Some(InviteState::Revoked));
    assert_eq!(users.items.len(), 2);
    assert!(disabled.disabled);
    server.stop().await;
}

#[tokio::test]
async fn a_patch_without_exactly_one_change_is_invalid_input_before_any_request() {
    let server = Server::start().await;
    let (owner, _app) = server.owner().await;
    let someone = owner.state.me().await.unwrap().id;

    let both = owner
        .state
        .patch_user(
            someone,
            UserPatchRequest {
                role: Some(UserRole::Admin),
                disabled: Some(true),
            },
        )
        .await
        .unwrap_err();

    assert!(is_invalid(&both, ""), "{both:?}");
    server.stop().await;
}

#[tokio::test]
async fn an_unknown_user_is_not_found() {
    let server = Server::start().await;
    let (owner, _app) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let unknown: UserId =
        serde_json::from_value(serde_json::json!("01900000-0000-7000-8000-0000000000ff")).unwrap();

    let refused = owner
        .state
        .patch_user(
            unknown,
            UserPatchRequest {
                role: None,
                disabled: Some(true),
            },
        )
        .await
        .unwrap_err();

    assert!(is_api(&refused, 404, ErrorCode::NotFound), "{refused:?}");
    server.stop().await;
}
