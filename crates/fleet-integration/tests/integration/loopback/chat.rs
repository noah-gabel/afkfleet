//! A user's chat line end to end (Plan.md P11.3; ADR-0020, S5): through
//! fleet-client and serve's own wiring to the managed agent's stream, the
//! bot's session, and its answer back over the stream, the ingest and the
//! bus the send waits on. The HTTP tests answer for the agent on the bus;
//! this is the one case where the real ingest does. The line then reads back
//! in the bot's history as the sender's own (P11.3; ADR-0020, U3).

use fleet_api_types::ErrorCode;
use fleet_api_types::chat::{ChatLineKind, ChatSendOutcome};
use fleet_core::chat::ChatMessage;
use fleet_core::mc::SessionError;
use fleet_testkit::mc::Performed;

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

#[tokio::test]
async fn a_line_sent_through_the_api_reaches_the_bots_session_and_its_answer_comes_back() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;

    let sent = world.owner.chat(bot, "hello there").await.unwrap();
    let session = agent.fake().try_session(0).unwrap();
    let said: Vec<Performed> = session
        .log()
        .into_iter()
        .filter(|performed| matches!(performed, Performed::Chat(_)))
        .collect();
    session.fail_chat(SessionError::ChatUnavailable);
    let refused = world.owner.chat(bot, "and again").await.unwrap_err();

    assert_eq!(sent.outcome.known(), Some(ChatSendOutcome::Sent));
    assert_eq!(
        said,
        [Performed::Chat(
            ChatMessage::try_from("hello there").unwrap()
        )]
    );
    let failure = refused.api().unwrap();
    assert_eq!(failure.status, 409, "{refused:?}");
    assert!(failure.is(ErrorCode::ChatUnavailable), "{refused:?}");
    world.end(vec![agent]).await;
}

/// A line sent through the API reads back as the newest line of the bot's
/// history: the sender's own, and named (the Owner sees every sender).
#[tokio::test]
async fn a_sent_line_reads_back_in_the_history_as_the_senders_own() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;

    let sent = world.owner.chat(bot, "hello history").await.unwrap();
    let page = world.owner.history(bot).await;

    let newest = page.items.first().unwrap();
    assert_eq!(newest.id, sent.message_id);
    assert_eq!(newest.kind.known(), Some(ChatLineKind::User));
    assert_eq!(newest.text.as_str(), "hello history");
    assert!(!newest.arguments_hidden);
    assert!(newest.mine);
    assert_eq!(
        newest.sent_by.as_ref().map(|by| by.username.as_str()),
        Some(crate::harness::server::OWNER)
    );
    world.end(vec![agent]).await;
}
