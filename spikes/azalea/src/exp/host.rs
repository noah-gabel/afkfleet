//! P1.2: bots on a dedicated MC host thread, used from a multi-threaded
//! runtime.
//!
//! `cargo run -- host [join|builder|custom] [bots]` (defaults: `custom 3`).
//! All bots share one host thread. The multi-threaded side gets each
//! `(Client, events)` back over a oneshot, waits for Spawn, then calls the
//! client from a tokio worker thread, then exits every bot and checks that
//! the event channels close and the host tasks go away.

use std::{sync::atomic::Ordering, time::Duration};

use azalea::{Client, Event, account::Account};
use tokio::{runtime, time::timeout};
use tracing::info;

use crate::{
    DEV_SERVER, Res,
    host::Host,
    session::{self, Session, Variant},
};

const fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
const _: () = assert_send_sync_clone::<Client>();

pub fn run(args: &[String]) -> Res {
    let variant: Variant = args.first().map_or("custom", String::as_str).parse()?;
    let bots: usize = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(3);

    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let host = Host::spawn("mc-host-0")?;
        let mut sessions = Vec::new();
        for i in 1..=bots {
            let name = format!("AfkBot{i}");
            let session = host
                .run(move || {
                    session::connect(
                        variant,
                        Account::offline(&name),
                        DEV_SERVER.to_owned(),
                        None,
                    )
                })
                .await??;
            sessions.push(session);
        }
        info!(
            ?variant,
            bots,
            host_tasks = host.tasks.load(Ordering::SeqCst),
            "all clients handed over"
        );

        for s in &mut sessions {
            wait_for(s, |e| matches!(e, Event::Spawn), Duration::from_secs(15)).await?;
        }
        info!("all spawned");

        // Use each client from a tokio worker thread of the multi-threaded runtime.
        for s in &sessions {
            let client = s.client.clone();
            let (thread, position) = tokio::spawn(async move {
                client.set_direction(90.0, 10.0);
                (
                    std::thread::current().name().map(str::to_owned),
                    client.position(),
                )
            })
            .await?;
            info!(bot = %s.name, ?thread, ?position, "called from a worker thread");
        }

        tokio::time::sleep(Duration::from_secs(3)).await;
        for s in &sessions {
            info!(
                bot = %s.name,
                direction = ?s.client.direction(),
                ticks = s.ticks.ticks.load(Ordering::Relaxed),
                since_last_tick_ms = ?s.ticks.since_last_tick_ms(),
                dropped_events = s.ticks.dropped_events.load(Ordering::Relaxed),
                "after 3 s"
            );
        }

        for s in &sessions {
            s.client.exit();
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        for s in &sessions {
            let client = s.client.clone();
            let entities = host
                .run(move || async move { client.ecs.read().entities().len() })
                .await?;
            info!(bot = %s.name, entities, "entities in this World 2 s after exit()");
        }
        let checks = sessions.into_iter().map(|mut s| async move {
            let closed = timeout(Duration::from_secs(10), async {
                while s.events.recv().await.is_some() {}
            })
            .await
            .is_ok();
            let runner = timeout(Duration::from_secs(10), &mut s.runner_end).await;
            info!(bot = %s.name, events_closed = closed, ?runner, "after exit()");
        });
        futures_join_all(checks).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        info!(
            host_tasks = host.tasks.load(Ordering::SeqCst),
            host_alive = !host.is_finished(),
            "end"
        );
        Ok(())
    })
}

/// Waits until an event matching `pred` arrives (other events are dropped).
pub async fn wait_for(
    session: &mut Session,
    pred: impl Fn(&Event) -> bool,
    limit: Duration,
) -> Res<Event> {
    let bot = session.name.clone();
    timeout(limit, async {
        while let Some(event) = session.events.recv().await {
            if pred(&event) {
                return Ok(event);
            }
        }
        Err(format!("{bot}: event channel closed").into())
    })
    .await
    .map_err(|_| format!("{bot}: timed out waiting for an event"))?
}

/// Minimal `join_all` so the spike doesn't need the `futures` crate.
async fn futures_join_all<F: std::future::Future<Output = ()> + Send + 'static>(
    futures: impl IntoIterator<Item = F>,
) {
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    for handle in handles {
        let _ = handle.await;
    }
}

