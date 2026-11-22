//! An agent of the last release beside this release's server (Plan.md
//! P15.29; ADR-0036, D13; ADR-0044, E3; ADR-0046): the server is updated
//! first, so for a while its agents are older, and a scripted mode must
//! never reach one.
//!
//! The chaos world's stack, with its second agent the image the last
//! release published (`world::LAST_RELEASE_AGENT`), pulled once and pinned
//! by its index digest. This release's own agent is stopped, disabled and
//! released, so its three bots go to the old agent on their step mode, and
//! run there. Then a scripted mode can't be set on one of them (409
//! `agent_too_old`, nothing changed), and a new bot on it waits for a newer
//! agent and is never placed on the old one. The old agent's stream never
//! ends meanwhile, and its log holds no warning and no error: it refused
//! nothing it was sent, because it was sent no script.
//!
//! `just test-slow` and `just test-stack` run it (the Stack workflow
//! nightly), beside the smoke tests; its name says so.

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::agents::AgentStatus;
use fleet_api_types::bots::{BotModeRequest, BotPatchRequest, BotStatusKind, UnplacedReason};
use fleet_api_types::error::ErrorCode;
use fleet_api_types::modes::{CreateModeRequest, ModeSteps, ModeVisibility};
use fleet_api_types::{BoundedText, Open, PageQuery, SecretText};
use fleet_client::ClientError;
use fleet_core::id::{BotId, ModeId};
use fleet_script::EXAMPLES;

use super::super::stack::QUICK;
use super::world::{BOTS, Chaos, LOCAL, MINECRAFT, SECOND, until};
use crate::slow_scripts::request;

/// The stack's project, and its networks' number.
const PROJECT: &str = "afkfleet-smoke-last-release";
const NET: u8 = 140;
/// The version the last release's agent reports in its hello.
const LAST_RELEASE: &str = "0.1.3";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_smoke_an_agent_of_the_last_release_is_sent_no_script_and_refuses_nothing() {
    let mut chaos = Chaos::start_beside_the_last_release(PROJECT, NET).await;
    let old = chaos.agent(SECOND, AgentStatus::Online).await;
    assert_eq!(
        old.version.as_ref().map(BoundedText::as_str),
        Some(LAST_RELEASE)
    );

    onto_the_old_agent(&mut chaos).await;
    let scripted = a_scripted_mode(&chaos).await;

    // A scripted mode is never set on a bot the old agent runs.
    let first = chaos.bots[0];
    let before = chaos
        .shown(first)
        .await
        .expect("the server answers")
        .mode_id;
    match chaos
        .owner
        .client
        .set_bot_mode(first, &BotModeRequest { mode_id: scripted })
        .await
    {
        Err(ClientError::Api(failure)) => {
            assert_eq!(failure.status, 409, "{failure:?}");
            assert_eq!(
                failure.code.known(),
                Some(ErrorCode::AgentTooOld),
                "{failure:?}"
            );
        }
        other => panic!("a scripted mode was set on a bot of the old agent: {other:?}"),
    }
    assert_eq!(
        chaos
            .shown(first)
            .await
            .expect("the server answers")
            .mode_id,
        before
    );

    // A new bot on the scripted mode waits for a newer agent, and the old
    // one never gets it.
    let waiting = a_bot_on(&chaos, "AfkBot4", scripted).await;
    until(
        &chaos.stack.compose,
        "server",
        "the scripted bot waiting for a newer agent",
        async || {
            let shown = chaos.shown(waiting).await?;
            (shown.status.kind.known() == Some(BotStatusKind::Unplaced)
                && shown.status.why.as_ref().and_then(Open::known)
                    == Some(UnplacedReason::AgentTooOld)
                && shown.agent.is_none())
            .then_some(())
        },
    )
    .await;

    // The old agent's stream never ended, and it refused nothing.
    let now = chaos.agent(SECOND, AgentStatus::Online).await;
    assert_eq!(
        now.connected_since, old.connected_since,
        "the old agent's stream ended"
    );
    assert_eq!(now.bots, u32::try_from(BOTS.len()).unwrap());
    its_log_holds_no_warning(&chaos);
}

/// Stops this release's agent, as its host would be stopped for an update,
/// disables and releases it, and waits until its bots run on the old agent.
async fn onto_the_old_agent(chaos: &mut Chaos) {
    let local = chaos.agent(LOCAL, AgentStatus::Online).await;
    chaos.stop("agent");
    chaos.owner.client.disable_agent(local.id).await.unwrap();
    chaos.owner.client.release_agent(local.id).await.unwrap();
    let bots = chaos.bots.clone();
    until(
        &chaos.stack.compose,
        "server",
        "every bot online on the old agent",
        async || {
            for bot in &bots {
                let shown = chaos.shown(*bot).await?;
                let on_the_old = shown
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.name.as_str() == SECOND);
                if !(on_the_old && Chaos::reported_online(&shown)) {
                    return None;
                }
            }
            Some(())
        },
    )
    .await;
}

/// The reference's AFK farm as a scripted mode of the Owner's.
async fn a_scripted_mode(chaos: &Chaos) -> ModeId {
    let example = EXAMPLES
        .iter()
        .find(|example| example.id == "afk_farm")
        .expect("the AFK farm");
    chaos
        .owner
        .client
        .create_mode_with_commands(&CreateModeRequest {
            name: example.title.to_owned(),
            visibility: ModeVisibility::Private,
            steps: ModeSteps(Vec::new()),
            script: Some(request(example.draft())),
        })
        .await
        .unwrap()
        .id
}

/// A new offline account `name` and its bot, sent to the project's
/// Minecraft server on `mode`, and started.
async fn a_bot_on(chaos: &Chaos, name: &str, mode: ModeId) -> BotId {
    let client = &chaos.owner.client;
    let account = client
        .create_offline_account(&CreateOfflineAccountRequest {
            name: SecretText::new(name.to_owned()),
        })
        .await
        .unwrap();
    let bot = client
        .bots(&PageQuery {
            before: None,
            limit: Some(100),
        })
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|bot| bot.account.id == account.id)
        .expect("the account's bot is listed")
        .id;
    client
        .patch_bot(
            bot,
            &BotPatchRequest {
                server_address: MINECRAFT.to_owned(),
            },
        )
        .await
        .unwrap();
    client
        .set_bot_mode(bot, &BotModeRequest { mode_id: mode })
        .await
        .unwrap();
    client.start_bot(bot).await.unwrap();
    bot
}

/// The old agent's log, which holds no warning and no error: a message it
/// couldn't read would have ended its stream with one.
fn its_log_holds_no_warning(chaos: &Chaos) {
    let log = chaos
        .stack
        .compose
        .run(
            &["--profile", "second", "logs", "--no-color", "agent-two"],
            QUICK,
        )
        .unwrap();
    assert!(log.success, "{}", log.stderr);
    let alarming: Vec<&str> = log
        .stdout
        .lines()
        .filter(|line| line.contains("\"level\":\"WARN\"") || line.contains("\"level\":\"ERROR\""))
        .collect();
    assert_eq!(alarming, Vec::<&str>::new(), "the old agent warned");
}
