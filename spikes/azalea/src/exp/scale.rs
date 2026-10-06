//! P1.7: resource cost of the hosting models. Run it in Linux:
//! `linux.sh scale <model> <bots>`, one fresh process per data point.
//!
//! Models:
//! - `a1`: one App per bot (Variant C), all on 1 host thread
//! - `a4`: one App per bot, round-robin over 4 host threads
//! - `an`: one App per bot, one host thread per bot
//! - `s10`: Swarm shards of 10 bots, one host thread per shard
//! - `s`: one Swarm with every bot, on 1 host thread
//!
//! Method: staggered joins (250 ms apart), wait for every Spawn, 30 s
//! warm-up, then sample RSS/threads/fds every 5 s for 60 s and compute CPU
//! from utime+stime over the window. Prints one `SCALE …` summary line.

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use azalea::{Event, account::Account};
use tokio::runtime;
use tracing::info;

use crate::{
    DEV_SERVER, Res,
    exp::host::wait_for,
    host::Host,
    sample::{clk_tck, sample},
    session::{self, Session, Variant},
};

const STAGGER: Duration = Duration::from_millis(250);

pub fn run(args: &[String]) -> Res {
    let full_model = args.first().map_or("a4", String::as_str).to_owned();
    // A `-st` suffix switches every App to the single-threaded executor.
    let st = full_model.ends_with("-st");
    let model = full_model.trim_end_matches("-st").to_owned();
    let bots: usize = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(10);
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let baseline = sample();
        let names: Vec<String> = (1..=bots).map(|i| format!("AfkBot{i}")).collect();
        let started = Instant::now();
        let mut hosts: Vec<Host> = Vec::new();
        let mut sessions: Vec<Session> = Vec::new();
        let host_count = match model.as_str() {
            "a1" | "s" => 1,
            "a4" => 4,
            "an" => bots,
            "s10" => bots.div_ceil(10),
            other => return Err(format!("unknown model `{other}`").into()),
        };
        for i in 0..host_count {
            hosts.push(Host::spawn(&format!("mc-host-{i}"))?);
        }
        match model.as_str() {
            "a1" | "a4" | "an" => {
                for (i, name) in names.iter().enumerate() {
                    let host = &hosts[i % host_count];
                    let (name, addr) = (name.clone(), DEV_SERVER.clone());
                    sessions.push(
                        host.run(move || session::connect(Variant::Custom, Account::offline(&name), addr, session::st_setup(st)))
                            .await??,
                    );
                    tokio::time::sleep(STAGGER).await;
                }
            }
            "s" | "s10" => {
                let shard = if model == "s" { bots } else { 10 };
                for (i, chunk) in names.chunks(shard).enumerate() {
                    let accounts: Vec<Account> = chunk.iter().map(|n| Account::offline(n)).collect();
                    let addr = DEV_SERVER.clone();
                    let shard_sessions = hosts[i]
                        .run(move || session::connect_swarm(accounts, addr, session::st_setup(st), Some(STAGGER)))
                        .await??;
                    sessions.extend(shard_sessions);
                }
            }
            _ => unreachable!(),
        }
        for s in &mut sessions {
            wait_for(s, |e| matches!(e, Event::Spawn), Duration::from_secs(120)).await?;
        }
        let join_time = started.elapsed();
        info!(?join_time, bots, %model, "all spawned; warming up 30 s");

        // Drain events in the background so channels never fill up.
        let mut events: Vec<_> = sessions.iter_mut().map(|s| std::mem::replace(&mut s.events, tokio::sync::mpsc::channel(1).1)).collect();
        let drain = tokio::spawn(async move {
            loop {
                for rx in &mut events {
                    while rx.try_recv().is_ok() {}
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });

        tokio::time::sleep(Duration::from_secs(30)).await;
        let window_start = sample();
        let t_start = Instant::now();
        let ticks_start: u64 = sessions.iter().map(|s| s.ticks.ticks.load(Ordering::Relaxed)).sum();
        let mut rss_max = 0;
        let mut rss_sum = 0;
        let mut n = 0;
        let mut last = None;
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let s = sample();
            if let Some(s) = &s {
                rss_max = rss_max.max(s.rss_kb);
                rss_sum += s.rss_kb;
                n += 1;
            }
            last = s;
        }
        let secs = t_start.elapsed().as_secs_f64();
        let ticks_end: u64 = sessions.iter().map(|s| s.ticks.ticks.load(Ordering::Relaxed)).sum();
        let dropped: u64 = sessions.iter().map(|s| s.ticks.dropped_events.load(Ordering::Relaxed)).sum();
        let tps = (ticks_end - ticks_start) as f64 / secs / bots as f64;
        match (baseline, window_start, last) {
            (Some(b), Some(w), Some(l)) => {
                let cores = (l.cpu_ticks - w.cpu_ticks) as f64 / clk_tck() as f64 / secs;
                info!(
                    "SCALE model={full_model} bots={bots} hosts={host_count} join_s={:.1} rss_base_mib={} rss_avg_mib={} rss_max_mib={} rss_per_bot_mib={:.1} cpu_cores={cores:.3} cpu_per_bot_pct={:.2} threads={} fds={} tps_per_bot={tps:.1} dropped_events={dropped} cpus={} names={:?}",
                    join_time.as_secs_f64(),
                    b.rss_kb / 1024,
                    rss_sum / n.max(1) / 1024,
                    rss_max / 1024,
                    (rss_sum / n.max(1)).saturating_sub(b.rss_kb) as f64 / 1024.0 / bots as f64,
                    cores * 100.0 / bots as f64,
                    l.threads,
                    l.fds,
                    std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
                    l.thread_names,
                );
            }
            _ => info!("SCALE model={full_model} bots={bots}: no /proc samples (not Linux) tps_per_bot={tps:.1}"),
        }
        drain.abort();
        // No exit(): a Swarm can deadlock on teardown (P1.2). Ending the
        // process closes every connection.
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    std::process::exit(0);
}
