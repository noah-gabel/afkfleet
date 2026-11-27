//! What a bot sees, on demand (Plan.md P16.20): the conductor answers a
//! screen request from the bot's snapshot, on the stream that asked, within
//! a bound, and never more looks at once than the fleet runs bots.

use core::num::{NonZeroU32, NonZeroU64};

use fleet_agent::managed::conductor::Instruction;
use fleet_core::mc::{MenuView, Observation};
use fleet_core::mode::HotbarSlot;
use fleet_core::script::menu::{BotInventory, Item, ItemData, Screen, ScreenKind};
use fleet_core::script::{BlockPosition, Blocks, BotFacts, Identifier, Position, Snapshot};
use fleet_proto::control::{
    AgentMessage, ScreenAnswer, ScreenOutcome, ScreenRequest, ScreenRequestId, ScreenUnseen, Seen,
};
use fleet_runtime::LOOK_TIMEOUT;

use crate::rig::{Rig, assign, bot, runtime, secs, spec};

fn number(n: u64) -> ScreenRequestId {
    ScreenRequestId::new(NonZeroU64::new(n).unwrap())
}

fn read(n: u64, bot_n: u8) -> Instruction {
    Instruction::ReadScreen(ScreenRequest {
        id: number(n),
        bot_id: bot(bot_n),
    })
}

/// A chest whose first slot holds a potion, with data the guard reads.
fn view() -> MenuView {
    let potion = Item::new(
        Identifier::try_from("minecraft:potion").unwrap(),
        NonZeroU32::MIN,
        ItemData::new(vec![7, 7]),
    )
    .with_potion(Some("minecraft:turtle_master"));
    MenuView {
        inventory: BotInventory::new(core::iter::empty()),
        screen: Some(Screen::shown(
            ScreenKind::Generic9x3,
            "Auction House",
            [Some(potion)],
            None,
        )),
        sign: None,
    }
}

/// The bot placed in its world, seeing `menu`.
fn showing(menu: MenuView) -> Observation {
    let place = Position::try_new(0.5, 64.0, 0.5).unwrap();
    Observation {
        facts: Snapshot::new(
            BotFacts {
                health: 20.0,
                food: 20,
                position: place,
                yaw: 0.0,
                pitch: 0.0,
                dimension: Identifier::try_from("overworld").unwrap(),
                riding: None,
                held: None,
                selected: HotbarSlot::FIRST,
                looking_at: None,
            },
            Vec::new(),
            Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |_| None),
        ),
        anchor: Some(place),
        menu,
    }
}

/// The screen answers sent, in order.
fn answers(rig: &Rig) -> Vec<ScreenAnswer> {
    rig.sent
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Screen(answer) => Some(answer.clone()),
            _ => None,
        })
        .collect()
}

fn unseen(n: u64, bot_n: u8, why: ScreenUnseen) -> ScreenAnswer {
    ScreenAnswer {
        id: number(n),
        bot_id: bot(bot_n),
        outcome: ScreenOutcome::Unseen(why),
    }
}

#[tokio::test(start_paused = true)]
async fn a_screen_request_is_answered_with_what_the_bot_sees_without_the_guards_parts() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    let session = rig.join(0).await;
    session.show(showing(view()));

    rig.instruct(read(1, 1)).await;
    rig.pump().await;

    assert_eq!(
        answers(&rig),
        [ScreenAnswer {
            id: number(1),
            bot_id: bot(1),
            outcome: ScreenOutcome::Seen(Box::new(Seen::of(&view()))),
        }]
    );
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_bot_that_isnt_in_a_world_or_isnt_run_here_is_answered_so_at_once() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    // Connected, never placed in its world.
    let _session = rig.join(0).await;

    rig.instruct(read(1, 1)).await;
    rig.instruct(read(2, 3)).await;
    rig.pump().await;

    assert_eq!(
        answers(&rig),
        [
            unseen(1, 1, ScreenUnseen::NotOnline),
            unseen(2, 3, ScreenUnseen::UnknownBot)
        ]
    );
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_session_that_doesnt_answer_is_timed_out_within_the_bound() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    let session = rig.join(0).await;
    session.show(showing(view()));
    session.delay_observe(secs(60));

    rig.instruct(read(1, 1)).await;
    tokio::time::sleep(LOOK_TIMEOUT + secs(1)).await;
    rig.pump().await;

    assert_eq!(answers(&rig), [unseen(1, 1, ScreenUnseen::TimedOut)]);
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn past_as_many_waiting_looks_as_the_fleet_runs_bots_a_request_is_busy_at_once() {
    let max_bots = runtime().max_bots.get();
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(
        (1..=4).map(|n| assign(spec(n), None)).collect(),
    ))
    .await;
    let mut sessions = Vec::new();
    for index in 0..max_bots {
        let session = rig.join(index).await;
        session.show(showing(view()));
        session.delay_observe(secs(1));
        sessions.push(session);
    }

    for n in 1..=4_u8 {
        rig.instruct(read(u64::from(n), n)).await;
    }
    rig.instruct(read(5, 1)).await;
    rig.pump().await;
    let at_once = answers(&rig);
    tokio::time::sleep(secs(2)).await;
    rig.pump().await;

    assert_eq!(at_once, [unseen(5, 1, ScreenUnseen::Busy)]);
    assert_eq!(answers(&rig).len(), 5, "{:?}", answers(&rig));
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_screen_request_of_an_ended_stream_is_dropped_unanswered() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    let session = rig.join(0).await;
    session.show(showing(view()));
    session.delay_observe(secs(1));
    rig.instruct(read(1, 1)).await;
    let old = rig.generation();

    // The stream ends while the look runs; one of the old stream comes late.
    rig.end();
    rig.inbox.instruction(old, read(2, 1)).unwrap();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    tokio::time::sleep(secs(2)).await;
    rig.pump().await;

    assert_eq!(answers(&rig), []);
    rig.stop().await;
}
