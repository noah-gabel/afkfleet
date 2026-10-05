//! P1.6: a panic or a hang inside an ECS system. Run it in Linux
//! (`linux.sh fault <mode>`), so thread names and fds are visible.
//!
//! Modes:
//! - `custom-panic`: AfkBot1–3 as separate Apps on host thread 0, AfkBot4 on
//!   host thread 1; AfkBot1's App panics.
//! - `swarm-panic`: AfkBot1–3 as one Swarm on host 0, AfkBot4 on host 1; the
//!   Swarm's App panics.
//! - `custom-hang`: like `custom-panic`, but AfkBot1's system loops forever.
//! - `starve`: one hanging App per Bevy compute-pool thread, each on its own
//!   host thread, plus a healthy bot on another host thread.
//!
//! After the fault: 20 s of observation (ticks, events, runner, host thread,
//! whether AfkBot4 sees AfkBot1 leave the tab list = the server dropped it),
//! then all of AfkBot1's handles are dropped and 25 s more are observed.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use azalea::{
    Event,
    account::Account,
    app::App,
    ecs::{prelude::Res as EcsRes, resource::Resource},
    prelude::GameTick,
};
use tokio::{runtime, time::timeout};
use tracing::info;

// `#[derive(Resource)]` expands to `bevy_ecs::…` paths.
use azalea::ecs as bevy_ecs;

use crate::{
    DEV_SERVER, Res,
    exp::host::wait_for,
    host::Host,
    sample::{brief, sample},
    session::{self, AppSetup, Session, Variant},
};

const PANIC: u8 = 1;
const HANG: u8 = 2;

#[derive(Resource, Clone)]
struct Fault(Arc<AtomicU8>);

fn fault_system(fault: EcsRes<Fault>) {
    match fault.0.load(Ordering::SeqCst) {
        PANIC => panic!("injected panic (P1.6)"),
        HANG => loop {
            std::thread::sleep(Duration::from_secs(1));
        },
        _ => {}
    }
}

fn fault_setup(flag: &Arc<AtomicU8>) -> AppSetup {
    let flag = flag.clone();
    Box::new(move |app: &mut App| {
        app.insert_resource(Fault(flag));
        app.add_systems(GameTick, fault_system);
    })
}

