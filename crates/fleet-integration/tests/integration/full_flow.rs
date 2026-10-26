//! P11.7's full flow (Plan.md P11.7; ADR-0020, S58, R78): the real server
//! and the real managed agent in-process, the bot on the fake connector,
//! and the Owner's live events over fleet-client's own socket. A bot started
//! through fleet-client is seen Online, chats in and out, and stops, every
//! `bot` frame as `GET /bots/{id}` shows it at that moment and every chat
//! line as the history stores it.

use fleet_api_types::Open;
use fleet_api_types::bots::{BotResponse, BotStateKind};
use fleet_client::RefetchReason;
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::mc::SessionEvent;
use fleet_testkit::mc::{EmitOutcome, Performed};

use crate::harness::agent::RunOptions;
use crate::harness::live::{Live, Update};
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// Whether `bot`'s status is `state`.
fn is(bot: &BotResponse, state: BotStateKind) -> bool {
    bot.status.state.as_ref().and_then(Open::known) == Some(state)
}

#[tokio::test]
async fn a_started_bot_is_seen_online_chatting_both_ways_and_stopped_as_get_shows_it() {
    let world = World::new(ServerOptions::default()).await;
    let mut agent = world.agent("agent-a").await;
    agent.start(&world.ledger, RunOptions::capacity(1));
    let bot = world.owner.bot("AfkBot1").await;
    let live = world.owner.live();
    world
        .logs
        .until("the socket's first fetch", async || live.refetches() == 1)
        .await;

    // 1, 2: started through fleet-client, seen Online on the socket.
    world.owner.start(bot).await;
    let online = world
        .live_settles(&live, bot, "the bot online on the socket", |shown| {
            is(shown, BotStateKind::Online)
        })
        .await;

    // 3, in: two lines from the server the bot plays on.
    let session = agent.fake().try_session(0).unwrap();
    let texts = ["the first line in", "the second line in"];
    for text in texts {
        let emitted = session.emit(SessionEvent::Chat(IncomingChat::system(text)));
        assert_eq!(emitted, EmitOutcome::Queued);
    }
    world
        .logs
        .until("both lines on the socket", async || {
            live.lines_in(bot).len() == texts.len()
        })
        .await;
    let lines_in = live.lines_in(bot);
    let stored = world.owner.history(bot).await;

    // 3, out: the user's line, its frame before its `chat_sent`.
    let sent = world.owner.chat(bot, "hello out there").await.unwrap();
    world
        .logs
        .until("the line's chat_sent on the socket", async || {
            live.sent(bot, sent.request_id).1.is_some()
        })
        .await;
    let (line_out, sent_at) = live.sent(bot, sent.request_id);
    let (line_at, line_out) = line_out.expect("the user's line came as a chat frame");
    let stored_out = world.owner.history(bot).await;
    let performed = session.log();

    // 4: stopped, the session gone.
    world.owner.stop(bot).await;
    world
        .live_settles(&live, bot, "the bot stopped on the socket", |shown| {
            is(shown, BotStateKind::Stopped) && !world.ledger.live(bot)
        })
        .await;
    let updates = live.updates();
    live.close().await;

    assert!(online.agent.is_some(), "{online:?}");
    assert_eq!(
        lines_in
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        texts
    );
    for line in &lines_in {
        let kept = stored.items.iter().find(|kept| kept.id == line.id);
        assert_eq!(kept, Some(line), "the history's line {}", line.id);
    }
    assert!(
        line_at < sent_at.unwrap(),
        "the line came before its chat_sent"
    );
    assert_eq!(line_out.id, sent.message_id);
    let kept_out = stored_out.items.iter().find(|kept| kept.id == line_out.id);
    assert_eq!(kept_out, Some(&line_out));
    assert!(
        performed.contains(&Performed::Chat(
            ChatMessage::try_from("hello out there").unwrap()
        )),
        "{performed:?}"
    );
    let refetches: Vec<&Update> = updates
        .iter()
        .filter(|update| matches!(update, Update::Refetch(_)))
        .collect();
    assert!(
        matches!(
            refetches.as_slice(),
            [Update::Refetch(RefetchReason::Ready)]
        ),
        "one fetch, at the start: {refetches:?}"
    );
    assert_eq!(Live::drops(&updates), Vec::<String>::new());
    world.end(vec![agent]).await;
}
