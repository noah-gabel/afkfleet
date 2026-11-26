//! A managed agent and a scripted mode (Plan.md P15.20, P15.22): its own
//! engine refuses a script per bot, its stream staying up, and a script's
//! status and log reach the stream as the wire's events.

use fleet_core::bot::{BotSpec, BotState};
use fleet_core::mc::{Observation, SessionEvent};
use fleet_core::mode::{HotbarSlot, ModeDefinition};
use fleet_core::script::{
    BlockPosition, Blocks, BotFacts, Identifier, LogKind, Position, ProblemKind, Script,
    ScriptDraft, ScriptLogLine, ScriptPlace, ScriptState, Snapshot,
};
use fleet_proto::control::BotEventKind;
use fleet_testkit::mc::{EmitOutcome, SessionController};

use crate::rig::{Rig, assign, secs, spec};

/// Bot `n` on the script `source`, with the mode's defaults and nothing
/// declared.
fn scripted(n: u8, source: &str) -> BotSpec {
    let script = Script::try_from(ScriptDraft {
        api: 1,
        source: source.to_owned(),
        leash: 8,
        tool_guard: 2,
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
        signs: Vec::new(),
    })
    .unwrap();
    BotSpec {
        mode: ModeDefinition::scripted(script),
        ..spec(n)
    }
}

/// What the fake session shows a bot placed at the overworld's origin.
fn placed() -> Observation {
    let place = Position::try_new(0.5, 64.0, 0.5).unwrap();
    let bot = BotFacts {
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
    };
    let blocks = Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |_| None);
    Observation {
        facts: Snapshot::new(bot, Vec::new(), blocks),
        anchor: Some(place),
        menu: fleet_core::mc::MenuView::default(),
    }
}

/// The `index`-th session, placed in its world and joined; the stream
/// pumped until it would wait.
async fn join_placed(rig: &mut Rig, index: usize) -> SessionController {
    rig.pump().await;
    let session = tokio::time::timeout(secs(5), rig.fake.session(index))
        .await
        .expect("the bot connects");
    session.show(placed());
    assert_eq!(session.emit(SessionEvent::Joined), EmitOutcome::Queued);
    // A script's compile runs in `spawn_blocking`: a sleep waits for it,
    // then for its length (.claude/rules/testing.md).
    tokio::time::sleep(secs(1)).await;
    rig.pump().await;
    session
}

/// Bot `n`'s script's statuses on the stream.
fn statuses(rig: &Rig, n: u8) -> Vec<ScriptState> {
    rig.events(n)
        .into_iter()
        .filter_map(|kind| match kind {
            BotEventKind::ScriptStatus(state) => Some(state),
            _ => None,
        })
        .collect()
}

/// Bot `n`'s script's log lines on the stream.
fn log_lines(rig: &Rig, n: u8) -> Vec<ScriptLogLine> {
    rig.events(n)
        .into_iter()
        .filter_map(|kind| match kind {
            BotEventKind::ScriptLog(line) => Some(line),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_script_its_engine_refuses_is_refused_for_its_bot_alone_and_the_stream_stays_up() {
    let mut rig = Rig::new();
    rig.hello().await;
    let generation = rig.generation();

    rig.instruct(fleet_agent::managed::conductor::Instruction::Reconcile(
        vec![
            assign(
                scripted(1, "fn on_join(first) {\n    log.info(kees);\n}\n"),
                None,
            ),
            assign(spec(2), None),
        ],
    ))
    .await;
    join_placed(&mut rig, 0).await;
    join_placed(&mut rig, 1).await;

    let refused = statuses(&rig, 1);
    let [ScriptState::Refused(problem)] = refused.as_slice() else {
        panic!("one refusal for bot 1: {:?}", rig.sent);
    };
    assert_eq!(problem.kind, ProblemKind::Compile);
    assert_eq!(problem.hook, None);
    assert_eq!(problem.place, ScriptPlace::new(2, 14));
    assert_eq!(problem.message.as_str(), "`kees` isn't defined");
    // Its bot stays online and idle; the other bot and the stream go on.
    assert!(
        rig.states(1)
            .iter()
            .any(|state| matches!(state, BotState::Online { .. }))
    );
    assert!(
        rig.states(2)
            .iter()
            .any(|state| matches!(state, BotState::Online { .. }))
    );
    assert_eq!(statuses(&rig, 2), []);
    assert!(rig.outbox.is_live(generation));
    assert!(!rig.outbox.overflowed());
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_running_scripts_status_and_log_lines_reach_the_stream() {
    let mut rig = Rig::new();
    rig.hello().await;

    rig.instruct(fleet_agent::managed::conductor::Instruction::Reconcile(
        vec![assign(
            scripted(
                1,
                "fn on_join(first) {\n    log.info(`joined ${first}`);\n}\n",
            ),
            None,
        )],
    ))
    .await;
    join_placed(&mut rig, 0).await;

    assert!(
        statuses(&rig, 1).contains(&ScriptState::Running),
        "{:?}",
        rig.sent
    );
    let lines: Vec<(LogKind, String)> = log_lines(&rig, 1)
        .into_iter()
        .map(|line| (line.kind, line.text.as_str().to_owned()))
        .collect();
    assert_eq!(lines, [(LogKind::Line, "joined true".to_owned())]);
    rig.stop().await;
}
