//! The rights a socket judges its events on (ADR-0020, S30, S41, R18,
//! R30–R32): one shared snapshot, read again only for a commit count it
//! predates, by one read at a time, tagged with the count from before its
//! read; and on a socket, a change of rights holds at once, for every frame
//! not yet sent, the ones already waiting among them.

use core::time::Duration;

use fleet_api_types::events::SocketClose;
use fleet_core::authz::{GrantLevel, Role};
use fleet_core::id::{AccountId, UserId};
use fleet_core::system::Clock as _;
use fleet_server::app::events::{LiveEventKind, LiveRights};
use fleet_server::http::ws::SocketTimings;
use fleet_server::ports::live_hints::LiveHints as _;
use fleet_server::ports::store::{Store as _, StoreError};
use fleet_testkit::log_capture;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_tungstenite::tungstenite::Message;
use tracing::Level;

use super::super::support::{LOGOUT, Options, Setup, setup, setup_entry, setup_with};
use super::bots::{agent_event, bot_frame, get_bot, watched};
use super::{
    PATIENCE, Piped, Served, closed, commit_anything, next, next_event, next_event_after_pings,
    open, ready, said, ticket,
};

#[sqlx::test(migrations = false)]
async fn a_snapshot_new_enough_is_shared_without_another_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    let count = rights.count();

    let first = rights.at_least(count).await.unwrap();
    let again = rights.at_least(count).await.unwrap();
    let older = rights.at_least(0).await.unwrap();

    assert_eq!(first.at(), count);
    assert_eq!((again.at(), older.at()), (count, count));
    assert_eq!(setup.hooked.rights_reads(), 1);
}

#[sqlx::test(migrations = false)]
async fn twenty_askers_after_one_commit_share_one_rebuild(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    rights.at_least(rights.count()).await.unwrap();
    commit_anything(&setup).await;
    let count = rights.count();
    let mut held = setup.hooked.hold_next_rights_read();

    let askers: Vec<_> = (0..20)
        .map(|_| {
            let rights = rights.clone();
            tokio::spawn(async move { rights.at_least(count).await.unwrap().at() })
        })
        .collect();
    held.read(PATIENCE).await;
    held.release();
    let mut seen = Vec::new();
    for asker in askers {
        seen.push(
            tokio::time::timeout(PATIENCE, asker)
                .await
                .unwrap()
                .unwrap(),
        );
    }

    assert!(seen.iter().all(|at| *at >= count), "{seen:?}");
    assert_eq!(
        setup.hooked.rights_reads(),
        2,
        "one at the start, one after"
    );
}

#[sqlx::test(migrations = false)]
async fn a_snapshot_is_tagged_with_the_count_from_before_its_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    let before = rights.count();
    let mut held = setup.hooked.hold_next_rights_read();
    let reader = rights.clone();
    let reading = tokio::spawn(async move { reader.at_least(before).await.unwrap() });

    // The read has read the database; a commit lands after it.
    held.read(PATIENCE).await;
    commit_anything(&setup).await;
    held.release();
    let snapshot = tokio::time::timeout(PATIENCE, reading)
        .await
        .unwrap()
        .unwrap();
    let after = rights.count();
    let fresh = rights.at_least(after).await.unwrap();

    assert_eq!(snapshot.at(), before);
    assert!(after > before);
    assert!(fresh.at() >= after);
    assert_eq!(
        setup.hooked.rights_reads(),
        2,
        "the commit made it read again"
    );
}

#[sqlx::test(migrations = false)]
async fn a_read_that_fails_fails_the_ask_and_keeps_no_snapshot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    rights.at_least(rights.count()).await.unwrap();
    commit_anything(&setup).await;
    setup.hooked.fail_rights_reads();

    let failed = rights.at_least(rights.count()).await;
    let again = rights.at_least(rights.count()).await;

    assert!(matches!(failed, Err(StoreError::Busy)), "{failed:?}");
    assert!(matches!(again, Err(StoreError::Busy)), "{again:?}");
    assert_eq!(setup.hooked.rights_reads(), 3);
}

// --- On a socket ---------------------------------------------------------

/// The frame that says `bot` is no longer seen.
fn removed(bot: fleet_core::id::BotId) -> serde_json::Value {
    json!({ "type": "bot_removed", "bot_id": bot.to_string() })
}

