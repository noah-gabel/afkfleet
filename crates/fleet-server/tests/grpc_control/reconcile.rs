//! The reconciler over the real port (P10.5; ADR-0019, S32–S45): the test's
//! client is the agent, the database is real, and every assignment is
//! committed and audited before the agent hears of it.
//!
//! A check that a bot is *not* placed needs a pass to have run: such a test
//! adds a canary, a placeable bot with a higher ID, which the same pass
//! places. The fenced bot would have gone first.

use core::num::NonZeroU32;
use std::sync::Arc;

use chrono::DateTime;
use fleet_core::agent::AgentStatus;
use fleet_core::audit::AuditAction;
use fleet_core::bot::{
    BotAccount, BotState, DesiredRunState, LastState, Lifecycle, PauseReason, ShownState,
    StickyState, UnplacedWhy,
};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::BotId;
use fleet_core::mode::ModeDefinition;
use fleet_core::placement::Unplaced;
use fleet_core::system::Clock as _;
use fleet_proto::control::{Assignment, LifecycleCommand, ServerMessage};
use fleet_server::app::agents::AgentService;
use fleet_server::app::bots::{BotAction, BotService, Controlled};
use fleet_server::grpc::control::ONE_HELLO;
use fleet_server::grpc::registry::{AgentRegistry, RefusedReports};
use fleet_server::ports::live_agents::LiveAgents as _;
use fleet_server::ports::store::Store as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{CAPACITY, Settings, barrier, hello, removed, report, snapshot, start, text};

fn ids(assignments: &[Assignment]) -> Vec<BotId> {
    assignments
        .iter()
        .map(|assignment| assignment.spec.id)
        .collect()
}

fn online() -> BotState {
    BotState::Online {
        since: DateTime::from_timestamp_millis(1_790_000_000_000).unwrap(),
        attempt: NonZeroU32::MIN,
    }
}

fn paused() -> BotState {
    BotState::Paused {
        reason: PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        },
    }
}

fn sticky_paused() -> StickyState {
    StickyState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    })
}

