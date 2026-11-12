//! A kill during a refresh of an account's Microsoft token (Plan.md P13.5;
//! ADR-0019, S64: "a kill during a refresh is a crash"; ADR-0033), with the
//! case's own Microsoft, never the real one. Microsoft rotates the refresh
//! token at every refresh, so a server that dies in one holds either a token
//! that's still good or one Microsoft has spent. Either way the account is
//! never corrupt:
//!
//! - **Killed before Microsoft saw the request:** the stored token is
//!   still good. The account stays active, and its bot joins again by
//!   itself.
//! - **Killed after Microsoft rotated the token,** before the new one was
//!   stored (the same for a kill between the answer and its commit): the
//!   stored token is spent. The next refresh is refused, the account reads
//!   "link again", and its bot stays off until then. Nothing else is lost.
//!
//! A graceful stop while a refresh hangs, which ends with exit 0, is
//! fleet-server's own (`tests/serve/control_plane.rs`).

use fleet_api_types::accounts::AccountStatus;
use fleet_core::disconnect::DisconnectReason;

use super::child::Trip;
use super::{after_the_fault, assert_sound, serve_in_child};
use crate::harness::agent::RunOptions;
use crate::harness::client::asked;
use crate::harness::microsoft::Stall;
use crate::harness::server::{Microsoft, ServerOptions};
use crate::harness::world::World;

/// What the agent logs once a stream welcomed it.
const CONNECTED: &str = "the agent is connected";

fn options(microsoft: Microsoft) -> ServerOptions {
    ServerOptions {
        backups: true,
        microsoft,
        ..ServerOptions::default()
    }
}

#[rstest::rstest]
#[case::before_microsoft_saw_the_request(Stall::BeforeTheRequest)]
#[case::after_microsoft_rotated_the_token(Stall::AfterTheRotation)]
#[tokio::test]
async fn a_server_killed_during_a_refresh_leaves_the_account_active_or_to_link_again(
    #[case] stall: Stall,
) {
    let mut world = World::new(options(Microsoft::Fake(None))).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(2));
    let bot = world.owner.link(&world.logs).await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    assert_eq!(
        world.owner.account_status(bot).await,
        Some(AccountStatus::Active)
    );
    world.server.stop().await;
    // The next server holds no session of the account: the first request
    // for one makes it refresh, and that refresh never answers. The child
    // says so itself: its own line stands in the options it reads.
    let mut child = serve_in_child(
        &world,
        options(Microsoft::Fake(Some((stall, || ())))),
        Trip::Never,
        false,
    )
    .await;
    world
        .logs
        .until("the agent is back", async || {
            world.logs.count(CONNECTED) == 2
        })
        .await;
    // The bot's server closes: it joins again, and asks for its session.
    world
        .ledger
        .end(bot, DisconnectReason::kicked(None, "Server closed"));

    child.says("TRIPPED").await;
    child.kill(&world);

    assert_sound(&world.server.config().database).await;
    world.server.start_again().await;
    match stall {
        Stall::BeforeTheRequest => {
            // The stored token is still good: the bot is back by itself.
            world.online(&agent, bot, 0).await;
            assert_eq!(world.ledger.sessions(bot), 2);
            assert_eq!(
                world.owner.account_status(bot).await,
                Some(AccountStatus::Active)
            );
        }
        Stall::AfterTheRotation => {
            // The stored token is spent: Microsoft refuses it, and the
            // account must be linked again.
            asked(&world.logs, "the account reads link again", async || {
                (world.owner.account_status(bot).await == Some(AccountStatus::ReauthRequired))
                    .then_some(())
            })
            .await;
            assert!(!world.ledger.live(bot));
            assert_eq!(world.ledger.sessions(bot), 1, "it joined without a session");
        }
    }
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}