#[sqlx::test(migrations = false)]
async fn a_revoked_grant_turns_the_next_event_into_bot_removed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (_, marker) = setup.bot_of(watched.controller.id, 30).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.controller.token).await;

    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.grants()
            .delete(watched.account, watched.controller.id)
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);
    setup.events.bots(&[marker]);

    assert_eq!(
        next_event_after_pings(&mut socket).await,
        removed(watched.bot)
    );
    assert_eq!(
        next_event_after_pings(&mut socket).await["bot_id"],
        json!(marker.to_string()),
        "no death of the bot came"
    );
}

#[sqlx::test(migrations = false)]
async fn a_deleted_account_turns_its_bot_into_bot_removed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (_, marker) = setup.bot_of(watched.viewer.id, 30).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.viewer.token).await;

    let mut tx = setup.db.write().await.unwrap();
    tx.accounts()
        .delete(watched.account, setup.clock.now())
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);
    setup.events.bots(&[watched.bot, marker]);

    assert_eq!(
        next_event_after_pings(&mut socket).await,
        removed(watched.bot)
    );
    assert_eq!(
        next_event_after_pings(&mut socket).await["bot_id"],
        json!(marker.to_string()),
        "nothing more of the gone bot came"
    );
}

/// A bot read made before a revoke and answering after it is made again
/// (S43): nothing taken from the bus before the revoke is sent after it.
#[sqlx::test(migrations = false)]
async fn a_frame_queued_before_a_revoke_never_reaches_its_viewer(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (_, marker) = setup.bot_of(watched.controller.id, 30).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.controller.token).await;

    // The bot's read is held while three deaths queue behind it; then the
    // grant is revoked, and the read lets go.
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[watched.bot]);
    held.read(PATIENCE).await;
    for _ in 0..3 {
        agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);
    }
    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.grants()
            .delete(watched.account, watched.controller.id)
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    setup.events.bots(&[marker]);
    held.release();

    assert_eq!(
        next_event_after_pings(&mut socket).await,
        removed(watched.bot),
        "read before the revoke, and read again after it (S43)"
    );
    assert_eq!(
        next_event_after_pings(&mut socket).await["bot_id"],
        json!(marker.to_string()),
        "no death waiting before the revoke came"
    );
}

#[sqlx::test(migrations = false)]
async fn a_lowered_role_mutes_the_socket_until_its_4001(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // A long gap between two session checks: the role change's check waits
    // for it, while the death is sent or not.
    let setup = setup_with(
        options,
        Options {
            sockets: SocketTimings {
                check_gap: Duration::from_secs(2),
                ..SocketTimings::default()
            },
            ..Options::default()
        },
    )
    .await;
    let watched = watched(&setup).await;
    // A grant of their own: lowered, the Admin still sees the bot by the
    // rule, so only the mute keeps its death from them.
    setup
        .share(
            watched.account,
            watched.admin.id,
            fleet_core::authz::GrantLevel::View,
            watched.member.id,
        )
        .await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.admin.token).await;
    // The connect's check, then the socket's own after a commit: the next
    // one waits for the gap.
    commit_anything(&setup).await;
    setup.hooked.until_socket_checks(2, PATIENCE).await;

    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.users()
            .set_role(watched.admin.id, Role::Member)
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);

    // Nothing but pings before the close.
    assert_eq!(
        closed(&mut socket).await,
        said(SocketClose::Unauthenticated)
    );
}

#[sqlx::test(migrations = false)]
async fn a_rights_read_that_fails_at_the_open_closes_with_1013_before_ready(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    setup.hooked.fail_rights_reads();

    let ticket = ticket(&setup, &watched.member.token).await;
    let mut socket = open(&served, &watched.member.token, &ticket).await.unwrap();

    assert_eq!(closed(&mut socket).await, said(SocketClose::Busy));
}

#[sqlx::test(migrations = false)]
async fn a_rights_rebuild_that_fails_closes_the_socket_with_1013(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.member.token).await;

    setup.hooked.fail_rights_reads();
    commit_anything(&setup).await;
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);

    assert_eq!(closed(&mut socket).await, said(SocketClose::Busy));
}

