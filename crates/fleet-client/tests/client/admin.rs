//! The admins' routes: users and invites.

use core::time::Duration;

use fleet_api_types::invites::{CreateInviteRequest, InviteState};
use fleet_api_types::users::{UserPatchRequest, UserRole};
use fleet_api_types::{ErrorCode, PageQuery, UserId};
use fleet_client::ClientError;

use crate::support::{PASSWORD, Server, is_api, register};

fn all<I>() -> PageQuery<I> {
    PageQuery {
        before: None,
        limit: None,
    }
}

#[tokio::test]
async fn the_owner_manages_invites_and_users() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let member = CreateInviteRequest {
        role: UserRole::Member,
    };

    let used = owner.create_invite(&member).await.unwrap();
    let spare = owner.create_invite(&member).await.unwrap();
    let invites = owner.invites(&all()).await.unwrap();
    let (bob, _) = server.client();
    let registered = bob
        .register(&register(used.code.expose(), "bob", PASSWORD))
        .await
        .unwrap();
    owner.revoke_invite(spare.invite.id).await.unwrap();
    let revoke_used = owner.revoke_invite(used.invite.id).await.unwrap_err();
    let after = owner.invites(&all()).await.unwrap();

    assert_eq!(invites.items.len(), 2);
    assert!(
        invites
            .items
            .iter()
            .all(|invite| invite.state.known() == Some(InviteState::Pending))
    );
    assert!(
        is_api(&revoke_used, 409, ErrorCode::Conflict),
        "{revoke_used:?}"
    );
    let state_of = |id| {
        after
            .items
            .iter()
            .find(|invite| invite.id == id)
            .and_then(|invite| invite.state.known())
    };
    assert_eq!(state_of(used.invite.id), Some(InviteState::Used));
    assert_eq!(state_of(spare.invite.id), Some(InviteState::Revoked));

    let users = owner.users(&all()).await.unwrap();
    let disabled = owner
        .patch_user(
            registered.id,
            &UserPatchRequest {
                role: None,
                disabled: Some(true),
            },
        )
        .await
        .unwrap();

    assert_eq!(users.items.len(), 2);
    assert!(disabled.disabled);
    assert_eq!(disabled.id, registered.id);
    server.stop().await;
}

#[tokio::test]
async fn an_unknown_user_is_not_found_and_a_member_cant_list_users() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let code = owner
        .create_invite(&CreateInviteRequest {
            role: UserRole::Member,
        })
        .await
        .unwrap()
        .code;
    let (bob, _) = server.client();
    bob.register(&register(code.expose(), "bob", PASSWORD))
        .await
        .unwrap();
    server.advance(Duration::from_secs(60));
    server.first_login(&bob, "bob").await;
    let unknown: UserId = "01900000-0000-7000-8000-000000000001".parse().unwrap();

    let missing = owner
        .patch_user(
            unknown,
            &UserPatchRequest {
                role: Some(UserRole::Admin),
                disabled: None,
            },
        )
        .await
        .unwrap_err();
    let hidden = bob.users(&all()).await.unwrap_err();

    assert!(is_api(&missing, 404, ErrorCode::NotFound), "{missing:?}");
    assert!(is_api(&hidden, 403, ErrorCode::Forbidden), "{hidden:?}");
    assert!(!matches!(hidden, ClientError::NoSession));
    bob.me().await.unwrap();
    server.stop().await;
}
