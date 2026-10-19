//! The chat routes (Plan.md P11.3; ADR-0020, S4, S8). Without `[grpc]` no
//! agent runs a bot, so every line here meets a bot that runs nowhere: what
//! a bot's agent answers is tested in fleet-server's HTTP tests and in
//! fleet-integration's chat case, and the 200 and 202 over the scripted
//! transport in `session.rs`.

use fleet_api_types::ErrorCode;
use fleet_api_types::chat::{ChatCommandRequest, ChatSendRequest};

use crate::bots::bot_of;
use crate::support::{Server, is_api, step_up};

fn chat(text: &str) -> ChatSendRequest {
    ChatSendRequest {
        message: text.to_owned(),
    }
}

fn command(text: &str) -> ChatCommandRequest {
    ChatCommandRequest {
        message: text.to_owned(),
    }
}

#[tokio::test]
async fn a_line_for_a_bot_that_runs_nowhere_is_refused_as_not_online() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (_, bot) = bot_of(&alice, "AfkBot1").await;

    let refused = alice.send_chat(bot, &chat("hello")).await.unwrap_err();

    assert!(
        is_api(&refused, 409, ErrorCode::BotNotOnline),
        "{refused:?}"
    );
    server.stop().await;
}

/// A command off the allowlist is pointed to the command route, which
/// needs a fresh step-up, and then reaches the bot's state.
#[tokio::test]
async fn an_unlisted_command_is_pointed_to_the_command_route_which_needs_a_step_up() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (_, bot) = bot_of(&alice, "AfkBot1").await;

    let pointed = alice
        .send_chat(bot, &chat("/op AfkBot2"))
        .await
        .unwrap_err();
    let without = alice
        .send_command(bot, &command("/op AfkBot2"))
        .await
        .unwrap_err();
    step_up(&alice).await;
    let with = alice
        .send_command(bot, &command("/op AfkBot2"))
        .await
        .unwrap_err();

    assert!(
        is_api(&pointed, 403, ErrorCode::CommandNeedsStepUp),
        "{pointed:?}"
    );
    assert!(
        is_api(&without, 403, ErrorCode::StepUpRequired),
        "{without:?}"
    );
    assert!(is_api(&with, 409, ErrorCode::BotNotOnline), "{with:?}");
    server.stop().await;
}

#[tokio::test]
async fn another_members_bot_is_404_on_both_routes() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (bob, _) = server.member(&owner, "bob").await;
    let (_, bot) = bot_of(&alice, "AfkBot1").await;
    step_up(&bob).await;

    let errors = [
        bob.send_chat(bot, &chat("hello")).await.unwrap_err(),
        bob.send_command(bot, &command("/spawn")).await.unwrap_err(),
    ];

    for error in &errors {
        assert!(is_api(error, 404, ErrorCode::NotFound), "{error:?}");
    }
    server.stop().await;
}

#[tokio::test]
async fn an_invalid_line_is_422_on_message_and_never_echoed() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (_, bot) = bot_of(&owner, "AfkBot1").await;

    let errors = [
        owner
            .send_chat(bot, &chat("secret_label\nnext"))
            .await
            .unwrap_err(),
        owner
            .send_command(bot, &command("secret_label"))
            .await
            .unwrap_err(),
    ];

    for error in &errors {
        assert!(is_api(error, 422, ErrorCode::ValidationFailed), "{error:?}");
        let failure = error.api().unwrap();
        assert_eq!(
            failure
                .fields
                .iter()
                .map(|field| field.path.as_str())
                .collect::<Vec<_>>(),
            ["message"]
        );
        assert!(!format!("{error}").contains("secret_label"), "{error}");
        assert!(
            failure
                .fields
                .iter()
                .all(|field| !field.message.as_str().contains("secret_label"))
        );
    }
    server.stop().await;
}
