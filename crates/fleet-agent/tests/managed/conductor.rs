//! The conductor (Plan.md P10.7; ADR-0019, S65, R47–R49) on paused time: a
//! real fleet on the fake connector, the real outbox and credentials, the
//! test playing the link and the server.

use chrono::DateTime;
use core::num::NonZeroUsize;
use fleet_agent::config::{LogConfig, LogFilter, LogFormat};

use fleet_agent::managed::conductor::{BarrierError, INBOUND_QUEUE, InboxError, Instruction};
use fleet_agent::managed::credentials::LinkGeneration;
use fleet_agent::telemetry::layer;
use fleet_core::bot::{BotState, DesiredRunState, Lifecycle, PauseReason, StickyState};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::ChatRequestId;
use fleet_core::mc::{SessionCredentialProvider as _, SessionEvent, SessionRequest};
use fleet_proto::control::{
    AgentMessage, BotEventKind, ChatFailedReason, LifecycleCommand, SendChat,
};
use fleet_runtime::FleetError;
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::mc::EmitOutcome;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::rig::{Rig, assign, bot, runtime, secs, settle, spec, spec_on};

fn paused() -> StickyState {
    StickyState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    })
}

fn request(n: u8) -> ChatRequestId {
    ChatRequestId::new_v7(DateTime::UNIX_EPOCH, [n; 10]).unwrap()
}

fn chat(n: u8, request_n: u8) -> Instruction {
    Instruction::SendChat(SendChat {
        request: request(request_n),
        bot_id: bot(n),
        message: ChatMessage::try_from("hello").unwrap(),
    })
}

fn capture() -> (LogBuffer, DefaultGuard) {
    let logs = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: LogFilter::try_from("debug").unwrap(),
    };
    let subscriber = tracing_subscriber::registry().with(layer(&config, false, logs.clone()));
    (logs, tracing::subscriber::set_default(subscriber))
}

const WAITS: &str = "a bot waits for another bot's removal";