#[sqlx::test(migrations = false)]
async fn every_welcome_gets_one_full_reconcile_even_an_empty_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;

    let (mut call, _) = port.welcomed(&agent).await;

    assert_eq!(call.reconciled().await, []);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_placeable_bot_reaches_its_agent_with_its_spec_once_its_assignment_is_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;

    let (mut call, _) = port.welcomed(&agent).await;
    let reconciled = call.reconciled().await;

    assert_eq!(ids(&reconciled), [bot]);
    let spec = &reconciled[0].spec;
    assert_eq!(
        spec.account,
        BotAccount::Offline("AfkBot1".parse().unwrap())
    );
    assert_eq!(spec.server, "play.example.com".parse().unwrap());
    assert_eq!(spec.mode, ModeDefinition::afk());
    assert_eq!(spec.desired, DesiredRunState::Running);
    assert_eq!(reconciled[0].restore, None);
    assert_eq!(port.bot_row(bot).await.unwrap().agent, Some(agent.id));
    let assigned = port.entries(&AuditAction::BOT_ASSIGN).await;
    assert_eq!(assigned.len(), 1, "{assigned:?}");
    assert_eq!(text(&assigned[0], "agent"), Some(agent.id.to_string()));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn bots_fill_the_agents_in_enrollment_order_up_to_their_capacity(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let first = port.enroll(1, "agent-1").await;
    let second = port.enroll(2, "agent-2").await;
    let bots = [port.bot(1).await, port.bot(2).await, port.bot(3).await];
    // A new server process, whose first pass finds both agents enabled.
    let port = port.restart().await;
    let mut one = port.welcomed_with(&first, 1, Vec::new()).await;
    // The gate waits for agent-2 (S45).
    assert_eq!(one.reconciled().await, []);

    let mut two = port.welcomed_with(&second, 1, Vec::new()).await;

    assert_eq!(one.assigned().await.spec.id, bots[0]);
    assert_eq!(ids(&two.reconciled().await), [bots[1]]);
    assert_eq!(port.bot_row(bots[2]).await.unwrap().agent, None);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_hellos_bot_it_wasnt_given_is_left_out_and_fenced_past_its_stream_and_a_restart(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::ungated()).await;
    let holder = port.enroll(1, "agent-1").await;
    let other = port.enroll(2, "agent-2").await;
    let bot = port.bot(1).await;
    // agent-1 runs the bot without the server's word: a restored backup.
    let mut stray = port
        .welcomed_with(&holder, CAPACITY, vec![snapshot(bot, online())])
        .await;
    assert_eq!(stray.reconciled().await, []);
    let strays = port.entries(&AuditAction::BOT_STRAY).await;
    assert_eq!(strays.len(), 1, "{strays:?}");
    assert_eq!(text(&strays[0], "agent"), Some(holder.id.to_string()));

    // Its reports of the bot are dropped.
    stray.send(report(bot, online())).await;
    stray.send(hello("agent-1")).await;
    assert_eq!(stray.ended().await.message(), ONE_HELLO);
    port.disconnect(&holder).await;
    assert_eq!(
        port.registry.take_refused_reports(),
        [RefusedReports {
            agent: holder.id,
            name: "agent-1".try_into().unwrap(),
            count: 1,
            sessions_denied: 0,
            chat_dropped: 0,
        }]
    );
    // Its stream is gone; the record still fences the bot.
    let (mut two, _) = port.welcomed(&other).await;
    let canary = port.bot(2).await;
    port.changed();
    let given = ids(&two.reconciled().await);
    let given = if given.is_empty() {
        vec![two.assigned().await.spec.id]
    } else {
        given
    };
    assert_eq!(given, [canary]);
    assert_eq!(port.bot_row(bot).await.unwrap().agent, None);

    // A new server process: the record still fences it.
    drop(two);
    let port = port.restart().await;
    let mut two = port
        .welcomed_with(&other, CAPACITY, vec![snapshot(canary, online())])
        .await;
    assert_eq!(ids(&two.reconciled().await), [canary]);
    assert_eq!(port.bot_row(bot).await.unwrap().agent, None);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_removed_of_a_bot_the_server_never_released_frees_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(ids(&call.reconciled().await), [bot]);

    call.send(removed(bot)).await;
    call.send(barrier()).await;

    assert!(
        matches!(call.next().await, Ok(Some(ServerMessage::SessionDenied(_)))),
        "the Removed was handled before"
    );
    assert_eq!(port.bot_row(bot).await.unwrap().agent, Some(agent.id));
    assert!(port.registry.held(agent.id, bot).is_some());
    assert_eq!(port.entries(&AuditAction::BOT_DETACH).await, []);
    assert_eq!(port.registry.take_refused_reports().len(), 1);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_removed_from_an_older_stream_never_clears_a_bot_its_newer_stream_was_given(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::ungated()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut old, _) = port.welcomed(&agent).await;
    assert_eq!(ids(&old.reconciled().await), [bot]);
    // The bot is stopped and let go: held releasing on the old stream.
    port.set_running(bot, false).await;
    assert!(matches!(
        old.next().await,
        Ok(Some(ServerMessage::Update(_)))
    ));
    old.send(report(bot, BotState::Stopped)).await;
    assert!(matches!(
        old.next().await,
        Ok(Some(ServerMessage::Unassign(id))) if id == bot
    ));
    // It qualifies again, and its Removed's clearing stays busy until the
    // agent's new stream has been given its bots.
    port.fail_detaches(u32::MAX);
    port.set_running(bot, true).await;
    old.send(removed(bot)).await;
    old.send(barrier()).await;
    assert!(matches!(
        old.next().await,
        Ok(Some(ServerMessage::SessionDenied(_)))
    ));

    // The agent reconnects without the bot: its row still names it, so its
    // new stream is given the bot back.
    old.close();
    port.disconnect(&agent).await;
    let (mut new, _) = port.welcomed(&agent).await;
    let given = ids(&new.reconciled().await);
    port.fail_detaches(0);
    // A pass after it: a canary it places shows it ran.
    let canary = port.bot(2).await;
    port.changed();
    let placed = new.assigned().await.spec.id;

    assert_eq!(given, [bot]);
    assert_eq!(placed, canary);
    assert_eq!(
        port.bot_row(bot).await.unwrap().agent,
        Some(agent.id),
        "the old stream's note is stale"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_agent_back_with_less_room_hands_its_paused_bot_on_with_its_restore(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let first = port.enroll(1, "agent-1").await;
    let second = port.enroll(2, "agent-2").await;
    let paused_bot = port.bot(1).await;
    let running_bot = port.bot(2).await;
    port.assign(paused_bot, &first).await;
    port.assign(running_bot, &first).await;
    let mut two = port.welcomed_with(&second, CAPACITY, Vec::new()).await;
    assert_eq!(two.reconciled().await, []);

    // agent-1 comes back with room for one: the bot it runs stays, the
    // paused one is let go and waits for its Removed.
    let mut one = port
        .welcomed_with(
            &first,
            1,
            vec![
                snapshot(paused_bot, paused()),
                snapshot(running_bot, online()),
            ],
        )
        .await;
    assert_eq!(ids(&one.reconciled().await), [running_bot]);
    one.send(removed(paused_bot)).await;

    let moved = two.assigned().await;
    assert_eq!(moved.spec.id, paused_bot);
    assert_eq!(moved.restore, Some(sticky_paused()));
    let unassigned = port.entries(&AuditAction::BOT_UNASSIGN).await;
    assert_eq!(text(&unassigned[0], "reason").as_deref(), Some("capacity"));
    let detached = port.entries(&AuditAction::BOT_DETACH).await;
    assert_eq!(text(&detached[0], "reason").as_deref(), Some("removed"));
    assert_eq!(
        port.bot_row(paused_bot).await.unwrap().agent,
        Some(second.id)
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_bot_whose_agent_went_quiet_reads_unknown_and_stays_assigned(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(ids(&call.reconciled().await), [bot]);
    call.send(report(bot, online())).await;

    call.close();
    port.disconnect(&agent).await;
    let view = BotService::new(
        Arc::new(port.clock.clone()),
        Arc::new(port.db.clone()),
        Arc::new(port.registry.clone()),
        true,
    )
    .view(
        &fleet_core::authz::Actor {
            id: fleet_core::id::UserId::new_v7(
                chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
                [1; 10],
            )
            .unwrap(),
            role: fleet_core::authz::Role::Owner,
        },
        bot,
    )
    .await
    .unwrap()
    .unwrap();

    let ShownState::Unknown {
        agent: status,
        agent_last_seen,
        last_known,
    } = view.shown
    else {
        panic!("{:?}", view.shown);
    };
    assert_eq!(status, AgentStatus::Offline);
    assert!(agent_last_seen.is_some());
    assert_eq!(last_known.map(|last| last.state), Some(LastState::Online));
    assert_eq!(port.bot_row(bot).await.unwrap().agent, Some(agent.id));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_new_server_process_gives_an_agent_restarted_empty_its_paused_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(ids(&call.reconciled().await), [bot]);
    call.send(report(bot, paused())).await;
    call.close();
    port.disconnect(&agent).await;

    let port = port.restart().await;
    let (mut call, _) = port.welcomed(&agent).await;
    let reconciled = call.reconciled().await;

    assert_eq!(ids(&reconciled), [bot]);
    assert_eq!(reconciled[0].restore, Some(sticky_paused()));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_offline_bot_is_never_placed_while_offline_accounts_are_off(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        Settings {
            allow_offline: false,
            ..Settings::reconciling()
        },
    )
    .await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;

    let (mut call, _) = port.welcomed(&agent).await;

    assert_eq!(call.reconciled().await, []);
    assert_eq!(port.bot_row(bot).await.unwrap().agent, None);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_relinked_account_waits_for_the_deleted_ones_bot_to_be_removed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (account, old) = port.bot_named(1, "AfkBot1").await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(ids(&call.reconciled().await), [old]);

    // The account is deleted, as its service does, and linked again.
    let mut tx = port.db.write().await.unwrap();
    assert!(
        tx.accounts()
            .delete(account, port.clock.now())
            .await
            .unwrap()
            .is_some()
    );
    tx.commit(fleet_server::ports::audit::NewAuditEntry::new(
        port.clock.now(),
        AuditAction::try_from("test.control_setup").unwrap(),
        fleet_core::audit::AuditOutcome::Success,
    ))
    .await
    .unwrap();
    port.changed();
    assert!(matches!(
        call.next().await,
        Ok(Some(ServerMessage::Unassign(id))) if id == old
    ));
    let (_, relinked) = port.bot_named(2, "afkbot1").await;
    let canary = port.bot(3).await;
    port.changed();

    assert_eq!(
        call.assigned().await.spec.id,
        canary,
        "not the relinked one"
    );
    call.send(removed(old)).await;
    assert_eq!(call.assigned().await.spec.id, relinked);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_disabled_agents_released_bot_goes_to_the_next_agent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        Settings {
            sweep: core::time::Duration::from_millis(100),
            ..Settings::reconciling()
        },
    )
    .await;
    let first = port.enroll(1, "agent-1").await;
    let second = port.enroll(2, "agent-2").await;
    let bot = port.bot(1).await;
    port.assign(bot, &first).await;
    let mut one = port
        .welcomed_with(&first, CAPACITY, vec![snapshot(bot, online())])
        .await;
    let (mut two, _) = port.welcomed(&second).await;
    assert_eq!(ids(&one.reconciled().await), [bot]);
    assert_eq!(two.reconciled().await, []);

    port.host_disable(&first).await;
    one.rejected().await;
    port.disconnect(&first).await;
    // Disabling moves nothing (S32).
    assert_eq!(port.bot_row(bot).await.unwrap().agent, Some(first.id));
    let clock = Arc::new(port.clock.clone());
    let released = AgentService::new(
        clock.clone(),
        Arc::new(port.db.clone()),
        Arc::new(AgentRegistry::new(clock)),
    )
    .release(first.id)
    .await
    .unwrap();
    port.changed();

    assert_eq!(released, [bot]);
    assert_eq!(two.assigned().await.spec.id, bot);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_bot_started_through_its_service_is_placed_and_once_stopped_is_taken_off_its_agent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    port.set_running(bot, false).await;
    let by = port.acting(1).await;
    let bots = port.bots();
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await, []);

    let started = bots.control(&by, bot, BotAction::Start).await.unwrap();
    let placed = call.assigned().await;
    assert_eq!(started, Controlled::Changed);
    assert_eq!(placed.spec.id, bot);
    assert_eq!(placed.spec.desired, DesiredRunState::Running);
    assert_eq!(port.bot_row(bot).await.unwrap().agent, Some(agent.id));

    call.send(report(bot, online())).await;
    let stopped = bots.control(&by, bot, BotAction::Stop).await.unwrap();
    let update = call.next().await;
    call.send(report(bot, BotState::Stopped)).await;
    let unassigned = call.next().await;
    call.send(removed(bot)).await;
    // The reconciler clears it on its own task.
    let detached = port.entries_once(&AuditAction::BOT_DETACH, 1).await;

    assert_eq!(stopped, Controlled::Changed);
    assert!(
        matches!(&update, Ok(Some(ServerMessage::Update(spec)))
            if spec.id == bot && spec.desired == DesiredRunState::Stopped),
        "{update:?}"
    );
    assert!(
        matches!(unassigned, Ok(Some(ServerMessage::Unassign(id))) if id == bot),
        "{unassigned:?}"
    );
    assert_eq!(text(&detached[0], "reason").as_deref(), Some("removed"));
    assert_eq!(port.bot_row(bot).await.unwrap().agent, None);
    assert_eq!(port.entries(&AuditAction::BOT_START).await.len(), 1);
    assert_eq!(port.entries(&AuditAction::BOT_STOP).await.len(), 1);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn stopping_a_paused_bot_frees_its_agents_slot_and_keeps_it_paused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let paused_bot = port.bot(1).await;
    let waiting = port.bot(2).await;
    port.assign(paused_bot, &agent).await;
    let by = port.acting(1).await;
    let bots = port.bots();
    // Room for one: the paused bot keeps it, the other bot waits.
    let mut call = port
        .welcomed_with(&agent, 1, vec![snapshot(paused_bot, paused())])
        .await;
    assert_eq!(ids(&call.reconciled().await), [paused_bot]);

    let stopped = bots
        .control(&by, paused_bot, BotAction::Stop)
        .await
        .unwrap();
    let unassigned = call.next().await;
    call.send(removed(paused_bot)).await;
    let next = call.assigned().await;
    let view = bots.view(&by.actor, paused_bot).await.unwrap().unwrap();

    assert_eq!(stopped, Controlled::Changed);
    assert!(
        matches!(unassigned, Ok(Some(ServerMessage::Unassign(id))) if id == paused_bot),
        "{unassigned:?}"
    );
    assert_eq!(next.spec.id, waiting);
    assert_eq!(view.record.bot.desired, DesiredRunState::Stopped);
    assert_eq!(view.record.bot.agent, None);
    assert_eq!(
        view.shown,
        ShownState::Unplaced {
            why: UnplacedWhy::Unqualified(Unplaced::Stopped),
            sticky: Some(sticky_paused()),
        }
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_lifecycle_command_reaches_the_stream_that_holds_the_bot_once_and_never_a_later_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let by = port.acting(1).await;
    let bots = port.bots();
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(ids(&call.reconciled().await), [bot]);
    call.send(report(bot, paused())).await;
    // Its answer means the report was handled.
    call.send(barrier()).await;
    assert!(matches!(
        call.next().await,
        Ok(Some(ServerMessage::SessionDenied(_)))
    ));

    let resumed = bots
        .control(&by, bot, BotAction::Command(Lifecycle::Resume))
        .await
        .unwrap();
    let command = call.next().await;
    // The agent comes back on a new stream, still paused.
    call.close();
    port.disconnect(&agent).await;
    let mut again = port
        .welcomed_with(&agent, CAPACITY, vec![snapshot(bot, paused())])
        .await;
    let given = ids(&again.reconciled().await);
    again.send(barrier()).await;
    let after = again.next().await;

    assert_eq!(resumed, Controlled::Sent);
    assert!(
        matches!(command, Ok(Some(ServerMessage::Lifecycle(LifecycleCommand {
            bot_id,
            command: Lifecycle::Resume,
        }))) if bot_id == bot),
        "{command:?}"
    );
    assert_eq!(given, [bot]);
    assert!(
        matches!(after, Ok(Some(ServerMessage::SessionDenied(_)))),
        "not replayed: {after:?}"
    );
    let entries = port.entries(&AuditAction::BOT_RESUME).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(text(&entries[0], "effect").as_deref(), Some("sent"));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn starting_a_bot_whose_account_a_release_record_fences_places_it_nowhere(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::ungated()).await;
    let holder = port.enroll(1, "agent-1").await;
    let other = port.enroll(2, "agent-2").await;
    let bot = port.bot(1).await;
    port.set_running(bot, false).await;
    let by = port.acting(1).await;
    let bots = port.bots();
    // agent-1 runs the bot without the server's word: a stray, whose
    // record fences it past its stream.
    let mut stray = port
        .welcomed_with(&holder, CAPACITY, vec![snapshot(bot, online())])
        .await;
    assert_eq!(stray.reconciled().await, []);
    stray.close();
    port.disconnect(&holder).await;
    let (mut call, _) = port.welcomed(&other).await;
    assert_eq!(call.reconciled().await, []);
    let canary = port.bot(2).await;

    let started = bots.control(&by, bot, BotAction::Start).await.unwrap();
    let placed = call.assigned().await;

    assert_eq!(started, Controlled::Changed);
    assert_eq!(placed.spec.id, canary);
    let row = port.bot_row(bot).await.unwrap();
    assert_eq!(row.agent, None);
    assert_eq!(row.desired, DesiredRunState::Running);
    port.stop().await;
}
