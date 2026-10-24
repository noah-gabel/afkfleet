//! What the live events cost (ADR-0020, S41): a socket reads its merged
//! hints in one batched read, so 200 bots are one or two reads, never 200;
//! and with 200 bots, 20 users and 400 grants, a rights rebuild and one
//! viewer's read of all 200 bots stay within loose bounds. The measured
//! times print for P13.4's load test (`--no-capture`).

use std::sync::Arc;

use fleet_core::authz::{Actor, GrantLevel, Role};
use fleet_core::id::{BotId, UserId};
use fleet_server::app::bots::BotService;
use fleet_server::ports::live_hints::LiveHints as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::time::Instant;

use super::super::support::{Setup, setup};
use super::{PATIENCE, Served, next_event_after_pings, ready};

/// How many bots, users and grants per account the measured fleet has.
const BOTS: u8 = 200;
const USERS: u8 = 20;

/// Twenty Members, user `201 + i` owning bots `10 i .. 10 i + 10`, each
/// account shared with the next two users, at View and at Control: 200
/// bots, 400 grants. The users, and the bots in ID order.
async fn fleet(setup: &Setup) -> (Vec<UserId>, Vec<BotId>) {
    let mut users = Vec::new();
    for i in 0..USERS {
        users.push(
            setup
                .add_user(201 + i, &format!("costuser{i}"), Role::Member)
                .await,
        );
    }
    let mut bots = Vec::new();
    for n in 0..BOTS {
        let owner = usize::from(n / 10);
        let (account, bot) = setup.bot_of(users[owner], n).await;
        for (step, level) in [(1, GrantLevel::View), (2, GrantLevel::Control)] {
            let grantee = users[(owner + step) % users.len()];
            setup.share(account, grantee, level, users[owner]).await;
        }
        bots.push(bot);
    }
    bots.sort();
    (users, bots)
}

#[sqlx::test(migrations = false)]
async fn a_merge_of_200_bots_makes_at_most_2_reads(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let mut bots = Vec::new();
    for n in 0..BOTS {
        bots.push(setup.bot_of(member.id, n).await.1);
    }
    bots.sort();
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &member.token).await;
    let reads = setup.hooked.bot_reads();
    let (first, second) = bots.split_at(100);

    // The first half's read is held; the second half, and the first again,
    // wait on the bus meanwhile, one hint each.
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(first);
    held.read(PATIENCE).await;
    setup.events.bots(second);
    setup.events.bots(first);
    held.release();
    let mut frames = Vec::new();
    for _ in 0..300 {
        frames.push(next_event_after_pings(&mut socket).await);
    }

    assert!(frames.iter().all(|frame| frame["type"] == json!("bot")));
    let named: Vec<String> = frames
        .iter()
        .map(|frame| frame["bot_id"].as_str().unwrap().to_owned())
        .collect();
    let ids: Vec<String> = bots.iter().map(ToString::to_string).collect();
    assert_eq!(named.get(..100), ids.get(..100), "the held half first");
    assert_eq!(named.get(100..), Some(ids.as_slice()), "then all, merged");
    assert_eq!(
        setup.hooked.bot_reads() - reads,
        2,
        "the held read, then one for the 200 merged"
    );
}

/// The median and the maximum of `times`, sorted in place.
fn median_and_max(
    times: &mut [core::time::Duration],
) -> (core::time::Duration, core::time::Duration) {
    times.sort();
    let median = times.get(times.len() / 2).copied().unwrap_or_default();
    let max = times.last().copied().unwrap_or_default();
    (median, max)
}

/// Real time over a real database: the bounds hold the medians, about
/// twenty times what a debug build measured (2.3 ms and 5.5 ms), so only a
/// change of scale fails it, never one scheduler stall (CI's instrumented
/// coverage run has them); the maxima only print.
#[sqlx::test(migrations = false)]
async fn rebuild_and_merge_costs_stay_within_loose_bounds(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (users, bots) = fleet(&setup).await;
    let rights = setup.live_events.rights().clone();
    let service = BotService::new(
        Arc::new(setup.clock.clone()),
        Arc::new(setup.random.clone()),
        Arc::new(setup.db.clone()),
        Arc::new(setup.registry.clone()),
        setup.events.clone(),
        true,
    );

    let mut rebuilds = Vec::new();
    for _ in 0..20 {
        super::commit_anything(&setup).await;
        let started = Instant::now();
        let snapshot = rights.at_least(rights.count()).await.unwrap();
        rebuilds.push(started.elapsed());
        assert_eq!(
            snapshot
                .visible(&Actor {
                    id: users[0],
                    role: Role::Member
                })
                .len(),
            30
        );
    }
    let mut reads = Vec::new();
    for &id in &users {
        let viewer = Actor {
            id,
            role: Role::Member,
        };
        let started = Instant::now();
        let views = service.views(&viewer, &bots).await.unwrap();
        reads.push(started.elapsed());
        // Every row: the socket authorizes each, as `GET /bots/{id}` does.
        assert_eq!(views.len(), bots.len());
    }
    let all: core::time::Duration = reads.iter().sum();
    let (rebuild, rebuild_max) = median_and_max(&mut rebuilds);
    let (read, read_max) = median_and_max(&mut reads);
    println!(
        "S41 (200 bots, 20 users, 400 grants): rebuild median {rebuild:?}, max {rebuild_max:?}; \
         one viewer's read of 200 hinted bots median {read:?}, max {read_max:?}; \
         20 viewers {all:?}"
    );

    assert!(
        rebuild < core::time::Duration::from_millis(50),
        "the median {rebuild:?}"
    );
    assert!(
        read < core::time::Duration::from_millis(250),
        "the median {read:?}"
    );
}