/// Two bots read together; a revoke lands while the first one's frame is
/// on its way, so the second is read again, never sent from the old read.
#[sqlx::test(migrations = false)]
async fn a_read_made_before_a_commit_is_read_again_for_the_bots_not_yet_sent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (second_account, second) = setup.bot_of(watched.member.id, 11).await;
    setup
        .share(
            second_account,
            watched.controller.id,
            fleet_core::authz::GrantLevel::Control,
            watched.member.id,
        )
        .await;
    let (_, marker) = setup.bot_of(watched.controller.id, 30).await;
    let piped = Piped::start(&setup);
    let ticket = ticket(&setup, &watched.controller.token).await;
    let mut socket = piped.open(&watched.controller.token, &ticket).await;
    assert_eq!(next_event(&mut socket).await, json!({ "type": "ready" }));

    // Read together; the first frame is stuck in the pipe until the client
    // reads, and the second bot's grant is revoked meanwhile: once the
    // server writes it, the step that read both is over.
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[watched.bot, second]);
    held.read(PATIENCE).await;
    held.release();
    socket.get_mut().written().await;
    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.grants()
            .delete(second_account, watched.controller.id)
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    setup.events.bots(&[marker]);

    assert_eq!(
        next_event(&mut socket).await["bot_id"],
        json!(watched.bot.to_string())
    );
    assert_eq!(next_event(&mut socket).await, removed(second));
    assert_eq!(
        next_event(&mut socket).await["bot_id"],
        json!(marker.to_string()),
        "the second bot came only as removed"
    );
}

// --- Commits landing during a step's reads (S43) ------------------------

/// Revokes `user`'s grant on `account`.
async fn revoked(setup: &Setup, account: AccountId, user: UserId) {
    let mut tx = setup.db.write().await.unwrap();
    assert!(tx.grants().delete(account, user).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
}

/// A revoke committed while the death's rights read is held, after that
/// read: the snapshot it gives is older than the count, so the death waits
/// for the next read, which sees the revoke.
#[sqlx::test(migrations = false)]
async fn a_revoke_committed_while_a_rebuild_is_held_drops_the_event(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (_, marker) = setup.bot_of(watched.controller.id, 30).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.controller.token).await;
    // So the death's step reads the rights again.
    commit_anything(&setup).await;
    let reads = setup.hooked.rights_reads();

    let mut held = setup.hooked.hold_next_rights_read();
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);
    held.read(PATIENCE).await;
    revoked(&setup, watched.account, watched.controller.id).await;
    setup.events.bots(&[marker]);
    held.release();

    assert_eq!(
        next_event_after_pings(&mut socket).await,
        removed(watched.bot)
    );
    assert_eq!(
        next_event_after_pings(&mut socket).await["bot_id"],
        json!(marker.to_string()),
        "no death came"
    );
    assert_eq!(
        setup.hooked.rights_reads() - reads,
        2,
        "the held read, and one after the revoke"
    );
}

/// Three commits, each landing during a rights read: the death waits out
/// each, and goes once, on the first read no commit outran.
#[sqlx::test(migrations = false)]
async fn a_death_waits_out_commits_during_its_rights_reads_and_goes_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (_, marker) = setup.bot_of(watched.controller.id, 30).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.controller.token).await;
    commit_anything(&setup).await;
    let reads = setup.hooked.rights_reads();

    setup.hooked.commit_during_next_rights_reads(3);
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);
    setup.events.bots(&[marker]);
    let died = next_event_after_pings(&mut socket).await;
    let after = next_event_after_pings(&mut socket).await;

    assert_eq!(
        (died["type"].clone(), died["bot_id"].clone()),
        (json!("bot_died"), json!(watched.bot.to_string()))
    );
    assert_eq!(after["bot_id"], json!(marker.to_string()));
    assert_eq!(
        setup.hooked.rights_reads() - reads,
        4,
        "three outrun, then one that held"
    );
}

