//! Sessions against an offline-mode server: joins and their messages, chat, a
//! kick, and a reconnect (Plan.md P3.7).
//!
//! A bot never sees its own join message: the server broadcasts it before it
//! adds the new player, so a second bot, the watcher, sees it instead (as in
//! the spike, P1.4).

use fleet_core::chat::ChatKind;
use fleet_core::disconnect::{DisconnectClass, DisconnectReason, TranslationKey};
use fleet_core::mc::{SessionEvent, SessionEvents, SessionHandle};
use fleet_mc::{AzaleaConnector, McConfig, McEvents};

use crate::harness::{Mode, Server, bot, connect, is_chat, offline, wait_for, within};

const NAME: &str = "AfkBot1";
const WATCHER: &str = "AfkBot2";

async fn joined(events: &mut McEvents, what: &str) {
    wait_for(events, what, |event| *event == SessionEvent::Joined).await;
}

/// Waits until `events` show the system message that `name` joined.
async fn sees_join_of(events: &mut McEvents, name: &str) {
    let message = format!("{name} joined the game");
    wait_for(
        events,
        &format!("the message that {name} joined"),
        |event| is_chat(event, ChatKind::System, None, &message),
    )
    .await;
}

#[tokio::test]
async fn slow_offline_server_scenario() {
    let server = Server::start(Mode::Offline).await;
    let connector = AzaleaConnector::new(&McConfig::default());

    // 1. Join, and see the next player's join message.
    let (session, mut events) = connect(&connector, &server, bot(1), offline(NAME)).await;
    joined(&mut events, "the bot's join").await;
    let (watcher, mut watcher_events) =
        connect(&connector, &server, bot(2), offline(WATCHER)).await;
    joined(&mut watcher_events, "the watcher's join").await;
    sees_join_of(&mut events, WATCHER).await;

    // 2. Chat, and see it echoed with the bot as its sender, by the bot and
    // by the watcher.
    let sent = session
        .send_chat("hello from afkfleet".parse().unwrap())
        .await;
    assert_eq!(sent, Ok(()));
    for (events, whose) in [
        (&mut events, "the bot's"),
        (&mut watcher_events, "the watcher's"),
    ] {
        wait_for(events, &format!("{whose} chat echo"), |event| {
            is_chat(event, ChatKind::Chat, Some(NAME), "hello from afkfleet")
        })
        .await;
    }

    // 3. A kick through RCON ends the session with its reason.
    server.rcon(&format!("kick {NAME}")).await;
    let ended = wait_for(&mut events, "the kick", |event| {
        matches!(event, SessionEvent::Disconnected(_))
    })
    .await;
    let SessionEvent::Disconnected(DisconnectReason::Kicked(kick)) = &ended else {
        panic!("expected a kick, got {ended:?}");
    };
    assert_eq!(
        kick.key().map(TranslationKey::as_str),
        Some("multiplayer.disconnect.kicked")
    );
    assert_eq!(
        DisconnectReason::Kicked(kick.clone()).classify(),
        DisconnectClass::Transient
    );
    assert_eq!(within("the end of the events", events.next()).await, None);
    within("the teardown", session.disconnect()).await;

    // 4. Reconnect under the same name; the watcher sees it join again.
    let (session, mut events) = connect(&connector, &server, bot(1), offline(NAME)).await;
    joined(&mut events, "the bot's rejoin").await;
    sees_join_of(&mut watcher_events, NAME).await;

    within("the teardown", session.disconnect()).await;
    within("the watcher's teardown", watcher.disconnect()).await;
    assert_eq!(connector.pool().live_threads(), 0);
}
