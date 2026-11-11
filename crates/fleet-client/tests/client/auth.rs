//! Health, logging in with 2FA, and registering with an invite.

use core::time::Duration;

use fleet_api_types::ErrorCode;
use fleet_api_types::invites::CreateInviteRequest;
use fleet_api_types::users::UserRole;
use fleet_client::{ClientError, LoginStep};

use crate::support::{OWNER, PASSWORD, Server, is_api, login, register, stored};

#[tokio::test]
async fn health_answers_without_a_session() {
    let server = Server::start().await;
    let (client, _) = server.client();

    client.health_live().await.unwrap();
    client.health_ready().await.unwrap();

    server.stop().await;
}

#[tokio::test]
async fn the_owner_sets_up_2fa_and_logs_in_with_the_next_code() {
    let server = Server::start().await;
    let (client, store) = server.client();

    server.first_login(&client, OWNER).await;
    let me = client.me().await.unwrap();

    assert_eq!(me.username.as_str(), OWNER);
    assert_eq!(me.role.known(), Some(UserRole::Owner));
    assert!(me.mfa_confirmed_at.is_some());
    assert_eq!(me.recovery_codes_left, 10);
    assert!(
        stored(&store)
            .await
            .is_some_and(|token| token.starts_with("afk_rt_"))
    );
    server.stop().await;
}

#[tokio::test]
async fn a_wrong_password_is_refused_and_starts_nothing() {
    let server = Server::start().await;
    let (client, store) = server.client();

    let refused = client
        .login(&login(OWNER, "not the password"))
        .await
        .unwrap_err();
    let without_step = client.mfa_enroll().await.unwrap_err();

    assert!(
        is_api(&refused, 401, ErrorCode::InvalidCredentials),
        "{refused:?}"
    );
    assert!(
        matches!(without_step, ClientError::NoPendingStep),
        "{without_step:?}"
    );
    assert_eq!(stored(&store).await, None);
    server.stop().await;
}

#[tokio::test]
async fn a_wrong_code_is_refused_and_the_step_takes_the_right_one() {
    let server = Server::start().await;
    let (owner, _, app) = server.owner().await;
    owner.logout().await.unwrap();
    let (client, store) = server.client();

    let step = client.login(&login(OWNER, PASSWORD)).await.unwrap();
    let wrong = client
        .login_mfa(fleet_api_types::SecretText::new("000000".to_owned()))
        .await
        .unwrap_err();
    client.login_mfa(server.code(&app)).await.unwrap();

    assert!(matches!(step, LoginStep::MfaRequired { .. }), "{step:?}");
    assert!(
        is_api(&wrong, 401, ErrorCode::InvalidCredentials),
        "{wrong:?}"
    );
    assert!(stored(&store).await.is_some());
    client.me().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn logging_in_again_while_logged_in_is_refused() {
    let server = Server::start().await;
    let (client, _, _) = server.owner().await;

    let refused = client.login(&login(OWNER, PASSWORD)).await.unwrap_err();

    assert!(
        matches!(refused, ClientError::AlreadyLoggedIn),
        "{refused:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn the_sixth_login_a_minute_is_rate_limited_with_its_wait() {
    let server = Server::start().await;
    let (client, _) = server.client();

    for n in 1..=5 {
        let refused = client
            .login(&login(&format!("nobody{n}"), PASSWORD))
            .await
            .unwrap_err();
        assert!(
            is_api(&refused, 401, ErrorCode::InvalidCredentials),
            "{refused:?}"
        );
    }
    let limited = client.login(&login("nobody6", PASSWORD)).await.unwrap_err();

    assert!(is_api(&limited, 429, ErrorCode::RateLimited), "{limited:?}");
    assert_eq!(
        limited.api().unwrap().retry_after,
        Some(Duration::from_secs(12))
    );
    server.stop().await;
}

#[tokio::test]
async fn an_invitee_registers_logs_in_and_sets_up_2fa() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let created = owner
        .create_invite(&CreateInviteRequest {
            role: UserRole::Member,
        })
        .await
        .unwrap();
    let (client, _) = server.client();

    let registered = client
        .register(&register(created.code.expose(), "bob", PASSWORD))
        .await
        .unwrap();
    server.advance(Duration::from_secs(60));
    server.first_login(&client, "bob").await;
    let me = client.me().await.unwrap();

    assert_eq!(registered.username.as_str(), "bob");
    assert_eq!(registered.role.known(), Some(UserRole::Member));
    assert_eq!(me.id, registered.id);
    server.stop().await;
}

#[tokio::test]
async fn a_weak_password_and_a_used_invite_are_refused_on_their_fields() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let created = owner
        .create_invite(&CreateInviteRequest {
            role: UserRole::Member,
        })
        .await
        .unwrap();
    let code = created.code.expose().to_owned();
    let (client, _) = server.client();

    let weak = client
        .register(&register(&code, "bob", "aaaaaaaaaaaa"))
        .await
        .unwrap_err();
    client
        .register(&register(&code, "bob", PASSWORD))
        .await
        .unwrap();
    let reused = client
        .register(&register(&code, "carol", PASSWORD))
        .await
        .unwrap_err();

    for (error, field) in [(&weak, "password"), (&reused, "invite")] {
        assert!(is_api(error, 422, ErrorCode::ValidationFailed), "{error:?}");
        let fields = &error.api().unwrap().fields;
        assert!(
            fields.iter().any(|problem| problem.path.as_str() == field),
            "{fields:?}"
        );
    }
    server.stop().await;
}
