//! The admins' routes: users, invites, agents and the audit log.

use core::time::Duration;

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::agents::AgentStatus;
use fleet_api_types::audit::{AuditActionName, AuditPageQuery};
use fleet_api_types::invites::{CreateInviteRequest, InviteState};
use fleet_api_types::users::{UserPatchRequest, UserRole};
use fleet_api_types::{ErrorCode, PageQuery, SecretText, UserId};
use fleet_client::ClientError;

use crate::support::{PASSWORD, Server, is_api, register, step_up};

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
    step_up(&owner).await;
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
    step_up(&owner).await;
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

/// The Owner reads the audit log a page at a time, following `next`, and
/// finds the invite and the Member's registration; the Member is refused.
#[tokio::test]
async fn the_owner_pages_through_the_audit_log_and_a_member_is_refused() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (bob, _) = server.member(&owner, "bob").await;

    let mut actions = Vec::new();
    let mut before = None;
    loop {
        let page = owner
            .audit(&AuditPageQuery {
                before,
                limit: Some(2),
                target_kind: None,
                target_id: None,
            })
            .await
            .unwrap();
        assert!(page.items.len() <= 2);
        actions.extend(page.items.iter().filter_map(|entry| entry.action.known()));
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    let refused = bob
        .audit(&AuditPageQuery {
            before: None,
            limit: None,
            target_kind: None,
            target_id: None,
        })
        .await
        .unwrap_err();

    assert!(
        actions.contains(&AuditActionName::InviteCreate),
        "{actions:?}"
    );
    assert!(
        actions.contains(&AuditActionName::UserCreate),
        "{actions:?}"
    );
    assert!(actions.len() > 2, "more than one page: {actions:?}");
    assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
    server.stop().await;
}

/// An Admin's disabling asks for the step-up, then ends the agent for good;
/// a Member is refused, and disabling again answers it as it is.
#[tokio::test]
async fn the_owner_disables_an_agent_after_a_step_up_and_a_member_is_refused() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    owner
        .create_offline_account(&CreateOfflineAccountRequest {
            name: SecretText::new("AfkBot1".to_owned()),
        })
        .await
        .unwrap();
    let bot = owner.bots(&all()).await.unwrap().items[0].id;
    let agent = server.agent_holding(bot, 1, false).await;

    let unstepped = owner.disable_agent(agent).await.unwrap_err();
    step_up(&owner).await;
    let (bob, _) = server.member(&owner, "bob").await;
    let disabled = owner.disable_agent(agent).await.unwrap();
    let again = owner.disable_agent(agent).await.unwrap();
    step_up(&bob).await;
    let refused = bob.disable_agent(agent).await.unwrap_err();

    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert_eq!(disabled.id, agent);
    assert!(!disabled.enabled);
    assert_eq!(disabled.status.known(), Some(AgentStatus::Disabled));
    assert_eq!(disabled.bots, 1, "its bot stays until it's released");
    assert_eq!(again, disabled);
    assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
    server.stop().await;
}

/// The target's kind and ID reach the server: the Owner's page narrowed to
/// the Member they invited holds only that Member's entries (ADR-0020, S89).
#[tokio::test]
async fn a_filtered_audit_page_reaches_the_server() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (bob, _) = server.member(&owner, "bob").await;
    let bob_id = bob.me().await.unwrap().id.to_string();

    let page = owner
        .audit(&AuditPageQuery {
            before: None,
            limit: None,
            target_kind: Some("user".to_owned()),
            target_id: Some(bob_id.clone()),
        })
        .await
        .unwrap();

    assert_ne!(page.items.len(), 0);
    for entry in &page.items {
        let target = entry.target.as_ref().unwrap();
        assert_eq!(target.kind.as_str(), "user");
        assert_eq!(target.id.as_str(), bob_id);
    }
    server.stop().await;
}

/// The Owner lists the agents with their bots, releases a disabled one's
/// bot after a step-up, and an enabled one is refused with its code.
#[tokio::test]
async fn the_owner_releases_a_disabled_agents_bots_and_an_enabled_one_is_refused() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let mut bots = Vec::new();
    for name in ["AfkBot1", "AfkBot2"] {
        owner
            .create_offline_account(&CreateOfflineAccountRequest {
                name: SecretText::new(name.to_owned()),
            })
            .await
            .unwrap();
    }
    for bot in owner.bots(&all()).await.unwrap().items {
        bots.push(bot.id);
    }
    let disabled = server.agent_holding(bots[0], 1, true).await;
    let enabled = server.agent_holding(bots[1], 2, false).await;
    step_up(&owner).await;

    let before = owner.agents(&all()).await.unwrap();
    let released = owner.release_agent(disabled).await.unwrap();
    let refused = owner.release_agent(enabled).await.unwrap_err();

    let held = |id| {
        before
            .items
            .iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.bots)
    };
    assert_eq!(held(disabled), Some(1));
    assert_eq!(released.id, disabled);
    assert_eq!(released.bots, 0);
    assert_eq!(released.status.known(), Some(AgentStatus::Disabled));
    assert!(
        is_api(&refused, 409, ErrorCode::AgentNotDisabled),
        "{refused:?}"
    );
    server.stop().await;
}
