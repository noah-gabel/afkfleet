//! The user's own routes: the profile, stepping up, the password and the
//! sessions.

use fleet_api_types::auth::StepUpRequest;
use fleet_api_types::me::PasswordChangeRequest;
use fleet_api_types::{ErrorCode, PageQuery, SecretText, SessionId};
use fleet_client::ClientError;

use crate::support::{OWNER, PASSWORD, Server, is_api, stored};

fn step_up(password: &str) -> StepUpRequest {
    StepUpRequest {
        password: SecretText::new(password.to_owned()),
    }
}

fn all<I>() -> PageQuery<I> {
    PageQuery {
        before: None,
        limit: None,
    }
}

#[tokio::test]
async fn sessions_are_listed_and_another_one_is_ended() {
    let server = Server::start().await;
    let (owner, _, app) = server.owner().await;
    let (other, other_store) = server.client();
    server.log_in(&other, OWNER, &app).await;

    let before = owner.sessions(&all()).await.unwrap();
    let current = before
        .items
        .iter()
        .find(|session| session.current)
        .unwrap()
        .id;
    let theirs = before
        .items
        .iter()
        .find(|session| !session.current)
        .unwrap()
        .id;
    owner.revoke_session(theirs).await.unwrap();
    let after = owner.sessions(&all()).await.unwrap();
    let ended = other.me().await.unwrap_err();
    let own = owner.revoke_session(current).await.unwrap_err();

    assert_eq!(before.items.len(), 2);
    assert_eq!(after.items.len(), 1);
    assert!(matches!(ended, ClientError::NoSession), "{ended:?}");
    assert_eq!(stored(&other_store).await, None);
    assert!(is_api(&own, 409, ErrorCode::Conflict), "{own:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_page_query_reaches_the_server() {
    let server = Server::start().await;
    let (owner, _, app) = server.owner().await;
    let (other, _) = server.client();
    server.log_in(&other, OWNER, &app).await;

    let first = owner
        .sessions(&PageQuery {
            before: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    let second = owner
        .sessions(&PageQuery {
            before: first.next,
            limit: Some(1),
        })
        .await
        .unwrap();
    let out_of_range = owner
        .sessions(&PageQuery::<SessionId> {
            before: None,
            limit: Some(101),
        })
        .await
        .unwrap_err();

    assert_eq!(first.items.len(), 1);
    assert!(first.next.is_some());
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].id, second.items[0].id);
    assert!(
        is_api(&out_of_range, 422, ErrorCode::ValidationFailed),
        "{out_of_range:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn an_unknown_session_is_not_found() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let unknown: SessionId = "01900000-0000-7000-8000-000000000001".parse().unwrap();

    let refused = owner.revoke_session(unknown).await.unwrap_err();

    assert!(is_api(&refused, 404, ErrorCode::NotFound), "{refused:?}");
    server.stop().await;
}

#[tokio::test]
async fn the_password_changes_after_a_step_up_and_ends_the_other_sessions() {
    let server = Server::start().await;
    let (owner, _, app) = server.owner().await;
    let (other, _) = server.client();
    server.log_in(&other, OWNER, &app).await;
    let new_password = "a different correct horse battery staple";

    let without = owner
        .change_password(&PasswordChangeRequest {
            new_password: SecretText::new(new_password.to_owned()),
        })
        .await
        .unwrap_err();
    owner.step_up(&step_up(PASSWORD)).await.unwrap();
    owner
        .change_password(&PasswordChangeRequest {
            new_password: SecretText::new(new_password.to_owned()),
        })
        .await
        .unwrap();
    let ended = other.me().await.unwrap_err();

    assert!(
        is_api(&without, 403, ErrorCode::StepUpRequired),
        "{without:?}"
    );
    assert!(matches!(ended, ClientError::NoSession), "{ended:?}");
    owner.me().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn a_wrong_step_up_password_is_forbidden_and_keeps_the_session() {
    let server = Server::start().await;
    let (owner, store, _) = server.owner().await;
    let before = stored(&store).await;

    let refused = owner
        .step_up(&step_up("not the password"))
        .await
        .unwrap_err();

    assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
    assert_eq!(stored(&store).await, before);
    owner.me().await.unwrap();
    server.stop().await;
}
