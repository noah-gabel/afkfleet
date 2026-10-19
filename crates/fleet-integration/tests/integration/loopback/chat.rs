//! A user's chat line end to end (Plan.md P11.3; ADR-0020, S5): through
//! fleet-client and serve's own wiring to the managed agent's stream, the
//! bot's session, and its answer back over the stream, the ingest and the
//! bus the send waits on. The HTTP tests answer for the agent on the bus;
//! this is the one case where the real ingest does.

use fleet_api_types::ErrorCode;
use fleet_api_types::chat::ChatSendOutcome;
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
