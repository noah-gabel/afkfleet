//! The Microsoft accounts' commands (Plan.md P9.8). Without Microsoft, every
//! start here ends in a refusal the server gives before it asks Microsoft;
//! the link a good start holds, and the opener, are tested in-crate
//! (`endpoints.rs`), where an answer can be made up (ADR-0018, group F).

use core::time::Duration;

use fleet_api_types::accounts::{
    AccountKind, AccountStatus, ChangeGrantRequest, CreateGrantRequest,
    CreateOfflineAccountRequest, GrantLevel, UNGRANTABLE,
};
use fleet_api_types::app::CommandErrorCode;
use fleet_api_types::page::PAGE_MAX;
use fleet_api_types::{ErrorCode, SecretText};

use crate::support::{Bridge, Server, is_api, is_invalid, step_up_request};

fn offline(name: &str) -> CreateOfflineAccountRequest {
    CreateOfflineAccountRequest {
        name: SecretText::new(name.to_owned()),
    }
}

#[tokio::test]
async fn without_a_server_every_account_command_is_no_server_and_nothing_opens() {
    let bridge = Bridge::new();
    let id = "01900000-0000-7000-8000-00000000000a".parse().unwrap();

    let answers = [
        bridge.state.start_link().await.map(drop),
        bridge.state.relink_account(id).await.map(drop),
        bridge.state.accounts(None, None).await.map(drop),
        bridge.state.account(id).await.map(drop),
        bridge.state.delete_account(id).await,
    ];
    let open = bridge.state.open_verification_link().unwrap_err();

    for answer in answers {
        assert_eq!(answer.unwrap_err().code, CommandErrorCode::NoServer);
    }
    assert_eq!(open.code, CommandErrorCode::NoVerificationLink);
    assert_eq!(bridge.opener.opened(), Vec::<String>::new());
}

#[tokio::test]
async fn a_refused_start_holds_no_link_and_the_status_of_an_unknown_flow_is_404() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let (alice, _) = server.member(&owner, "alice").await;
    let kept = alice
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap();
    for name in ["AfkBot2", "AfkBot3"] {
        alice
            .state
            .create_offline_account(offline(name))
            .await
            .unwrap();
    }

    let at_quota = alice.state.start_link().await.unwrap_err();
    let offline_again = alice.state.relink_account(kept.id).await.unwrap_err();
    let unknown = alice
        .state
        .link_status("01900000-0000-7000-8000-0000000000ff".parse().unwrap())
        .await
        .unwrap_err();
    let open = alice.state.open_verification_link().unwrap_err();

    assert!(is_api(&at_quota, 409, ErrorCode::Conflict), "{at_quota:?}");
    assert!(
        is_api(&offline_again, 409, ErrorCode::Conflict),
        "{offline_again:?}"
    );
    assert!(is_api(&unknown, 404, ErrorCode::NotFound), "{unknown:?}");
    assert_eq!(open.code, CommandErrorCode::NoVerificationLink);
    assert_eq!(alice.opener.opened(), Vec::<String>::new());
    server.stop().await;
}