#[tokio::test(start_paused = true)]
async fn a_reconciled_bot_runs_and_its_status_goes_out() {
    let mut rig = Rig::new();
    rig.hello().await;

    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.join(0).await;

    assert!(
        rig.states(1)
            .iter()
            .any(|state| matches!(state, BotState::Online { .. })),
        "{:?}",
        rig.sent
    );
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_assignment_starts_its_bot_in_its_restore_without_connecting() {
    let mut rig = Rig::new();
    rig.hello().await;

    rig.instruct(Instruction::Reconcile(vec![assign(
        spec(1),
        Some(paused()),
    )]))
    .await;

    let snapshot = rig.fleet.snapshot(bot(1)).await.unwrap();
    assert_eq!(snapshot.state, BotState::from(paused()));
    assert_eq!(rig.fake.session_count(), 0);
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_spec_that_says_stopped_stops_the_bot_and_keeps_it() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.join(0).await;
    let mut stopped = spec(1);
    stopped.desired = DesiredRunState::Stopped;

    rig.instruct(Instruction::Update(stopped)).await;

    assert_eq!(rig.states(1).last(), Some(&BotState::Stopped));
    assert_eq!(rig.running().await, [bot(1)], "kept");
    assert!(!rig.removed(1));
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_reconcile_removes_what_it_leaves_out_before_it_applies() {
    let (logs, _guard) = capture();
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.join(0).await;

    // Bot 2 plays bot 1's account: it can run only once bot 1 is gone.
    rig.instruct(Instruction::Reconcile(vec![assign(spec_on(2, 1), None)]))
        .await;
    rig.pump().await;
    tokio::time::sleep(crate::rig::secs(2)).await;
    rig.pump().await;

    assert!(rig.removed(1), "{:?}", rig.sent);
    assert_eq!(rig.running().await, [bot(2)]);
    assert_eq!(logs.lines_with(WAITS).unwrap().len(), 0, "{}", logs.text());
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_unassign_of_a_bot_it_doesnt_run_is_answered_with_removed() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(Vec::new())).await;

    rig.instruct(Instruction::Unassign(bot(7))).await;

    assert_eq!(rig.events(7), [BotEventKind::Removed]);
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_bot_whose_account_is_in_use_waits_for_a_removed_with_one_warn() {
    let (logs, _guard) = capture();
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.join(0).await;

    rig.instruct(Instruction::Assign(assign(spec_on(2, 1), None)))
        .await;
    let waiting = rig.running().await;
    tokio::time::sleep(crate::rig::secs(3)).await;
    rig.pump().await;
    let still = rig.running().await;
    rig.instruct(Instruction::Unassign(bot(1))).await;
    rig.pump().await;

    assert_eq!(waiting, [bot(1)]);
    assert_eq!(still, [bot(1)], "not tried on the tick");
    assert_eq!(rig.running().await, [bot(2)]);
    let warned = logs.lines_with(WAITS).unwrap();
    assert_eq!(warned.len(), 1, "{}", logs.text());
    assert_eq!(warned[0]["level"], "WARN");
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_update_of_a_bot_this_stream_didnt_give_is_dropped() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(Vec::new())).await;

    rig.instruct(Instruction::Update(spec(1))).await;

    assert_eq!(rig.running().await, []);
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn the_barrier_applies_what_was_queued_first_and_answers_the_fleets_snapshot() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(Vec::new())).await;
    // Queued, not yet taken, when the next hello's barrier comes.
    rig.inbox
        .instruction(rig.generation(), Instruction::Assign(assign(spec(1), None)))
        .unwrap();

    let snapshot = rig.hello().await;

    assert_eq!(
        snapshot.iter().map(|bot| bot.bot_id).collect::<Vec<_>>(),
        [bot(1)]
    );
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_barrier_clears_what_was_pending_so_the_reconcile_alone_decides() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.join(0).await;
    rig.instruct(Instruction::Assign(assign(spec_on(2, 1), None)))
        .await;

    // A new stream: the server gives bot 1 alone, then takes it.
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.instruct(Instruction::Unassign(bot(1))).await;
    rig.pump().await;

    assert_eq!(rig.running().await, [], "bot 2 was the old stream's");
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_lifecycle_command_makes_one_fleet_call_while_its_stream_lives() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(
        spec(1),
        Some(paused()),
    )]))
    .await;
    let old = rig.generation();

    rig.instruct(Instruction::Lifecycle(LifecycleCommand {
        bot_id: bot(1),
        command: Lifecycle::Resume,
    }))
    .await;
    rig.pump().await;
    let resumed = rig.fake.session_count();
    rig.end();
    rig.inbox
        .instruction(
            old,
            Instruction::Lifecycle(LifecycleCommand {
                bot_id: bot(1),
                command: Lifecycle::Restart,
            }),
        )
        .unwrap();
    rig.pump().await;

    assert_eq!(resumed, 1);
    assert_eq!(
        rig.fake.session_count(),
        1,
        "the ended stream's restart is dropped"
    );
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_chat_message_is_answered_by_its_ticket() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.join(0).await;

    rig.instruct(chat(1, 1)).await;
    rig.pump().await;

    assert!(
        rig.events(1).contains(&BotEventKind::ChatSent {
            request: request(1)
        }),
        "{:?}",
        rig.sent
    );
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_chat_message_to_a_bot_that_isnt_online_fails_at_once() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(
        spec(1),
        Some(paused()),
    )]))
    .await;

    rig.instruct(chat(1, 2)).await;

    assert!(
        rig.events(1).contains(&BotEventKind::ChatFailed {
            request: request(2),
            reason: ChatFailedReason::NotOnline,
        }),
        "{:?}",
        rig.sent
    );
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_chat_message_of_an_ended_stream_is_dropped_unanswered() {
    let mut rig = Rig::new();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.join(0).await;
    let old = rig.generation();
    rig.end();

    rig.inbox.instruction(old, chat(1, 3)).unwrap();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
        .await;
    rig.pump().await;

    let answers: Vec<BotEventKind> = rig
        .events(1)
        .into_iter()
        .filter(|kind| {
            matches!(
                kind,
                BotEventKind::ChatSent { .. } | BotEventKind::ChatFailed { .. }
            )
        })
        .collect();
    assert_eq!(answers, []);
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_stopped_spec_and_an_unassign_forget_the_bots_kept_grant() {
    for stop_it in [true, false] {
        let mut rig = Rig::new();
        rig.hello().await;
        rig.instruct(Instruction::Reconcile(vec![assign(spec(1), None)]))
            .await;
        rig.join(0).await;

        if stop_it {
            let mut stopped = spec(1);
            stopped.desired = DesiredRunState::Stopped;
            rig.instruct(Instruction::Update(stopped)).await;
        } else {
            rig.instruct(Instruction::Unassign(bot(1))).await;
        }
        rig.end();
        let kept = rig
            .credentials
            .session(SessionRequest {
                bot_id: bot(1),
                account: spec(1).account,
                fresh: false,
            })
            .await;

        assert!(kept.is_err(), "{stop_it}: the kept grant is gone");
        rig.stop().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_lag_resyncs_a_status_for_every_bot() {
    let mut config = runtime();
    config.event_buffer = core::num::NonZeroUsize::new(2).unwrap();
    let mut rig = Rig::parked(config);
    // The bots run before the conductor reads anything: it lags.
    for n in 1..=3 {
        rig.fleet.apply(spec(n), Some(paused())).await.unwrap();
    }
    crate::rig::settle().await;
    rig.open(Vec::new()).await;

    rig.conduct();
    rig.pump().await;

    for n in 1..=3 {
        assert_eq!(
            rig.states(n),
            [BotState::from(paused())],
            "bot {n}: {:?}",
            rig.sent
        );
    }
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_refuses_the_next_instruction() {
    // Held back: it reads nothing.
    let rig = Rig::parked(runtime());
    let generation = LinkGeneration::new(1);

    for _ in 0..INBOUND_QUEUE {
        rig.inbox
            .instruction(generation, Instruction::Unassign(bot(9)))
            .unwrap();
    }
    let refused = rig
        .inbox
        .instruction(generation, Instruction::Unassign(bot(9)));

    assert_eq!(refused, Err(InboxError::Full));
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_busy_removal_never_stops_a_bot_the_next_reconcile_gives() {
    // One call fits the supervisor's queue: a second is refused Busy.
    let mut config = runtime();
    config.supervisor_queue = NonZeroUsize::MIN;
    let mut rig = Rig::parked(config);
    rig.conduct();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![assign(
        spec(1),
        Some(paused()),
    )]))
    .await;
    // The supervisor is held, and a call fills its queue: the removal the
    // unassignment asks for is refused.
    rig.supervisor_gate.close();
    let fleet = rig.fleet.clone();
    let filler = tokio::spawn(async move { fleet.snapshot_all().await });
    settle().await;
    rig.instruct(Instruction::Unassign(bot(1))).await;
    rig.supervisor_gate.open();
    let _ = filler.await;

    // The stream drops; the hello lists bot 1, and its reconcile gives it
    // again.
    rig.hello().await;
    let before = rig.sent.len();
    rig.instruct(Instruction::Reconcile(vec![assign(
        spec(1),
        Some(paused()),
    )]))
    .await;
    tokio::time::sleep(secs(3)).await;
    rig.pump().await;

    assert_eq!(rig.running().await, [bot(1)], "it keeps running");
    let removed: Vec<&AgentMessage> = rig.sent[before..]
        .iter()
        .filter(|message| {
            matches!(message, AgentMessage::Event(event) if event.kind == BotEventKind::Removed)
        })
        .collect();
    assert_eq!(removed, Vec::<&AgentMessage>::new());
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_bot_past_the_fleets_capacity_waits_for_a_removed_with_one_warn() {
    let (logs, _guard) = capture();
    let mut rig = Rig::new();
    rig.hello().await;

    // Four bots fill the fleet; the fifth waits.
    rig.instruct(Instruction::Reconcile(
        (1..=5).map(|n| assign(spec(n), Some(paused()))).collect(),
    ))
    .await;
    tokio::time::sleep(secs(3)).await;
    rig.pump().await;
    let waiting = rig.running().await;
    rig.instruct(Instruction::Unassign(bot(1))).await;
    rig.pump().await;

    assert_eq!(
        waiting,
        [bot(1), bot(2), bot(3), bot(4)],
        "not tried on the tick"
    );
    assert_eq!(rig.running().await, [bot(2), bot(3), bot(4), bot(5)]);
    let warned = logs.lines_with(WAITS).unwrap();
    assert_eq!(warned.len(), 1, "{}", logs.text());
    assert_eq!(warned[0]["level"], "WARN");
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_lag_answers_with_removed_only_for_a_bot_being_removed_that_the_snapshot_lacks() {
    let mut config = runtime();
    config.event_buffer = NonZeroUsize::new(4).unwrap();
    let mut rig = Rig::parked(config);
    rig.conduct();
    rig.hello().await;
    rig.instruct(Instruction::Reconcile(vec![
        assign(spec(1), None),
        assign(spec(2), None),
        assign(spec(3), None),
    ]))
    .await;
    for index in 0..3 {
        rig.join(index).await;
    }
    let session_of = |n: u8| {
        let index = rig
            .fake
            .connects()
            .iter()
            .position(|connect| connect.bot_id == bot(n))
            .unwrap();
        rig.fake.try_session(index).unwrap()
    };
    let (first, second, third) = (session_of(1), session_of(2), session_of(3));
    // Bot 1 leaves within a second, bot 2 within a minute.
    first.delay_disconnect(secs(1));
    second.delay_disconnect(secs(60));
    rig.instruct(Instruction::Unassign(bot(1))).await;
    rig.instruct(Instruction::Unassign(bot(2))).await;

    // The conductor reads nothing while bot 1's Removed and then bot 3's
    // chat fill the fleet's buffer: the Removed is lost in a lag.
    rig.conductor_gate.close();
    tokio::time::sleep(secs(2)).await;
    for n in 0..8 {
        assert_eq!(
            third.emit(SessionEvent::Chat(IncomingChat::system(&format!(
                "line {n}"
            )))),
            EmitOutcome::Queued
        );
    }
    settle().await;
    rig.conductor_gate.open();
    rig.pump().await;

    assert!(rig.removed(1), "bot 1 is gone: {:?}", rig.sent);
    assert!(!rig.removed(2), "bot 2 is still leaving: {:?}", rig.sent);
    tokio::time::sleep(secs(60)).await;
    rig.pump().await;
    assert!(rig.removed(2), "its own Removed: {:?}", rig.sent);
    rig.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_apply_ends_the_reconciles_batch() {
    let mut rig = Rig::new();
    rig.hello().await;
    let reply_timeout = runtime().reply_timeout;

    // The supervisor answers nothing: the first apply times out, and the
    // rest wait for the tick instead of timing out one by one.
    rig.supervisor_gate.close();
    let started = tokio::time::Instant::now();
    rig.inbox
        .instruction(
            rig.generation(),
            Instruction::Reconcile((1..=3).map(|n| assign(spec(n), Some(paused()))).collect()),
        )
        .unwrap();
    // Behind the reconcile in the FIFO; its own snapshot times out too.
    let barrier = rig.inbox.barrier().await;
    let took = started.elapsed();
    rig.supervisor_gate.open();
    tokio::time::sleep(secs(3)).await;
    rig.pump().await;

    assert_eq!(
        barrier,
        Err(BarrierError::Fleet(FleetError::TimedOut)),
        "the snapshot timed out"
    );
    assert!(took < reply_timeout * 3, "{took:?}");
    assert_eq!(
        rig.running().await,
        [bot(1), bot(2), bot(3)],
        "on the ticks"
    );
    rig.stop().await;
}
