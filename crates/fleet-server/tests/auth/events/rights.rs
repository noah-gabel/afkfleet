//! The rights a socket judges its events on (ADR-0020, S30, S41, R18,
//! R30–R32): one shared snapshot, read again only for a commit count it
//! predates, by one read at a time, tagged with the count from before its
//! read; and on a socket, a change of rights holds at once, for every frame
//! not yet sent, the ones already waiting among them.

use core::time::Duration;

use fleet_api_types::events::SocketClose;
use fleet_core::authz::Role;
use fleet_core::system::Clock as _;
use fleet_server::app::events::LiveEventKind;
use fleet_server::http::ws::SocketTimings;
use fleet_server::ports::live_hints::LiveHints as _;
use fleet_server::ports::store::{Store as _, StoreError};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::{Options, setup, setup_entry, setup_with};
use super::bots::{agent_event, watched};
use super::{
    PATIENCE, Piped, Served, closed, commit_anything, next_event, next_event_after_pings, open,
    ready, said, ticket,
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

/// A frame built before a revoke may still go out; none taken from the bus
/// before it is sent after it.
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
        next_event_after_pings(&mut socket).await["type"],
        json!("bot"),
        "read before the revoke"
    );
    assert_eq!(
        next_event_after_pings(&mut socket).await,
        removed(watched.bot)
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
    // reads, and the second bot's grant is revoked meanwhile.
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[watched.bot, second]);
    held.read(PATIENCE).await;
    held.release();
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
