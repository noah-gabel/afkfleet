//! P1.9: clean disconnect, no leaks. Run it in Linux:
//! `MALLOC_ARENA_MAX=2 linux.sh leak <teardown> [cycles] [bots]`.
//!
//! Every cycle joins `bots` bots with the chosen model (one App and one fresh
//! host thread per bot, single-threaded executor), keeps them online for 10 s, tears
//! them down, drops every handle, settles 15 s and samples the process. A
//! `Weak` reference to each bot's World shows directly whether it was freed.
//!
//! Teardown modes:
//! - `disconnect`: `client.disconnect()` only (handles kept until the end of
//!   the cycle, then dropped)
//! - `exit`: `client.exit()`, wait for the runner to end, drop every handle

use std::{sync::Arc, time::Duration};

use azalea::{Event, account::Account};
use tokio::{runtime, time::timeout};
use tracing::info;

use crate::{
    DEV_SERVER, Res,
    exp::host::wait_for,
    host::Host,
    sample::sample,
    session::{self, Session, Variant},
};

pub fn run(args: &[String]) -> Res {
    let teardown = args.first().map_or("exit", String::as_str).to_owned();
    let cycles: usize = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(6);
    let bots: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(25);
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let s0 = sample();
        info!(
            "LEAK cycle=0 (before any join) rss_mib={} threads={} fds={}",
            s0.as_ref().map_or(0, |s| s.rss_kb / 1024),
            s0.as_ref().map_or(0, |s| s.threads),
            s0.as_ref().map_or(0, |s| s.fds)
        );
        let mut all_worlds = Vec::new();
        for cycle in 1..=cycles {
            // The chosen model: a fresh host thread per bot, closed after teardown.
            let hosts: Vec<Host> = (0..bots)
                .map(|i| Host::spawn(&format!("mc-host-{i}")))
                .collect::<Res<_>>()?;
            let mut sessions: Vec<Session> = Vec::new();
            for i in 0..bots {
                let name = format!("AfkBot{}", i + 1);
                let addr = DEV_SERVER.clone();
                sessions.push(
                    hosts[i]
                        .run(move || {
                            session::connect(
                                Variant::Custom,
                                Account::offline(&name),
                                addr,
                                session::st_setup(true),
                            )
                        })
                        .await??,
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            for s in &mut sessions {
                wait_for(s, |e| matches!(e, Event::Spawn), Duration::from_secs(60)).await?;
            }
            let worlds: Vec<_> = sessions.iter().map(|s| Arc::downgrade(&s.client.ecs)).collect();
            let online = sample();
            tokio::time::sleep(Duration::from_secs(10)).await;

            match teardown.as_str() {
                "disconnect" => {
                    for s in &sessions {
                        s.client.disconnect();
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let runners_ended = sessions
                        .iter_mut()
                        .filter_map(|s| s.runner_end.try_recv().ok())
                        .count();
                    info!(runners_ended, "5 s after disconnect() (handles still held)");
                }
                _ => {
                    for s in &sessions {
                        s.client.exit();
                    }
                    let mut ended = 0;
                    for s in &mut sessions {
                        if timeout(Duration::from_secs(5), &mut s.runner_end).await.is_ok() {
                            ended += 1;
                        }
                    }
                    info!(runners_ended = ended, "after exit()");
                }
            }
            drop(sessions);
            let handles: Vec<_> = hosts.into_iter().filter_map(Host::close).collect();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while handles.iter().any(|h| !h.is_finished()) && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let host_threads_ended = handles.iter().filter(|h| h.is_finished()).count();
            info!(host_threads_ended, of = bots, "host threads after close()");
            tokio::time::sleep(Duration::from_secs(15)).await;
            let alive = worlds.iter().filter(|w| w.strong_count() > 0).count();
            all_worlds.extend(worlds);
            let total_alive = all_worlds.iter().filter(|w| w.strong_count() > 0).count();
            let s = sample();
            info!(
                "LEAK teardown={teardown} cycle={cycle} bots={bots} online_rss_mib={} rss_mib={} threads={} fds={} worlds_alive_this_cycle={alive} worlds_alive_total={total_alive}",
                online.as_ref().map_or(0, |s| s.rss_kb / 1024),
                s.as_ref().map_or(0, |s| s.rss_kb / 1024),
                s.as_ref().map_or(0, |s| s.threads),
                s.as_ref().map_or(0, |s| s.fds),
            );
            if let Some(s) = &s {
                info!(names = ?s.thread_names, "threads by name");
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    std::process::exit(0);
}