/// Commits landing during every rights read, without end: the death waits,
/// while the socket keeps pinging and checking its session, so a logout
/// still closes it with 4001; never 1013.
#[sqlx::test(migrations = false)]
async fn an_endless_storm_of_commits_holds_the_frames_but_not_the_pings_or_the_session_check(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            sockets: SocketTimings {
                ping_interval: Duration::from_millis(100),
                ..SocketTimings::default()
            },
            ..Options::default()
        },
    )
    .await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.controller.token).await;
    let reads = setup.hooked.rights_reads();

    setup.hooked.commit_during_next_rights_reads(u32::MAX);
    commit_anything(&setup).await;
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);
    let mut pings = 0;
    while pings < 3 {
        match next(&mut socket).await {
            Some(Message::Ping(_)) => pings += 1,
            other => panic!("only pings while the storm lasts: {other:?}"),
        }
    }
    let out = setup
        .post(LOGOUT, Some(&watched.controller.token), None)
        .await;

    assert_eq!(out.status, 204, "{}", out.text);
    assert_eq!(
        closed(&mut socket).await,
        said(SocketClose::Unauthenticated)
    );
    assert!(setup.hooked.rights_reads() - reads > 3, "the storm ran");
}

/// Three bots leave in one read; the last is shared again while the
/// second's removal is on its way: it comes back as `bot`, and its own
/// removal, still waiting, never goes.
#[sqlx::test(migrations = false)]
async fn a_bot_that_rejoins_before_its_removal_went_out_is_sent_as_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let mut shared = vec![(watched.account, watched.bot)];
    for n in [11, 12] {
        let (account, bot) = setup.bot_of(watched.member.id, n).await;
        setup
            .share(
                account,
                watched.controller.id,
                GrantLevel::Control,
                watched.member.id,
            )
            .await;
        shared.push((account, bot));
    }
    shared.sort_by_key(|(_, bot)| *bot);
    let (_, marker) = setup.bot_of(watched.controller.id, 30).await;
    let piped = Piped::start(&setup);
    let ticket = ticket(&setup, &watched.controller.token).await;
    let mut socket = piped.open(&watched.controller.token, &ticket).await;
    assert_eq!(next_event(&mut socket).await, json!({ "type": "ready" }));

    let mut tx = setup.db.write().await.unwrap();
    for (account, _) in &shared {
        assert!(
            tx.grants()
                .delete(*account, watched.controller.id)
                .await
                .unwrap()
        );
    }
    tx.commit(setup_entry()).await.unwrap();
    setup.events.bots(&[marker]);
    let first = next_event(&mut socket).await;
    // The second's removal can't leave the pipe until it's read: the third
    // can't have gone yet when it's shared again.
    let (last_account, last) = shared[2];
    setup
        .share(
            last_account,
            watched.controller.id,
            GrantLevel::Control,
            watched.member.id,
        )
        .await;
    let mut rest: Vec<serde_json::Value> = Vec::new();
    while rest
        .last()
        .is_none_or(|frame| frame["bot_id"] != json!(marker.to_string()))
    {
        rest.push(next_event(&mut socket).await);
    }

    assert_eq!(first, removed(shared[0].1));
    assert_eq!(rest.len(), 3, "{rest:?}");
    assert!(rest.contains(&removed(shared[1].1)), "{rest:?}");
    assert!(
        rest.iter().any(
            |frame| frame["type"] == json!("bot") && frame["bot_id"] == json!(last.to_string())
        ),
        "{rest:?}"
    );
}

// --- A bot the held rights don't show yet (PR #67's review) -------------

/// A View grant given to the stranger while their socket is idle, and the
/// grant service's hint: their socket reads the rights again, and sends the
/// bot as their `GET /bots/{id}` answers it.
#[sqlx::test(migrations = false)]
async fn a_grantees_idle_socket_gets_the_newly_shared_bot_as_their_get(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.stranger.token).await;

    setup
        .share(
            watched.account,
            watched.stranger.id,
            GrantLevel::View,
            watched.member.id,
        )
        .await;
    setup.events.bots(&[watched.bot]);

    assert_eq!(
        next_event_after_pings(&mut socket).await,
        bot_frame(
            watched.bot,
            &get_bot(&setup, watched.bot, &watched.stranger).await
        )
    );
}

