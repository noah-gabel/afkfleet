//! The bots' and modes' read commands (Plan.md P11.9; ADR-0020, U32): a page
//! of bots, one bot, and a page of modes, against the real server.

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::app::CommandErrorCode;
use fleet_api_types::page::PAGE_MAX;
use fleet_api_types::{BotId, ErrorCode, SecretText};

use crate::support::{Bridge, Server, is_api, is_invalid};

fn offline(name: &str) -> CreateOfflineAccountRequest {
    CreateOfflineAccountRequest {
        name: SecretText::new(name.to_owned()),
    }
}

fn unknown_bot() -> BotId {
    "01900000-0000-7000-8000-00000000000b".parse().unwrap()
}

#[tokio::test]
async fn without_a_server_every_bot_and_mode_read_is_no_server() {
    let bridge = Bridge::new();

    let answers = [
        bridge.state.bots(None, None).await.map(drop),
        bridge.state.bot(unknown_bot()).await.map(drop),
        bridge.state.modes(None, None).await.map(drop),
    ];

    for answer in answers {
        assert_eq!(answer.unwrap_err().code, CommandErrorCode::NoServer);
    }
}

#[tokio::test]
async fn the_bots_come_a_page_at_a_time_newest_first_and_each_reads_as_listed() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    for name in ["AfkBot1", "AfkBot2", "AfkBot3"] {
        owner
            .state
            .create_offline_account(offline(name))
            .await
            .unwrap();
    }

    let first = owner.state.bots(None, Some(2)).await.unwrap();
    let rest = owner.state.bots(first.next, Some(2)).await.unwrap();
    let one = owner.state.bot(first.items[0].id).await.unwrap();

    // The server's clock is the test's, so the three IDs share a millisecond
    // and only their order, not the names', is the page's rule.
    let listed: Vec<_> = first.items.iter().chain(&rest.items).collect();
    let mut names: Vec<&str> = listed.iter().map(|bot| bot.account.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["AfkBot1", "AfkBot2", "AfkBot3"]);
    assert!(
        listed
            .windows(2)
            .all(|pair| pair[0].id.to_string() > pair[1].id.to_string()),
        "newest first"
    );
    assert!(first.next.is_some());
    assert_eq!(rest.next, None);
    assert_eq!(one, first.items[0]);
    server.stop().await;
}

#[tokio::test]
async fn a_bot_nobody_may_see_is_404() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;

    let gone = owner.state.bot(unknown_bot()).await.unwrap_err();

    assert!(is_api(&gone, 404, ErrorCode::NotFound), "{gone:?}");
    server.stop().await;
}

#[tokio::test]
async fn the_modes_page_holds_the_built_in_ones() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;

    let modes = owner.state.modes(None, Some(PAGE_MAX)).await.unwrap();

    let mut names: Vec<&str> = modes.items.iter().map(|mode| mode.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["afk", "farm"]);
    assert_eq!(modes.next, None);
    server.stop().await;
}

#[tokio::test]
async fn a_page_out_of_range_is_refused_before_any_request() {
    let bridge = Bridge::new();

    for limit in [0, PAGE_MAX + 1] {
        let bots = bridge.state.bots(None, Some(limit)).await.unwrap_err();
        let modes = bridge.state.modes(None, Some(limit)).await.unwrap_err();
        assert!(is_invalid(&bots, "limit"), "{bots:?}");
        assert!(is_invalid(&modes, "limit"), "{modes:?}");
    }
}