pub fn run(args: &[String]) -> Res {
    let mode = args
        .first()
        .map_or("custom-panic", String::as_str)
        .to_owned();
    // `st`: every App uses the single-threaded executor.
    let st = args.get(1).is_some_and(|a| a == "st");
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = rt.block_on(async move {
        let host0 = Host::spawn("mc-host-0")?;
        let host1 = Host::spawn("mc-host-1")?;
        let addr = DEV_SERVER.clone();
        let flag = Arc::new(AtomicU8::new(0));
        info!(st, "single-threaded executor");
        info!(before = %brief(&sample()), "process before joining");

        let mut sessions: Vec<Session> = Vec::new();
        let mut hosts_of: Vec<usize> = Vec::new();
        let mut extra_hosts = Vec::new();
        match mode.as_str() {
            "custom-panic" | "custom-hang" => {
                for (i, name) in ["AfkBot1", "AfkBot2", "AfkBot3"].into_iter().enumerate() {
                    let setup = session::combine(
                        (i == 0).then(|| fault_setup(&flag)),
                        session::st_setup(st),
                    );
                    let a = addr.clone();
                    sessions.push(
                        host0
                            .run(move || {
                                session::connect(Variant::Custom, Account::offline(name), a, setup)
                            })
                            .await??,
                    );
                    hosts_of.push(0);
                }
            }
            "swarm-panic" => {
                let setup = session::combine(Some(fault_setup(&flag)), session::st_setup(st));
                let a = addr.clone();
                let swarm = host0
                    .run(move || {
                        let accounts = ["AfkBot1", "AfkBot2", "AfkBot3"]
                            .map(Account::offline)
                            .to_vec();
                        session::connect_swarm(accounts, a, setup, Some(Duration::from_millis(300)))
                    })
                    .await??;
                hosts_of.extend([0, 0, 0]);
                sessions.extend(swarm);
            }
            "starve" => {
                let pool = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
                info!(pool, "hanging one App per compute-pool thread");
                for i in 0..pool {
                    let name = format!("AfkBot{}", i + 1);
                    let host = Host::spawn(&format!("mc-host-x{i}"))?;
                    let a = addr.clone();
                    let setup = session::combine(Some(fault_setup(&flag)), session::st_setup(st));
                    sessions.push(
                        host.run(move || {
                            session::connect(Variant::Custom, Account::offline(&name), a, setup)
                        })
                        .await??,
                    );
                    hosts_of.push(2 + i);
                    extra_hosts.push(host);
                }
            }
            other => return Err(format!("unknown mode `{other}`").into()),
        }
        // The witness: a healthy bot on another host thread.
        let a = addr.clone();
        let mut witness = host1
            .run(move || {
                session::connect(
                    Variant::Custom,
                    Account::offline("AfkBot9"),
                    a,
                    session::st_setup(st),
                )
            })
            .await??;
        for s in &mut sessions {
            wait_for(s, |e| matches!(e, Event::Spawn), Duration::from_secs(20)).await?;
        }
        wait_for(
            &mut witness,
            |e| matches!(e, Event::Spawn),
            Duration::from_secs(20),
        )
        .await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        info!(steady = %brief(&sample()), "process with all bots online");

        let fault = if mode.ends_with("panic") { PANIC } else { HANG };
        info!(
            fault = if fault == PANIC { "panic" } else { "hang" },
            "--- injecting the fault"
        );
        let t0 = Instant::now();
        flag.store(fault, Ordering::SeqCst);

        let mut removed_at: Option<Duration> = None;
        let mut last_ticks: Vec<u64> = sessions
            .iter()
            .map(|s| s.ticks.ticks.load(Ordering::Relaxed))
            .collect();
        let mut witness_last = witness.ticks.ticks.load(Ordering::Relaxed);
        let report = |label: &str,
                      sessions: &mut [Session],
                      last_ticks: &mut Vec<u64>,
                      witness: &mut Session,
                      witness_last: &mut u64,
                      removed_at: &mut Option<Duration>| {
            let mut parts = Vec::new();
            for (i, s) in sessions.iter_mut().enumerate() {
                let now = s.ticks.ticks.load(Ordering::Relaxed);
                let delta = now - last_ticks[i];
                last_ticks[i] = now;
                let closed = loop {
                    match s.events.try_recv() {
                        Ok(_) => {}
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break false,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break true,
                    }
                };
                let runner = s.runner_end.try_recv().ok();
                parts.push(format!(
                    "{}:+{delta}t{}{}",
                    s.name,
                    if closed { " closed" } else { "" },
                    runner.map(|r| format!(" runner=[{r}]")).unwrap_or_default()
                ));
            }
            let wnow = witness.ticks.ticks.load(Ordering::Relaxed);
            let wdelta = wnow - *witness_last;
            *witness_last = wnow;
            while let Ok(e) = witness.events.try_recv() {
                if let Event::RemovePlayer(p) = e
                    && p.profile.name == "AfkBot1"
                    && removed_at.is_none()
                {
                    *removed_at = Some(t0.elapsed());
                }
            }
            info!(
                t = ?t0.elapsed().as_secs(),
                %label,
                bots = %parts.join("  "),
                witness_ticks = wdelta,
                server_dropped_afkbot1_at = ?removed_at,
                "observe"
            );
        };

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            report(
                "after fault",
                &mut sessions,
                &mut last_ticks,
                &mut witness,
                &mut witness_last,
                &mut removed_at,
            );
        }
        let host0_responds = timeout(Duration::from_secs(1), host0.run(|| async {}))
            .await
            .is_ok();
        let host1_responds = timeout(Duration::from_secs(1), host1.run(|| async {}))
            .await
            .is_ok();
        info!(
            host0_responds,
            host0_alive = !host0.is_finished(),
            host1_responds,
            process = %brief(&sample()),
            "hosts 20 s after the fault"
        );

        info!("--- dropping every handle of AfkBot1 (Client clones, events, runner_end)");
        let first = sessions.remove(0);
        last_ticks.remove(0);
        drop(first);
        for _ in 0..25 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            report(
                "after drop",
                &mut sessions,
                &mut last_ticks,
                &mut witness,
                &mut witness_last,
                &mut removed_at,
            );
        }
        info!(process = %brief(&sample()), "end");
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    // Hung host threads never finish; leave without joining them.
    info!(?result, "exiting the process");
    std::process::exit(i32::from(result.is_err()));
}