/// A Member's new offline account, and the accounts service's hint: the
/// Owner's and an Admin's idle sockets each send its bot as their `GET`.
#[sqlx::test(migrations = false)]
async fn a_new_offline_account_reaches_the_owners_and_an_admins_idle_sockets(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let admin = setup.person(7, "admin", Role::Admin).await;
    let member = setup.person(2, "member", Role::Member).await;
    let served = Served::start(&setup).await;
    let mut sockets = Vec::new();
    for person in [&the_owner, &admin] {
        sockets.push(ready(&setup, &served, &person.token).await);
    }

    let (_, bot) = setup.bot_of(member.id, 10).await;
    setup.events.bots(&[bot]);

    for (person, socket) in [&the_owner, &admin].into_iter().zip(&mut sockets) {
        assert_eq!(
            next_event_after_pings(socket).await,
            bot_frame(bot, &get_bot(&setup, bot, person).await)
        );
    }
}

/// A bot shared with the stranger while their socket is idle, then a line
/// of its chat and that line's `chat_sent`, all before the socket woke:
/// the bot, then the line, then its `chat_sent`.
#[sqlx::test(migrations = false)]
async fn a_newly_shared_bots_frame_its_line_and_its_chat_sent_arrive_in_order(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.stranger.token).await;
    let request = fleet_core::id::ChatRequestId::new_v7(setup.clock.now(), [4; 10]).unwrap();
    let line = fleet_server::ports::chat::ListedChat {
        record: fleet_server::ports::chat::ChatRecord {
            id: fleet_server::ports::chat::ChatMessageId::new(1),
            bot: watched.bot,
            at: setup.clock.now(),
            entry: fleet_server::ports::chat::ChatEntry::Received(
                fleet_core::chat::IncomingChat::from_parts(
                    fleet_core::chat::ChatKind::System,
                    None,
                    "a line",
                    false,
                )
                .unwrap(),
            ),
        },
        sent_by: None,
    };

    setup
        .share(
            watched.account,
            watched.stranger.id,
            GrantLevel::Control,
            watched.member.id,
        )
        .await;
    setup.events.bots(&[watched.bot]);
    agent_event(
        &setup,
        watched.bot,
        watched.agent,
        LiveEventKind::Chat {
            line,
            request: None,
        },
    );
    agent_event(
        &setup,
        watched.bot,
        watched.agent,
        LiveEventKind::ChatSent(request),
    );
    let mut kinds = Vec::new();
    for _ in 0..3 {
        let frame = next_event_after_pings(&mut socket).await;
        assert_eq!(frame["bot_id"], json!(watched.bot.to_string()), "{frame}");
        kinds.push(frame["type"].clone());
    }

    assert_eq!(kinds, [json!("bot"), json!("chat"), json!("chat_sent")]);
}

/// A hidden bot's state, then a line and the state of a bot the Control
/// holder sees, all taken on rights a commit outran: the line comes before
/// the bot, as on current rights, so the hidden event left no trace in the
/// order of their frames (PR #67's review follow-up b, R47).
#[sqlx::test(migrations = false)]
async fn a_hidden_bots_event_taken_on_stale_rights_leaves_no_trace_in_the_order(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (_, marker) = setup.bot_of(watched.controller.id, 30).await;
    let piped = Piped::start(&setup);
    let ticket = ticket(&setup, &watched.controller.token).await;
    let mut socket = piped.open(&watched.controller.token, &ticket).await;
    assert_eq!(next_event(&mut socket).await, json!({ "type": "ready" }));
    let status = |bot| {
        LiveEventKind::Status(fleet_core::bot::BotSnapshot {
            bot_id: bot,
            state: fleet_core::bot::BotState::Stopped,
            since: setup.clock.now(),
            last_disconnect: None,
        })
    };
    let line = fleet_server::ports::chat::ListedChat {
        record: fleet_server::ports::chat::ChatRecord {
            id: fleet_server::ports::chat::ChatMessageId::new(1),
            bot: watched.bot,
            at: setup.clock.now(),
            entry: fleet_server::ports::chat::ChatEntry::Received(
                fleet_core::chat::IncomingChat::from_parts(
                    fleet_core::chat::ChatKind::System,
                    None,
                    "a line",
                    false,
                )
                .unwrap(),
            ),
        },
        sent_by: None,
    };

    // The marker's frame is on its way, so its step is over; a commit then
    // outruns the rights the socket holds, and the three events wait on the
    // bus until the client reads the marker.
    setup.events.bots(&[marker]);
    socket.get_mut().written().await;
    commit_anything(&setup).await;
    agent_event(&setup, watched.own, watched.agent, status(watched.own));
    agent_event(
        &setup,
        watched.bot,
        watched.agent,
        LiveEventKind::Chat {
            line,
            request: None,
        },
    );
    agent_event(&setup, watched.bot, watched.agent, status(watched.bot));
    let first = next_event(&mut socket).await;
    let mut kinds = Vec::new();
    for _ in 0..2 {
        let frame = next_event(&mut socket).await;
        assert_eq!(frame["bot_id"], json!(watched.bot.to_string()), "{frame}");
        kinds.push(frame["type"].clone());
    }

    assert_eq!(first["bot_id"], json!(marker.to_string()));
    assert_eq!(kinds, [json!("chat"), json!("bot")]);
}

