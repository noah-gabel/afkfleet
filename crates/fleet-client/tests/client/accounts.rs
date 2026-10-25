//! The Microsoft accounts' routes (Plan.md P9.4, P9.6, P9.7). What needs
//! Microsoft to succeed (a link's 201, its later states) is tested over the
//! scripted transport in `session.rs`; here every start ends in a refusal
//! the server gives before it asks Microsoft, since the real adapter is
//! wired (ADR-0018, group F).

use fleet_api_types::accounts::{
    AccountKind, AccountStatus, ChangeGrantRequest, CreateGrantRequest,
    CreateOfflineAccountRequest, GrantLevel, UNGRANTABLE,
};
use fleet_api_types::{AccountId, ErrorCode, LinkFlowId, PageQuery, SecretText};
use fleet_client::{Client, MemoryStore};

use crate::support::{Server, is_api, step_up};

fn all<I>() -> PageQuery<I> {
    PageQuery {
        before: None,
        limit: None,
    }
}

fn offline(name: &str) -> CreateOfflineAccountRequest {
    CreateOfflineAccountRequest {
        name: SecretText::new(name.to_owned()),
    }
}

async fn create(client: &Client<MemoryStore>, name: &str) -> AccountId {
    client
        .create_offline_account(&offline(name))
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn a_members_starts_are_refused_before_microsoft_is_asked() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let kept = create(&alice, "AfkBot1").await;
    create(&alice, "AfkBot2").await;
    create(&alice, "AfkBot3").await;

    let at_quota = alice.start_link().await.unwrap_err();
    let offline_again = alice.relink_account(kept).await.unwrap_err();
    let unknown_flow = alice
        .link_status(
            "01900000-0000-7000-8000-0000000000ff"
                .parse::<LinkFlowId>()
                .unwrap(),
        )
        .await
        .unwrap_err();

    assert!(is_api(&at_quota, 409, ErrorCode::Conflict), "{at_quota:?}");
    assert!(
        is_api(&offline_again, 409, ErrorCode::Conflict),
        "{offline_again:?}"
    );
    assert!(
        is_api(&unknown_flow, 404, ErrorCode::NotFound),
        "{unknown_flow:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn an_offline_account_is_created_listed_read_and_deleted_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;

    let created = alice
        .create_offline_account(&offline("AfkBot1"))
        .await
        .unwrap();
    let taken = alice
        .create_offline_account(&offline("afkbot1"))
        .await
        .unwrap_err();
    let listed = alice.accounts(&all()).await.unwrap();
    let read = alice.account(created.id).await.unwrap();
    let unstepped = alice.delete_account(created.id).await.unwrap_err();
    step_up(&alice).await;
    alice.delete_account(created.id).await.unwrap();
    let gone = alice.account(created.id).await.unwrap_err();

    assert_eq!(created.name.as_str(), "AfkBot1");
    assert_eq!(created.kind.known(), Some(AccountKind::Offline));
    assert_eq!(created.status.known(), Some(AccountStatus::Active));
    assert!(is_api(&taken, 409, ErrorCode::Conflict), "{taken:?}");
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items.first(), Some(&read));
    assert_eq!(read.id, created.id);
    assert_eq!(read.level.known(), Some(GrantLevel::Manage));
    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert!(is_api(&gone, 404, ErrorCode::NotFound), "{gone:?}");
    server.stop().await;
}