/// `host-builder-exit [nested|plain]`: does `ClientBuilder::start()` return
/// after `exit()`? `plain`: `start()` is the root future of `block_on` on its
/// own thread. `nested`: `start()` runs inside a task of an outer `LocalSet`,
/// like on our host thread.
pub fn builder_exit(args: &[String]) -> Res {
    use azalea::{ClientBuilder, DefaultPlugins, app::PluginGroup, bot::DefaultBotPlugins};
    use azalea::{auto_reconnect::AutoReconnectPlugin, auto_respawn::AutoRespawnPlugin};

    static REMOTE: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    async fn handle(bot: Client, event: Event, _: azalea::NoState) {
        if let Event::Spawn = event {
            if REMOTE_MODE.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = REMOTE.set(bot);
                return;
            }
            info!("spawned; calling exit() in 2 s");
            tokio::time::sleep(Duration::from_secs(2)).await;
            bot.exit();
            info!("exit() called");
        }
    }

    static REMOTE_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let nested = args.first().is_some_and(|a| a.starts_with("nested"));
    let remote = args.first().is_some_and(|a| a.ends_with("remote"));
    REMOTE_MODE.store(remote, std::sync::atomic::Ordering::SeqCst);
    let thread = std::thread::spawn(move || -> Res<String> {
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let start = async {
            ClientBuilder::new_without_plugins()
                .add_plugins(DefaultPlugins)
                .add_plugins(
                    DefaultBotPlugins
                        .build()
                        .disable::<AutoReconnectPlugin>()
                        .disable::<AutoRespawnPlugin>(),
                )
                .reconnect_after(None)
                .set_handler(handle)
                .start(Account::offline("AfkBot1"), DEV_SERVER)
                .await
        };
        let exit = if nested {
            tokio::task::LocalSet::new()
                .block_on(&rt, async { tokio::task::spawn_local(start).await })?
        } else {
            rt.block_on(start)
        };
        Ok(format!("{exit:?}"))
    });
    if remote {
        while REMOTE.get().is_none() {
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_secs(2));
        info!("calling exit() from the main thread");
        if let Some(c) = REMOTE.get() {
            c.exit();
        }
        info!("exit() returned");
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !thread.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if thread.is_finished() {
        let result = thread.join().map_err(|_| "builder thread panicked")??;
        info!(nested, %result, "start() returned");
    } else {
        info!(nested, "start() did NOT return within 30 s of starting");
    }
    Ok(())
}

/// `exit-race [custom|builder|join] [trials]`: how often does one `exit()`
/// from another thread get lost, and does calling it again help? Each trial
/// joins a fresh bot, waits for Spawn, then calls `exit()` until the event
/// channel closes (up to 5 calls, 1 s apart).
pub fn exit_race(args: &[String]) -> Res {
    let variant: Variant = args.first().map_or("custom", String::as_str).parse()?;
    let trials: usize = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(10);
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let host = Host::spawn("mc-host-0")?;
        let mut calls_needed = Vec::new();
        for trial in 1..=trials {
            let mut s = host
                .run(move || {
                    session::connect(
                        variant,
                        Account::offline("AfkBot1"),
                        DEV_SERVER.to_owned(),
                        None,
                    )
                })
                .await??;
            wait_for(
                &mut s,
                |e| matches!(e, Event::Spawn),
                Duration::from_secs(15),
            )
            .await?;
            // Let it run for a moment so exit() lands at a random point in the loop.
            tokio::time::sleep(Duration::from_millis(500 + 37 * trial as u64)).await;
            let mut calls = 0;
            let mut closed = false;
            while calls < 5 && !closed {
                s.client.exit();
                calls += 1;
                closed = timeout(Duration::from_secs(1), async {
                    while s.events.recv().await.is_some() {}
                })
                .await
                .is_ok();
            }
            info!(
                trial,
                calls, closed, "exit() calls until the event channel closed"
            );
            calls_needed.push(if closed { calls } else { 0 });
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let lost_first = calls_needed.iter().filter(|&&c| c != 1).count();
        info!(
            ?variant,
            trials,
            lost_first,
            ?calls_needed,
            "summary (0 = never closed)"
        );
        Ok(())
    })
}
