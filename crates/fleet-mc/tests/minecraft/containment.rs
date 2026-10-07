//! A panic in one session's ECS stays in that session (Plan.md P3.7;
//! ADR-0008 §2–3 and §5, ADR-0011). It needs the `fault-injection` feature.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use azalea::prelude::GameTick;
use fleet_core::chat::ChatKind;
use fleet_core::disconnect::DisconnectReason;
use fleet_core::id::BotId;
use fleet_core::mc::{SessionEvent, SessionHandle};
use fleet_core::mode::GameAction;
use fleet_mc::{AzaleaConnector, McConfig};

use crate::harness::{Mode, Server, bot, connect, is_chat, offline, wait_for, within};

/// An App hook that adds a panicking system to `victim`'s App, armed by
/// `trigger`. Other sessions' Apps are left alone.
fn panic_on_trigger(
    victim: BotId,
    trigger: &Arc<AtomicBool>,
) -> impl Fn(BotId, &mut azalea::app::App) + Send + Sync + 'static {
    let trigger = Arc::clone(trigger);
    move |bot_id, app| {
        if bot_id != victim {
            return;
        }
        let trigger = Arc::clone(&trigger);
        app.add_systems(GameTick, move || {
            assert!(!trigger.load(Ordering::SeqCst), "injected panic (P3.7)");
        });
    }
}

#[tokio::test]
async fn slow_fault_containment_scenario() {
    let server = Server::start(Mode::Offline).await;
    let trigger = Arc::new(AtomicBool::new(false));
    let connector = AzaleaConnector::new(&McConfig::default())
        .with_app_hook(panic_on_trigger(bot(1), &trigger));
    let (victim, mut victim_events) =
        connect(&connector, &server, bot(1), offline("AfkBot1")).await;
    let (bystander, mut bystander_events) =
        connect(&connector, &server, bot(2), offline("AfkBot2")).await;
    wait_for(&mut victim_events, "the victim's join", |event| {
        *event == SessionEvent::Joined
    })
    .await;
    wait_for(&mut bystander_events, "the bystander's join", |event| {
        *event == SessionEvent::Joined
    })
    .await;

    // A panic in the victim's ECS ends the victim as crashed.
    trigger.store(true, Ordering::SeqCst);
    wait_for(&mut victim_events, "the victim's crash", |event| {
        *event == SessionEvent::Disconnected(DisconnectReason::SessionCrashed)
    })
    .await;

    // The bystander, on the same pool, keeps ticking...
    let before = bystander.liveness().last_tick;
    within("a bystander tick", async {
        while bystander.liveness().last_tick <= before {
            tokio::task::yield_now().await;
        }
    })
    .await;
    // ...chats...
    let sent = bystander.send_chat("still here".parse().unwrap()).await;
    assert_eq!(sent, Ok(()));
    wait_for(
        &mut bystander_events,
        "the bystander's chat echo",
        |event| is_chat(event, ChatKind::Chat, Some("AfkBot2"), "still here"),
    )
    .await;
    // ...and acts.
    let looked = bystander
        .perform(GameAction::Look {
            yaw: 45.0,
            pitch: 20.0,
        })
        .await;
    assert_eq!(looked, Ok(()));
    server.sees_rotation("AfkBot2", 45.0, 20.0).await;

    within("the victim's teardown", victim.disconnect()).await;
    within("the bystander's teardown", bystander.disconnect()).await;
    assert_eq!(connector.pool().live_threads(), 0);
    // The crashed session's thread was shut down, not abandoned.
    assert_eq!(connector.pool().abandoned_threads(), 0);
}
