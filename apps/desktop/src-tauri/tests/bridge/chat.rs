//! The chat's commands (Plan.md P11.9; ADR-0020, U32, U34, S4, S8): a bot's
//! history a page at a time, and a line or a command sent, against the real
//! server. No agent runs here, so a line reaches the server's checks and
//! stops at `bot_not_online`; the server's own tests send it further.

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::app::CommandErrorCode;
use fleet_api_types::chat::{CHAT_MESSAGE_MAX_UNITS, ChatCommandRequest, ChatSendRequest};
use fleet_api_types::page::PAGE_MAX;
use fleet_api_types::{BotId, ErrorCode, SafeInt, SecretText};

use crate::support::{Bridge, Server, is_api, is_invalid, step_up_request};

fn unknown_bot() -> BotId {
    "01900000-0000-7000-8000-00000000000b".parse().unwrap()
}

fn line(message: &str) -> ChatSendRequest {
    ChatSendRequest {
        message: message.to_owned(),
    }
}

fn command(message: &str) -> ChatCommandRequest {
    ChatCommandRequest {
        message: message.to_owned(),
    }
}

/// The Owner's bot of the offline account `AfkBot1`, on `server`.
async fn a_bot(owner: &crate::support::Bridge) -> BotId {
    owner
        .state
        .create_offline_account(CreateOfflineAccountRequest {
            name: SecretText::new("AfkBot1".to_owned()),
        })
        .await
        .unwrap();
    owner.state.bots(None, None).await.unwrap().items[0].id
}

#[tokio::test]
async fn without_a_server_every_chat_command_is_no_server() {
    let bridge = Bridge::new();
    let state = &bridge.state;
    let bot = unknown_bot();

    let answers = [
        state.bot_chat(bot, None, None).await.map(drop),
        state.send_chat(bot, line("hello")).await.map(drop),
        state.send_command(bot, command("/spawn")).await.map(drop),
    ];

    for answer in answers {
        assert_eq!(answer.unwrap_err().code, CommandErrorCode::NoServer);
    }
}

#[tokio::test]
async fn a_bots_chat_comes_a_page_at_a_time_newest_first() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let bot = a_bot(&owner).await;
    server.said(bot, &["one", "two", "three"]).await;

    let first = owner.state.bot_chat(bot, None, Some(2)).await.unwrap();
    let rest = owner
        .state
        .bot_chat(bot, first.next, Some(2))
        .await
        .unwrap();

    let texts = |page: &fleet_api_types::chat::ChatPage| -> Vec<String> {
        page.items
            .iter()
            .map(|line| line.text.to_string())
            .collect()
    };
    assert_eq!(texts(&first), ["three", "two"]);
    assert_eq!(texts(&rest), ["one"]);
    assert!(first.next.is_some());
    assert_eq!(rest.next, None);
    server.stop().await;
}

#[tokio::test]
async fn the_chat_of_a_bot_nobody_may_see_is_404() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;

    let gone = owner
        .state
        .bot_chat(unknown_bot(), None, None)
        .await
        .unwrap_err();

    assert!(is_api(&gone, 404, ErrorCode::NotFound), "{gone:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_line_to_a_bot_with_no_agent_is_bot_not_online() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let bot = a_bot(&owner).await;

    let refused = owner.state.send_chat(bot, line("hello")).await.unwrap_err();

    assert!(
        is_api(&refused, 409, ErrorCode::BotNotOnline),
        "{refused:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn a_command_off_the_allowlist_goes_to_the_command_route_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let bot = a_bot(&owner).await;

    let pointed = owner
        .state
        .send_chat(bot, line("/op AfkBot2"))
        .await
        .unwrap_err();
    let asked = owner
        .state
        .send_command(bot, command("/op AfkBot2"))
        .await
        .unwrap_err();
    owner.state.step_up(step_up_request()).await.unwrap();
    let sent = owner
        .state
        .send_command(bot, command("/op AfkBot2"))
        .await
        .unwrap_err();

    assert!(
        is_api(&pointed, 403, ErrorCode::CommandNeedsStepUp),
        "{pointed:?}"
    );
    assert!(is_api(&asked, 403, ErrorCode::StepUpRequired), "{asked:?}");
    assert!(is_api(&sent, 409, ErrorCode::BotNotOnline), "{sent:?}");
    server.stop().await;
}

#[tokio::test]
async fn a_line_or_a_page_the_server_would_refuse_never_leaves_the_app() {
    let bridge = Bridge::new();
    let state = &bridge.state;
    let bot = unknown_bot();
    let too_long = "a".repeat(CHAT_MESSAGE_MAX_UNITS + 1);

    let refused = [
        state.send_chat(bot, line(&too_long)).await.unwrap_err(),
        state.send_chat(bot, line("ring\u{7}")).await.unwrap_err(),
        state.send_chat(bot, line("   ")).await.unwrap_err(),
        state.send_command(bot, command("hello")).await.unwrap_err(),
    ];
    let pages = [
        state
            .bot_chat(bot, Some(SafeInt::try_new(0).unwrap()), None)
            .await
            .unwrap_err(),
        state.bot_chat(bot, None, Some(0)).await.unwrap_err(),
        state
            .bot_chat(bot, None, Some(PAGE_MAX + 1))
            .await
            .unwrap_err(),
    ];

    for error in refused {
        assert!(is_invalid(&error, "message"), "{error:?}");
    }
    let [before, none, many] = pages;
    assert!(is_invalid(&before, "before"), "{before:?}");
    assert!(is_invalid(&none, "limit"), "{none:?}");
    assert!(is_invalid(&many, "limit"), "{many:?}");
}