#[tokio::test]
async fn an_offline_account_is_created_listed_read_and_deleted_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;

    let created = owner
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap();
    let bad_name = owner
        .state
        .create_offline_account(offline("no spaces"))
        .await
        .unwrap_err();
    let listed = owner.state.accounts(None, Some(10)).await.unwrap();
    let read = owner.state.account(created.id).await.unwrap();
    let unstepped = owner.state.delete_account(created.id).await.unwrap_err();
    owner.state.step_up(step_up_request()).await.unwrap();
    owner.state.delete_account(created.id).await.unwrap();
    let gone = owner.state.account(created.id).await.unwrap_err();

    assert_eq!(created.kind.known(), Some(AccountKind::Offline));
    assert!(is_invalid(&bad_name, "name"), "{bad_name:?}");
    assert_eq!(listed.items, std::slice::from_ref(&read));
    assert_eq!(read.name.as_str(), "AfkBot1");
    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert!(is_api(&gone, 404, ErrorCode::NotFound), "{gone:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_page_out_of_range_is_refused_before_any_request() {
    let bridge = Bridge::new();
    let id = "01900000-0000-7000-8000-00000000000a".parse().unwrap();

    for limit in [0, PAGE_MAX + 1] {
        let accounts = bridge.state.accounts(None, Some(limit)).await.unwrap_err();
        let awaiting = bridge
            .state
            .accounts_awaiting_approval(None, Some(limit))
            .await
            .unwrap_err();
        let grants = bridge
            .state
            .account_grants(id, None, Some(limit))
            .await
            .unwrap_err();
        assert!(is_invalid(&accounts, "limit"), "{accounts:?}");
        assert!(is_invalid(&awaiting, "limit"), "{awaiting:?}");
        assert!(is_invalid(&grants, "limit"), "{grants:?}");
    }
}

#[tokio::test]
async fn the_owner_reviews_a_members_pending_links_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let (alice, alice_id) = server.member(&owner, "alice").await;
    let approved = server.pending_account(alice_id, 1).await;
    let refused = server.pending_account(alice_id, 2).await;
    server.advance(Duration::from_secs(301));

    let awaiting = owner
        .state
        .accounts_awaiting_approval(None, None)
        .await
        .unwrap();
    let members = alice
        .state
        .accounts_awaiting_approval(None, None)
        .await
        .unwrap();
    let unstepped = owner.state.approve_account(approved).await.unwrap_err();
    owner.state.step_up(step_up_request()).await.unwrap();
    let active = owner.state.approve_account(approved).await.unwrap();
    owner.state.refuse_account(refused).await.unwrap();
    let again = owner.state.refuse_account(approved).await.unwrap_err();

    assert_eq!(awaiting.items.len(), 2);
    assert_eq!(members.items, []);
    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert_eq!(active.status.known(), Some(AccountStatus::Active));
    assert!(is_api(&again, 409, ErrorCode::Conflict), "{again:?}");
    server.stop().await;
}

#[tokio::test]
async fn grants_are_created_by_name_changed_listed_and_revoked() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner.state.step_up(step_up_request()).await.unwrap();
    let (_bob, bob_id) = server.member(&owner, "bob").await;
    let account = owner
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap()
        .id;
    let grant = |username: &str| CreateGrantRequest {
        username: SecretText::new(username.to_owned()),
        level: GrantLevel::View,
    };

    let created = owner
        .state
        .create_grant(account, grant("bob"))
        .await
        .unwrap();
    let unknown = owner
        .state
        .create_grant(account, grant("nobody"))
        .await
        .unwrap_err();
    let bad_name = owner
        .state
        .create_grant(account, grant("x"))
        .await
        .unwrap_err();
    let changed = owner
        .state
        .change_grant(
            account,
            bob_id,
            ChangeGrantRequest {
                level: GrantLevel::Manage,
            },
        )
        .await
        .unwrap();
    let listed = owner
        .state
        .account_grants(account, None, None)
        .await
        .unwrap();
    owner.state.revoke_grant(account, bob_id).await.unwrap();
    let missing = owner.state.revoke_grant(account, bob_id).await.unwrap_err();

    assert_eq!(created.user.id, bob_id);
    assert!(
        is_api(&unknown, 422, ErrorCode::ValidationFailed),
        "{unknown:?}"
    );
    let fields = unknown.api.as_ref().unwrap().fields.clone().unwrap();
    assert_eq!(fields.first().unwrap().message.as_str(), UNGRANTABLE);
    assert!(is_invalid(&bad_name, "username"), "{bad_name:?}");
    assert_eq!(changed.level.known(), Some(GrantLevel::Manage));
    assert_eq!(listed.items, [changed]);
    assert!(is_api(&missing, 404, ErrorCode::NotFound), "{missing:?}");
    server.stop().await;
}
