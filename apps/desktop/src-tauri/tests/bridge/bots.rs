//! The bots' and modes' commands (Plan.md P11.9; ADR-0020, U32, S73): a page
//! of bots, one bot, a page of modes, and the seven writes, against the real
//! server.

use fleet_api_types::accounts::{CreateGrantRequest, CreateOfflineAccountRequest, GrantLevel};
use fleet_api_types::app::CommandErrorCode;
use fleet_api_types::bots::{
    BotModeRequest, BotPatchRequest, BotStatusKind, DesiredState, UnplacedReason,
};
use fleet_api_types::page::PAGE_MAX;
use fleet_api_types::{BotId, ErrorCode, ModeId, SecretText};

use crate::support::{Bridge, Server, is_api, is_invalid, step_up_request};

fn offline(name: &str) -> CreateOfflineAccountRequest {
    CreateOfflineAccountRequest {
        name: SecretText::new(name.to_owned()),
    }
}

fn unknown_bot() -> BotId {
    "01900000-0000-7000-8000-00000000000b".parse().unwrap()
}

fn address(address: &str) -> BotPatchRequest {
    BotPatchRequest {
        server_address: address.to_owned(),
    }
}

/// The built-in `farm` mode, as the server seeds it.
fn farm() -> ModeId {
    "01a14202-2800-7000-8000-000000000002".parse().unwrap()
}

#[tokio::test]
async fn without_a_server_every_bot_and_mode_command_is_no_server() {
    let bridge = Bridge::new();
    let state = &bridge.state;
    let bot = unknown_bot();

    let answers = [
        state.bots(None, None).await.map(drop),
        state.bot(bot).await.map(drop),
        state.modes(None, None).await.map(drop),
        state
            .patch_bot(bot, address("play.example.com"))
            .await
            .map(drop),
        state
            .set_bot_mode(bot, BotModeRequest { mode_id: farm() })
            .await
            .map(drop),
        state.start_bot(bot).await.map(drop),
        state.stop_bot(bot).await.map(drop),
        state.restart_bot(bot).await.map(drop),
        state.reset_bot(bot).await.map(drop),
        state.resume_bot(bot).await.map(drop),
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

#[tokio::test]
async fn a_bots_server_is_set_after_a_step_up_and_an_unparseable_one_never_leaves_the_app() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let account = owner
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap();
    let bot = owner.state.bots(None, None).await.unwrap().items[0].id;

    let unparseable = owner
        .state
        .patch_bot(bot, address("https://play.example.com/x"))
        .await
        .unwrap_err();
    let unstepped = owner
        .state
        .patch_bot(bot, address("play.example.com"))
        .await
        .unwrap_err();
    owner.state.step_up(step_up_request()).await.unwrap();
    let set = owner
        .state
        .patch_bot(bot, address("play.example.com:25566"))
        .await
        .unwrap();

    assert!(
        is_invalid(&unparseable, "server_address"),
        "{unparseable:?}"
    );
    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert_eq!(set.account.id, account.id);
    assert_eq!(
        set.server_address
            .as_ref()
            .map(fleet_api_types::BoundedText::as_str),
        Some("play.example.com:25566")
    );
    server.stop().await;
}

#[tokio::test]
async fn a_bot_switches_mode_starts_unplaced_and_stops() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap();
    let bot = owner.state.bots(None, None).await.unwrap().items[0].id;
    owner.state.step_up(step_up_request()).await.unwrap();
    owner
        .state
        .patch_bot(bot, address("play.example.com"))
        .await
        .unwrap();

    let switched = owner
        .state
        .set_bot_mode(bot, BotModeRequest { mode_id: farm() })
        .await
        .unwrap();
    let started = owner.state.start_bot(bot).await.unwrap();
    let stopped = owner.state.stop_bot(bot).await.unwrap();

    assert_eq!(switched.mode_id, farm());
    assert_eq!(started.desired_state.known(), Some(DesiredState::Running));
    assert_eq!(started.status.kind.known(), Some(BotStatusKind::Unplaced));
    assert_eq!(
        started
            .status
            .why
            .as_ref()
            .and_then(fleet_api_types::Open::known),
        Some(UnplacedReason::AwaitingAgent)
    );
    assert_eq!(stopped.desired_state.known(), Some(DesiredState::Stopped));
    server.stop().await;
}

#[tokio::test]
async fn with_no_agent_a_restart_reset_or_resume_is_the_servers_409() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    owner
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap();
    let bot = owner.state.bots(None, None).await.unwrap().items[0].id;

    let restarted = owner.state.restart_bot(bot).await.unwrap_err();
    let reset = owner.state.reset_bot(bot).await.unwrap_err();
    let resumed = owner.state.resume_bot(bot).await.unwrap_err();

    assert!(
        is_api(&restarted, 409, ErrorCode::BotNotRunning),
        "{restarted:?}"
    );
    assert!(is_api(&reset, 409, ErrorCode::BotNotFailed), "{reset:?}");
    assert!(
        is_api(&resumed, 409, ErrorCode::BotNotPaused),
        "{resumed:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn a_view_holder_may_see_a_bot_but_not_start_it() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let account = owner
        .state
        .create_offline_account(offline("AfkBot1"))
        .await
        .unwrap();
    owner.state.step_up(step_up_request()).await.unwrap();
    let (viewer, _) = server.member(&owner, "viewer").await;
    owner
        .state
        .create_grant(
            account.id,
            CreateGrantRequest {
                username: SecretText::new("viewer".to_owned()),
                level: GrantLevel::View,
            },
        )
        .await
        .unwrap();
    let bot = viewer.state.bots(None, None).await.unwrap().items[0].id;

    let refused = viewer.state.start_bot(bot).await.unwrap_err();

    assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
    server.stop().await;
}