#[tokio::test]
async fn the_list_pages_with_before_and_limit() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    create(&owner, "AfkBot1").await;
    create(&owner, "AfkBot2").await;

    let first = owner
        .accounts(&PageQuery {
            before: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    let second = owner
        .accounts(&PageQuery {
            before: first.next,
            limit: Some(1),
        })
        .await
        .unwrap();
    let out_of_range = owner
        .accounts(&PageQuery {
            before: None,
            limit: Some(0),
        })
        .await
        .unwrap_err();

    assert_eq!(first.items.len(), 1);
    assert!(first.next.is_some());
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items.first(), second.items.first());
    assert!(
        is_api(&out_of_range, 422, ErrorCode::ValidationFailed),
        "{out_of_range:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn the_owner_reviews_a_members_pending_links_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, alice_id) = server.member(&owner, "alice").await;
    let approved = server.pending_account(alice_id, 1).await;
    let refused = server.pending_account(alice_id, 2).await;
    server.advance(core::time::Duration::from_secs(301));

    let awaiting = owner.accounts_awaiting_approval(&all()).await.unwrap();
    let members = alice.accounts_awaiting_approval(&all()).await.unwrap();
    let unstepped = owner.approve_account(approved).await.unwrap_err();
    step_up(&owner).await;
    let active = owner.approve_account(approved).await.unwrap();
    owner.refuse_account(refused).await.unwrap();
    let again = owner.approve_account(approved).await.unwrap_err();
    let deleted = owner.account(refused).await.unwrap_err();

    let mut waiting: Vec<_> = awaiting.items.iter().map(|account| account.id).collect();
    waiting.sort();
    let mut expected = vec![approved, refused];
    expected.sort();
    assert_eq!(waiting, expected);
    assert_eq!(members.items, []);
    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert_eq!(active.id, approved);
    assert_eq!(active.status.known(), Some(AccountStatus::Active));
    assert!(is_api(&again, 409, ErrorCode::Conflict), "{again:?}");
    assert!(is_api(&deleted, 404, ErrorCode::NotFound), "{deleted:?}");
    server.stop().await;
}

#[tokio::test]
async fn grants_are_created_by_name_changed_listed_and_revoked() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (_bob, bob_id) = server.member(&owner, "bob").await;
    let account = create(&alice, "AfkBot1").await;
    step_up(&alice).await;
    let to_bob = CreateGrantRequest {
        username: SecretText::new("bob".to_owned()),
        level: GrantLevel::View,
    };
    let to_nobody = CreateGrantRequest {
        username: SecretText::new("nobody".to_owned()),
        level: GrantLevel::View,
    };

    let created = alice.create_grant(account, &to_bob).await.unwrap();
    let twice = alice.create_grant(account, &to_bob).await.unwrap_err();
    let unknown = alice.create_grant(account, &to_nobody).await.unwrap_err();
    let changed = alice
        .change_grant(
            account,
            bob_id,
            &ChangeGrantRequest {
                level: GrantLevel::Control,
            },
        )
        .await
        .unwrap();
    let listed = alice.account_grants(account, &all()).await.unwrap();
    alice.revoke_grant(account, bob_id).await.unwrap();
    let after = alice.account_grants(account, &all()).await.unwrap();
    let missing = alice.revoke_grant(account, bob_id).await.unwrap_err();

    assert_eq!(created.user.id, bob_id);
    assert_eq!(created.level.known(), Some(GrantLevel::View));
    assert!(is_api(&twice, 409, ErrorCode::Conflict), "{twice:?}");
    assert!(
        is_api(&unknown, 422, ErrorCode::ValidationFailed),
        "{unknown:?}"
    );
    let fields = &unknown.api().unwrap().fields;
    assert_eq!(fields.len(), 1);
    assert_eq!(fields.first().unwrap().path.as_str(), "username");
    assert_eq!(fields.first().unwrap().message.as_str(), UNGRANTABLE);
    assert_eq!(changed.level.known(), Some(GrantLevel::Control));
    assert_eq!(listed.items, [changed]);
    assert_eq!(after.items, []);
    assert!(is_api(&missing, 404, ErrorCode::NotFound), "{missing:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_grantee_leaves_a_shared_account_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (bob, _) = server.member(&owner, "bob").await;
    let account = create(&alice, "AfkBot1").await;
    step_up(&alice).await;
    alice
        .create_grant(
            account,
            &CreateGrantRequest {
                username: SecretText::new("bob".to_owned()),
                level: GrantLevel::View,
            },
        )
        .await
        .unwrap();

    let unstepped = bob.leave_account(account).await.unwrap_err();
    step_up(&bob).await;
    bob.leave_account(account).await.unwrap();
    let listed = bob.accounts(&all()).await.unwrap();
    let again = bob.leave_account(account).await.unwrap_err();
    let by_owner = alice.leave_account(account).await.unwrap_err();

    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert_eq!(listed.items, []);
    assert!(is_api(&again, 404, ErrorCode::NotFound), "{again:?}");
    assert!(is_api(&by_owner, 403, ErrorCode::Forbidden), "{by_owner:?}");
    server.stop().await;
}