// --- Chat formats that don't read back (ADR-0020, S81) ----------------------------------

/// Where the line comes from, and what it says.
const FORMATS_TARGET: &str = "fleet_server::app::server_settings";
const UNREADABLE_FORMATS: &str = "chat formats don't read back";

/// A commit, then the rights read again at its count.
async fn rebuild(setup: &Setup, rights: &LiveRights) {
    commit_anything(setup).await;
    rights.at_least(rights.count()).await.unwrap();
}

/// S81: the rights log a settings row whose formats don't read back once,
/// when it turns unreadable, the first rebuild included, never at every
/// rebuild, since every commit moves the count; fixed and broken again, it
/// logs once more.
#[sqlx::test(migrations = false)]
async fn an_unreadable_chat_formats_row_logs_once_when_it_turns_unreadable_not_per_rebuild(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    let settings = setup
        .formats_for(1, "mc.example.com", &["{sender} : "])
        .await;
    setup.break_formats(settings, r#"["{Sender} : "]"#).await;
    let rights = setup.live_events.rights().clone();

    rights.at_least(rights.count()).await.unwrap();
    for _ in 0..3 {
        rebuild(&setup, &rights).await;
    }
    let while_broken = log_capture::count(FORMATS_TARGET, UNREADABLE_FORMATS);
    setup.break_formats(settings, r#"["{sender} : "]"#).await;
    rebuild(&setup, &rights).await;
    let fixed = log_capture::count(FORMATS_TARGET, UNREADABLE_FORMATS);
    setup.break_formats(settings, r#"["{Sender} : "]"#).await;
    rebuild(&setup, &rights).await;
    rebuild(&setup, &rights).await;

    assert_eq!(while_broken, 1);
    assert_eq!(fixed, 1);
    assert_eq!(log_capture::count(FORMATS_TARGET, UNREADABLE_FORMATS), 2);
}

/// S81: the line names the row by its ID, its server when it reads, its
/// rowid and the column that doesn't, at `error`, and never holds a
/// format; a row whose server doesn't read back names no server.
#[sqlx::test(migrations = false)]
async fn an_unreadable_formats_line_names_its_row_and_never_a_format(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    let formats = setup
        .formats_for(1, "mc.example.com", &["{sender} : "])
        .await;
    let server = setup
        .formats_for(2, "other.example.com", &["{sender} : "])
        .await;
    setup
        .break_formats(formats, r#"["{Sender} canary-format : "]"#)
        .await;
    let id = server.to_string();
    sqlx::query!(
        "UPDATE server_settings SET server = 'other.example.com' WHERE id = ?",
        id
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();
    let rights = setup.live_events.rights().clone();

    rights.at_least(rights.count()).await.unwrap();

    let at_error = |needle: &str| log_capture::count_at(FORMATS_TARGET, Level::ERROR, needle);
    assert_eq!(at_error(UNREADABLE_FORMATS), 2);
    assert_eq!(at_error(&format!("server_settings_id={formats}")), 1);
    assert_eq!(at_error("server=mc.example.com:25565"), 1);
    assert_eq!(at_error("column=chat_formats"), 1);
    assert_eq!(at_error(&format!("server_settings_id={server}")), 1);
    assert_eq!(at_error("column=server"), 1);
    assert_eq!(at_error("server=other"), 0);
    assert_eq!(at_error("rowid="), 2);
    log_capture::check_absent("canary-format").unwrap();
}
