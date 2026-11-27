//! What a bot sees, on demand (Plan.md P16.20): a look at its session's
//! screen, through the bot's actor, bounded in time and one at a time.

use core::num::NonZeroU32;

use fleet_core::mc::{MenuView, Observation};
use fleet_core::script::Identifier;
use fleet_core::script::menu::{BotInventory, Item, ItemData, Screen, ScreenKind};
use fleet_runtime::{FleetError, LOOK_TIMEOUT, LookError};

use crate::harness::{BOT, OTHER, Setup, id, ms, settle};
use crate::scripted::seen;

/// A chest whose first slot holds a potion, and a sword in the hotbar.
fn view() -> MenuView {
    let item = |name: &str| {
        Item::new(
            Identifier::try_from(name).unwrap(),
            NonZeroU32::MIN,
            ItemData::new(Vec::new()),
        )
    };
    MenuView {
        inventory: BotInventory::new(
            (0..46).map(|index| (index == 36).then(|| item("minecraft:diamond_sword"))),
        ),
        screen: Some(Screen::shown(
            ScreenKind::Generic9x3,
            "Auction House",
            [Some(item("minecraft:potion"))],
            None,
        )),
        sign: None,
    }
}

fn showing(menu: MenuView) -> Observation {
    Observation { menu, ..seen() }
}

#[tokio::test(start_paused = true)]
async fn an_online_bot_shows_what_it_sees() {
    let harness = Setup::new().start().await;
    let controller = harness.online(BOT, "AfkBot1", 0).await;
    controller.show(showing(view()));

    let look = harness.fleet.look(id(BOT)).await.unwrap();

    assert_eq!(look.seen().await, Ok(view()));
}

#[tokio::test(start_paused = true)]
async fn a_bot_between_worlds_or_offline_shows_nothing() {
    let harness = Setup::new().start().await;
    // Online, but not placed in its world yet: nothing shown.
    let _controller = harness.online(BOT, "AfkBot1", 0).await;
    harness
        .apply(crate::harness::spec(
            OTHER,
            "AfkBot2",
            fleet_core::bot::DesiredRunState::Stopped,
        ))
        .await;

    let between = harness.fleet.look(id(BOT)).await.unwrap().seen().await;
    let stopped = harness.fleet.look(id(OTHER)).await.unwrap().seen().await;

    assert_eq!(between, Err(LookError::NotOnline));
    assert_eq!(stopped, Err(LookError::NotOnline));
}

#[tokio::test(start_paused = true)]
async fn a_second_look_while_one_runs_is_busy_and_the_first_still_answers() {
    let harness = Setup::new().start().await;
    let controller = harness.online(BOT, "AfkBot1", 0).await;
    controller.show(showing(view()));
    controller.delay_observe(ms(300));

    let first = harness.fleet.look(id(BOT)).await.unwrap();
    settle().await;
    let second = harness.fleet.look(id(BOT)).await.unwrap().seen().await;

    assert_eq!(second, Err(LookError::Busy));
    assert_eq!(first.seen().await, Ok(view()));
    // Once it ended, the next one is taken.
    controller.delay_observe(ms(0));
    let third = harness.fleet.look(id(BOT)).await.unwrap().seen().await;
    assert_eq!(third, Ok(view()));
}

#[tokio::test(start_paused = true)]
async fn a_session_that_doesnt_answer_is_timed_out_within_the_bound() {
    let harness = Setup::new().start().await;
    let controller = harness.online(BOT, "AfkBot1", 0).await;
    controller.show(showing(view()));
    controller.delay_observe(LOOK_TIMEOUT * 10);
    let started = tokio::time::Instant::now();

    let look = harness.fleet.look(id(BOT)).await.unwrap().seen().await;

    assert_eq!(look, Err(LookError::TimedOut));
    assert!(
        started.elapsed() <= LOOK_TIMEOUT + ms(50),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn a_hung_session_answers_timed_out_at_once() {
    let harness = Setup::new().start().await;
    let controller = harness.online(BOT, "AfkBot1", 0).await;
    controller.show(showing(view()));
    controller.hang();

    let look = harness.fleet.look(id(BOT)).await.unwrap().seen().await;

    assert_eq!(look, Err(LookError::TimedOut));
}

#[tokio::test(start_paused = true)]
async fn a_bot_the_fleet_doesnt_run_is_refused() {
    let harness = Setup::new().start().await;

    let refused = harness.fleet.look(id(BOT)).await.map(|_| ());

    assert_eq!(refused, Err(FleetError::UnknownBot));
}
