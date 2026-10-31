//! The admin screens' commands (Plan.md P11.9; ADR-0020, U77): the audit log
//! and its filter, the agents with disabling and releasing, the Owner's grants
//! review, and leaving a shared account, against the real server.

use fleet_api_types::accounts::{CreateGrantRequest, CreateOfflineAccountRequest, GrantLevel};
use fleet_api_types::agents::AgentStatus;
use fleet_api_types::app::CommandErrorCode;
use fleet_api_types::{ErrorCode, SecretText};

use crate::support::{Server, is_api, is_invalid, step_up_request};

fn offline(name: &str) -> CreateOfflineAccountRequest {
    CreateOfflineAccountRequest {
        name: SecretText::new(name.to_owned()),
    }
}

#[tokio::test]
async fn the_audit_log_pages_and_narrows_to_a_target_for_admins_only() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let (bob, bob_id) = server.member(&owner, "bob").await;
    let bob_text = bob_id.to_string();

    let page = owner.state.audit(None, Some(2), None, None).await.unwrap();
    let narrowed = owner
        .state
        .audit(None, None, Some("user".to_owned()), Some(bob_text.clone()))
        .await
        .unwrap();
    let half = owner
        .state
        .audit(None, None, Some("user".to_owned()), None)
        .await
        .unwrap_err();
    let refused = bob.state.audit(None, None, None, None).await.unwrap_err();

    assert_eq!(page.items.len(), 2);
    assert_ne!(page.next, None);
    assert_ne!(narrowed.items.len(), 0);
    for entry in &narrowed.items {
        assert_eq!(entry.target.as_ref().unwrap().id.as_str(), bob_text);
    }
    assert!(is_invalid(&half, "target_id"), "{half:?}");
    assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
    server.stop().await;
}

#[tokio::test]
async fn an_agent_lists_is_disabled_and_its_bots_released() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap();
    let bot = owner.state.bots(None, None).await.unwrap().items[0].id;
    let agent = server.agent_holding(bot, 1).await;

    let unstepped = owner.state.disable_agent(agent).await.unwrap_err();
    owner.state.step_up(step_up_request()).await.unwrap();
    let not_yet = owner.state.release_agent(agent).await.unwrap_err();
    let listed = owner.state.agents(None, None).await.unwrap();
    let disabled = owner.state.disable_agent(agent).await.unwrap();
    let released = owner.state.release_agent(agent).await.unwrap();
    let bad_limit = owner.state.agents(None, Some(0)).await.unwrap_err();

    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert!(
        is_api(&not_yet, 409, ErrorCode::AgentNotDisabled),
        "{not_yet:?}"
    );
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].id, agent);
    assert_eq!(disabled.status.known(), Some(AgentStatus::Disabled));
    assert_eq!(disabled.bots, 1);
    assert_eq!(released.bots, 0);
    assert_eq!(bad_limit.code, CommandErrorCode::InvalidInput);
    server.stop().await;
}

#[tokio::test]
async fn the_owner_reviews_a_users_grants_and_a_grantee_leaves() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let (bob, bob_id) = server.member(&owner, "bob").await;
    let (carol, carol_id) = server.member(&owner, "carol").await;
    bob.state.step_up(step_up_request()).await.unwrap();
    let bobs = bob
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap()
        .id;
    bob.state
        .create_grant(
            bobs,
            CreateGrantRequest {
                username: SecretText::new("carol".to_owned()),
                level: GrantLevel::View,
            },
        )
        .await
        .unwrap();

    let held = owner.state.user_grants(carol_id, None, None).await.unwrap();
    let shared = owner
        .state
        .shared_accounts(bob_id, None, None)
        .await
        .unwrap();
    let refused = carol
        .state
        .user_grants(bob_id, None, None)
        .await
        .unwrap_err();
    let unstepped = carol.state.leave_account(bobs).await.unwrap_err();
    carol.state.step_up(step_up_request()).await.unwrap();
    carol.state.leave_account(bobs).await.unwrap();
    let again = carol.state.leave_account(bobs).await.unwrap_err();
    let held_after = owner.state.user_grants(carol_id, None, None).await.unwrap();

    assert_eq!(held.items.len(), 1);
    assert_eq!(held.items[0].account.id, bobs);
    assert!(!held.items[0].dormant);
    assert_eq!(shared.items.len(), 1);
    assert_eq!(shared.items[0].id, bobs);
    assert!(is_api(&refused, 404, ErrorCode::NotFound), "{refused:?}");
    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert!(is_api(&again, 404, ErrorCode::NotFound), "{again:?}");
    assert_eq!(held_after.items.len(), 0);
    server.stop().await;
}
