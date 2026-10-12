//! Replacing the authenticator in a session.

use core::time::Duration;

use fleet_api_types::auth::StepUpRequest;
use fleet_api_types::{ErrorCode, SecretText};

use crate::support::{OWNER, PASSWORD, Server, app_of, is_api};

#[tokio::test]
async fn a_reset_needs_a_fresh_step_up() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;

    let refused = owner.mfa_reset().await.unwrap_err();

    assert!(
        is_api(&refused, 403, ErrorCode::StepUpRequired),
        "{refused:?}"
    );
    owner.me().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn the_authenticator_is_replaced_in_the_session_and_logs_in_next_time() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    owner
        .step_up(&StepUpRequest {
            password: SecretText::new(PASSWORD.to_owned()),
        })
        .await
        .unwrap();

    let expires_in = owner.mfa_reset().await.unwrap();
    let app = app_of(&owner.mfa_enroll().await.unwrap());
    let codes = owner.mfa_confirm(server.code(&app)).await.unwrap();
    let me = owner.me().await.unwrap();
    server.next_step();
    owner.logout().await.unwrap();
    let (next, _) = server.client();
    server.log_in(&next, OWNER, &app).await;

    assert_eq!(expires_in, Duration::from_secs(600));
    assert_eq!(codes.recovery_codes.len(), 10);
    assert_eq!(me.recovery_codes_left, 10);
    next.me().await.unwrap();
    server.stop().await;
}
