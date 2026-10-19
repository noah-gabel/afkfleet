//! The bots' routes (Plan.md P11.2). Without `[grpc]` no agent runs a bot,
//! so every bot here runs nowhere: what reaches an agent is tested in
//! fleet-server's loopback tests (`grpc_control::reconcile`), and the
//! 202s over the scripted transport in `session.rs`.

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::bots::{
    BotModeRequest, BotPatchRequest, BotResponse, BotStatusKind, DesiredState, UnplacedReason,
};
use fleet_api_types::{AccountId, BotId, BoundedText, ErrorCode, Open, PageQuery, SecretText};
use fleet_client::{Client, MemoryStore};
use fleet_core::mode::ModePreset;

use crate::support::{Server, is_api, step_up};

fn all() -> PageQuery<BotId> {
    PageQuery {
        before: None,
        limit: None,
    }
}

/// `client`'s new offline account `name`, and the bot it comes with.
pub(crate) async fn bot_of(client: &Client<MemoryStore>, name: &str) -> (AccountId, BotId) {
    let account = client
        .create_offline_account(&CreateOfflineAccountRequest {
            name: SecretText::new(name.to_owned()),
        })
        .await
        .unwrap()
        .id;
    let bot = client
        .bots(&all())
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|bot| bot.account.id == account)
        .unwrap()
        .id;
    (account, bot)
}

fn address(text: &str) -> BotPatchRequest {
    BotPatchRequest {
        server_address: text.to_owned(),
    }
}

/// Why `bot` runs nowhere, if it reads so.
fn why(bot: &BotResponse) -> Option<UnplacedReason> {
    assert_eq!(bot.status.kind.known(), Some(BotStatusKind::Unplaced));
    bot.status.why.as_ref().and_then(Open::known)
}

#[tokio::test]
async fn a_new_accounts_bot_is_listed_and_read_stopped_without_a_server() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (account, _) = bot_of(&alice, "AfkBot1").await;

    let page = alice.bots(&all()).await.unwrap();
    let listed = page.items.first().unwrap();
    let read = alice.bot(listed.id).await.unwrap();

    assert_eq!((page.items.len(), page.next), (1, None));
    assert_eq!(&read, listed);
    assert_eq!(read.account.id, account);
    assert_eq!(read.server_address, None);
    assert_eq!(read.mode_id, ModePreset::Afk.id());
    assert_eq!(read.desired_state.known(), Some(DesiredState::Stopped));
    assert_eq!(why(&read), Some(UnplacedReason::NoServer));
    assert_eq!(read.agent, None);
    server.stop().await;
}

#[tokio::test]
async fn a_bots_server_needs_a_step_up_and_once_started_it_waits_for_an_agent() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (_, bot) = bot_of(&alice, "AfkBot1").await;

    let without = alice
        .patch_bot(bot, &address("play.example.com"))
        .await
        .unwrap_err();
    step_up(&alice).await;
    let unparsed = alice
        .patch_bot(bot, &address("play.example.com:0"))
        .await
        .unwrap_err();
    let patched = alice
        .patch_bot(bot, &address("play.example.com"))
        .await
        .unwrap();
    let started = alice.start_bot(bot).await.unwrap();
    let stopped = alice.stop_bot(bot).await.unwrap();

    assert!(
        is_api(&without, 403, ErrorCode::StepUpRequired),
        "{without:?}"
    );
    assert!(
        is_api(&unparsed, 422, ErrorCode::ValidationFailed),
        "{unparsed:?}"
    );
    assert_eq!(
        patched.server_address.as_ref().map(BoundedText::as_str),
        Some("play.example.com")
    );
    assert_eq!(why(&patched), Some(UnplacedReason::Stopped));
    assert_eq!(started.desired_state.known(), Some(DesiredState::Running));
    assert_eq!(why(&started), Some(UnplacedReason::AwaitingAgent));
    assert_eq!(stopped.desired_state.known(), Some(DesiredState::Stopped));
    assert_eq!(why(&stopped), Some(UnplacedReason::Stopped));
    server.stop().await;
}

#[tokio::test]
async fn a_bots_mode_is_set_by_its_id_and_an_unknown_one_is_refused() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (_, bot) = bot_of(&alice, "AfkBot1").await;

    let farm = alice
        .set_bot_mode(
            bot,
            &BotModeRequest {
                mode_id: ModePreset::Farm.id(),
            },
        )
        .await
        .unwrap();
    let unknown = alice
        .set_bot_mode(
            bot,
            &BotModeRequest {
                mode_id: "01900000-0000-7000-8000-0000000000ff".parse().unwrap(),
            },
        )
        .await
        .unwrap_err();

    assert_eq!(farm.mode_id, ModePreset::Farm.id());
    assert_eq!(alice.bot(bot).await.unwrap().mode_id, ModePreset::Farm.id());
    assert!(
        is_api(&unknown, 422, ErrorCode::ValidationFailed),
        "{unknown:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn a_command_for_a_bot_that_runs_nowhere_is_refused_with_its_409_code() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (_, bot) = bot_of(&alice, "AfkBot1").await;
    alice.start_bot(bot).await.unwrap();

    let restarted = alice.restart_bot(bot).await.unwrap_err();
    let reset = alice.reset_bot(bot).await.unwrap_err();
    let resumed = alice.resume_bot(bot).await.unwrap_err();

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
async fn another_members_bot_is_missing_from_their_list_and_404_to_every_call() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (bob, _) = server.member(&owner, "bob").await;
    let (_, bot) = bot_of(&alice, "AfkBot1").await;
    step_up(&bob).await;

    let listed = bob.bots(&all()).await.unwrap();
    let errors = [
        bob.bot(bot).await.unwrap_err(),
        bob.patch_bot(bot, &address("play.example.com"))
            .await
            .unwrap_err(),
        bob.set_bot_mode(
            bot,
            &BotModeRequest {
                mode_id: ModePreset::Farm.id(),
            },
        )
        .await
        .unwrap_err(),
        bob.start_bot(bot).await.unwrap_err(),
        bob.stop_bot(bot).await.unwrap_err(),
        bob.restart_bot(bot).await.unwrap_err(),
        bob.reset_bot(bot).await.unwrap_err(),
        bob.resume_bot(bot).await.unwrap_err(),
    ];

    assert_eq!((listed.items, listed.next), (Vec::new(), None));
    for error in &errors {
        assert!(is_api(error, 404, ErrorCode::NotFound), "{error:?}");
    }
    server.stop().await;
}
