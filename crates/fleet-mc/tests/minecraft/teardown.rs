//! A torn-down session leaves nothing behind (Plan.md P3.8; ADR-0008 §10,
//! ADR-0011).
//!
//! Every cycle puts four bots online at once and ends them in each way a
//! session can end: two with the full teardown, one kicked by the server
//! before its teardown, and one whose handles are all dropped without a
//! teardown, as when its actor panics (P3.2). Afterwards:
//! - fleet-mc's own counts, its host threads and Worlds, are back to 0, and
//!   no host thread was abandoned
//! - on Linux, the process has at most as many OS threads as after the
//!   warm-up cycle
//!
//! fleet-mc counts a host thread as ended just before the OS thread exits, so
//! the baseline is taken only once no host thread is left in the OS's list.
//! Otherwise a thread still exiting would inflate it and hide a leak.
//!
//! The warm-up starts what stays for the life of the process: Bevy's task
//! pools and async-compat's runtime (ADR-0008 §10). Every check runs while
//! the server's container is still up: dropping it would start
//! testcontainers' cleanup thread.

use core::time::Duration;
use std::collections::BTreeMap;
use std::time::Instant;

use fleet_core::disconnect::DisconnectReason;
use fleet_core::mc::{SessionEvent, SessionEvents, SessionHandle};
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession};

use crate::harness::{Mode, Server, bot, connect, offline, wait_for, within};
use crate::threads::{host_threads, os_thread_names, os_threads};

/// The cycles after the warm-up.
const CYCLES: u8 = 3;
/// The bots each cycle puts online at once.
const BOTS: u8 = 4;
/// How long the OS thread count may take to come back: three times tokio's
/// 10 s keep-alive of an idle blocking-pool thread, which may come and go.
const THREADS_SETTLE: Duration = Duration::from_secs(30);

/// The process's OS threads after the warm-up.
struct Baseline {
    threads: usize,
    names: BTreeMap<String, usize>,
}

/// A bot that's online, with its name.
struct Online {
    name: String,
    session: McSession,
    events: McEvents,
}

#[tokio::test]
async fn slow_teardown_scenario() {
    let server = Server::start(Mode::Offline).await;
    let connector = AzaleaConnector::new(&McConfig::default());

    cycle(&connector, &server, 0).await;
    nothing_left(&connector, "the warm-up").await;
    let baseline = os_baseline().await;

    for n in 1..=CYCLES {
        cycle(&connector, &server, n).await;
        let after = format!("cycle {n}");
        nothing_left(&connector, &after).await;
        if let Some(baseline) = &baseline {
            os_threads_back_to(baseline, &after).await;
        }
    }
}

/// Puts [`BOTS`] bots online at once, then ends them one after another, each
/// way a session can end. Every bot of the scenario gets a name of its own,
/// so the server never sees a duplicate login.
async fn cycle(connector: &AzaleaConnector, server: &Server, n: u8) {
    let mut bots = Vec::new();
    for index in n * BOTS..(n + 1) * BOTS {
        let name = format!("AfkBot{}", index + 1);
        let (session, mut events) = connect(connector, server, bot(index), offline(&name)).await;
        wait_for(&mut events, &format!("{name}'s join"), |event| {
            *event == SessionEvent::Joined
        })
        .await;
        bots.push(Online {
            name,
            session,
            events,
        });
    }
    let [first, second, kicked, dropped] = <[Online; 4]>::try_from(bots)
        .ok()
        .expect("a cycle has four bots");

    // The full teardown (ADR-0008 §10), then every handle is dropped.
    for bot in [first, second] {
        within("the teardown", bot.session.disconnect()).await;
    }

    // The server ends the session; its owner tears it down afterwards.
    let Online {
        name,
        session,
        mut events,
    } = kicked;
    server.rcon(&format!("kick {name}")).await;
    wait_for(&mut events, &format!("{name}'s kick"), |event| {
        matches!(
            event,
            SessionEvent::Disconnected(DisconnectReason::Kicked(_))
        )
    })
    .await;
    assert_eq!(within("the end of the events", events.next()).await, None);
    within("the teardown", session.disconnect()).await;

    // No teardown: dropping every handle ends the session and its thread.
    drop(dropped);
}

/// Waits until every host thread of `connector` has ended and every World is
/// freed, and checks that none was abandoned.
async fn nothing_left(connector: &AzaleaConnector, after: &str) {
    until(&format!("every host thread ending after {after}"), || {
        connector.pool().live_threads() == 0
    })
    .await;
    until(&format!("every World being freed after {after}"), || {
        connector.live_worlds() == 0
    })
    .await;
    assert_eq!(
        connector.pool().abandoned_threads(),
        0,
        "a host thread was abandoned by the end of {after}"
    );
}

/// Waits, without sleeping, until `done` holds.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    within(what, async {
        while !done() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// Waits, without sleeping, until no host thread is left in the OS's list,
/// then takes the baseline. `None` on every platform but Linux.
async fn os_baseline() -> Option<Baseline> {
    let deadline = Instant::now() + THREADS_SETTLE;
    loop {
        let names = os_thread_names()?;
        if host_threads(&names) == 0 {
            let threads = os_threads()?;
            return Some(Baseline { threads, names });
        }
        assert!(
            Instant::now() < deadline,
            "host threads were still running {THREADS_SETTLE:?} after the warm-up: {names:?}"
        );
        tokio::task::yield_now().await;
    }
}

/// Waits, without sleeping, until the process has at most as many OS threads
/// as at the `baseline`. After [`THREADS_SETTLE`], fails the test and shows
/// the threads by name, now and at the baseline.
async fn os_threads_back_to(baseline: &Baseline, after: &str) {
    let deadline = Instant::now() + THREADS_SETTLE;
    loop {
        let threads = os_threads().expect("the OS thread count was readable at the baseline");
        if threads <= baseline.threads {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{threads} OS threads {THREADS_SETTLE:?} after {after}, more than the {} after the warm-up. Now: {:?}; after the warm-up: {:?}",
            baseline.threads,
            os_thread_names().unwrap_or_default(),
            baseline.names
        );
        tokio::task::yield_now().await;
    }
}
