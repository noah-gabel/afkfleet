//! The chat routes (Plan.md P11.3; ADR-0020, S4, S8, S9, U3). Without
//! `[grpc]` no agent runs a bot, so every line here meets a bot that runs
//! nowhere: what a bot's agent answers is tested in fleet-server's HTTP tests
//! and in fleet-integration's chat case, and the 200 and 202 over the
//! scripted transport in `session.rs`. The history reads rows the test
//! writes into the server's database; the field rules per viewer are
//! fleet-server's to test.

use fleet_api_types::chat::{ChatCommandRequest, ChatPage, ChatPageQuery, ChatSendRequest};
use fleet_api_types::{ErrorCode, SafeInt};

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

fn query(before: Option<i64>, limit: Option<u32>) -> ChatPageQuery {
    ChatPageQuery {
        before: before.map(|cursor| SafeInt::try_new(cursor).unwrap()),
        limit,
    }
}

fn texts(page: &ChatPage) -> Vec<&str> {
    page.items.iter().map(|line| line.text.as_str()).collect()
}

#[tokio::test]
async fn a_bot_without_chat_has_an_empty_history() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let (_, bot) = bot_of(&owner, "AfkBot1").await;

    let page = owner.chat(bot, &query(None, None)).await.unwrap();

    assert_eq!(page.items, Vec::new());
    assert_eq!(page.next, None);
    server.stop().await;
}

/// The history pages newest first: `next` is the next page's `before`, and
/// the last page has none.
#[tokio::test]
async fn the_history_pages_with_before_and_limit() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let (_, bot) = bot_of(&owner, "AfkBot1").await;
    server.said(bot, &["one", "two", "three"]).await;

    let first = owner.chat(bot, &query(None, Some(2))).await.unwrap();
    let next = first.next.map(SafeInt::get);
    let second = owner.chat(bot, &query(next, Some(2))).await.unwrap();

    assert_eq!(texts(&first), ["three", "two"]);
    assert!(next.is_some());
    assert_eq!(texts(&second), ["one"]);
    assert_eq!(second.next, None);
    server.stop().await;
}

#[tokio::test]
async fn another_members_bots_history_is_404_and_a_bad_query_422() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (_, bot) = bot_of(&owner, "AfkBot1").await;

    let hidden = alice.chat(bot, &query(None, None)).await.unwrap_err();
    let errors = [
        owner.chat(bot, &query(None, Some(0))).await.unwrap_err(),
        owner.chat(bot, &query(Some(0), None)).await.unwrap_err(),
    ];

    assert!(is_api(&hidden, 404, ErrorCode::NotFound), "{hidden:?}");
    for (error, field) in errors.iter().zip(["limit", "before"]) {
        assert!(is_api(error, 422, ErrorCode::ValidationFailed), "{error:?}");
        let fields: Vec<&str> = error
            .api()
            .unwrap()
            .fields
            .iter()
            .map(|field| field.path.as_str())
            .collect();
        assert_eq!(fields, [field]);
    }
    server.stop().await;
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
