//! P1.3: what failures look like, with auto-reconnect and auto-respawn off.
//!
//! `cargo run -- fail <scenario> [custom|join]` (default variant: `custom`,
//! i.e. both plugins disabled; `join` = azalea's defaults, for comparison).
//! Every non-tick event after the trigger is logged with [`describe`].

use std::time::{Duration, Instant};

use azalea::{Event, FormattedText, account::Account};
use tokio::{runtime, time::timeout};
use tracing::info;

use crate::{
    DEV_SERVER, Res,
    exp::host::wait_for,
    host::Host,
    rcon::{self, DEV_CONTAINER, MISMATCH_COMPOSE},
    session::{self, Session, Variant},
};

pub fn run(args: &[String]) -> Res {
    let scenario = args
        .first()
        .ok_or("usage: fail <scenario> [custom|join]")?
        .clone();
    let variant: Variant = args.get(1).map_or("custom", String::as_str).parse()?;
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let host = Host::spawn("mc-host-0")?;
        let h = &host;
        let join = |name: &str, addr: &str| {
            let (name, addr) = (name.to_owned(), addr.to_owned());
            async move {
                h.run(move || session::connect(variant, Account::offline(&name), addr, None))
                    .await?
            }
        };
        match scenario.as_str() {
            // --- Connection failures ---
            "closed" => connect_failure(join("AfkBot1", "127.0.0.1:25599").await?, 30).await,
            "blackhole" => connect_failure(join("AfkBot1", "192.0.2.10:25565").await?, 150).await,
            "blackhole-exit" => {
                let mut s = join("AfkBot1", "192.0.2.10:25565").await?;
                tokio::time::sleep(Duration::from_secs(2)).await;
                info!("calling exit() while the TCP connect is still pending");
                s.client.exit();
                let closed = timeout(Duration::from_secs(5), async {
                    while let Some(e) = s.events.recv().await {
                        info!(event = %describe_event(&e), "event");
                    }
                })
                .await
                .is_ok();
                let runner = timeout(Duration::from_secs(5), &mut s.runner_end).await;
                info!(events_closed = closed, ?runner, "after exit() during connect");
                Ok(())
            }
            "unresolvable" => {
                let started = Instant::now();
                match join("AfkBot1", "nonexistent.invalid").await {
                    Ok(_) => info!("connect unexpectedly succeeded"),
                    Err(e) => info!(error = %e, elapsed = ?started.elapsed(), "connect returned an error"),
                }
                Ok(())
            }
            // --- Kicks from a running server ---
            "kick" => kicked(join("AfkBot1", DEV_SERVER.as_str()).await?, "kick AfkBot1 Bye from the spike").await,
            "kick-noreason" => kicked(join("AfkBot1", DEV_SERVER.as_str()).await?, "kick AfkBot1").await,
            "ban" => {
                let s = join("AfkBot1", DEV_SERVER.as_str()).await?;
                kicked(s, "ban AfkBot1 Spike test ban").await?;
                info!("rejoining while banned");
                observe(&mut join("AfkBot1", DEV_SERVER.as_str()).await?, 10).await;
                info!(out = %rcon::rcon("pardon AfkBot1")?, "pardoned");
                Ok(())
            }
            "ban-ip" => {
                let s = join("AfkBot1", DEV_SERVER.as_str()).await?;
                kicked(s, "ban-ip AfkBot1 Spike test ip ban").await?;
                info!("rejoining while IP-banned");
                observe(&mut join("AfkBot2", DEV_SERVER.as_str()).await?, 10).await;
                let banlist = rcon::rcon("banlist ips")?;
                info!(lines = banlist.lines().count(), "ip banlist (not logged: holds an IP)");
                // The banned IP is the Docker gateway; pardon every listed IP.
                // The output glues tokens together ("ban(s):172.x.y.z"), so pull
                // out every dotted-quad.
                for token in banlist.split_whitespace() {
                    let ip: String = token
                        .chars()
                        .skip_while(|c| !c.is_ascii_digit())
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    if ip.matches('.').count() == 3 {
                        let _ = rcon::rcon(&format!("pardon-ip {ip}"));
                    }
                }
                info!(remaining = %rcon::rcon("banlist ips")?, "after pardon-ip");
                Ok(())
            }
            "whitelist" => {
                info!(out = %rcon::rcon("whitelist on")?, "whitelist");
                observe(&mut join("AfkBot9", DEV_SERVER.as_str()).await?, 10).await;
                info!(out = %rcon::rcon("whitelist off")?, "whitelist");
                Ok(())
            }
            "duplicate" => {
                let mut first = join("AfkBot1", DEV_SERVER.as_str()).await?;
                wait_for(&mut first, |e| matches!(e, Event::Spawn), Duration::from_secs(15)).await?;
                info!("first AfkBot1 is online; joining a second AfkBot1");
                let mut second = join("AfkBot1", DEV_SERVER.as_str()).await?;
                let (a, b) = tokio::join!(observe(&mut first, 10), observe(&mut second, 10));
                info!(first = a, second = b, "events seen (first, second)");
                Ok(())
            }
            "full" => {
                info!("restarting the dev server with MC_MAX_PLAYERS=1");
                rcon::dev_up(&[("MC_MAX_PLAYERS", "1")])?;
                let mut first = join("AfkBot1", DEV_SERVER.as_str()).await?;
                wait_for(&mut first, |e| matches!(e, Event::Spawn), Duration::from_secs(30)).await?;
                observe(&mut join("AfkBot2", DEV_SERVER.as_str()).await?, 10).await;
                first.client.exit();
                info!("restoring the dev server");
                rcon::dev_up(&[])?;
                Ok(())
            }
            "outdated" => {
                let version = args.get(2).map_or("26.3", String::as_str);
                info!(%version, "starting the mismatch server on 127.0.0.1:25566");
                rcon::compose(
                    MISMATCH_COMPOSE,
                    &["up", "--detach", "--wait", "--wait-timeout", "300"],
                    &[("MC_MISMATCH_VERSION", version)],
                )?;
                observe(&mut join("AfkBot1", "127.0.0.1:25566").await?, 10).await;
                rcon::compose(MISMATCH_COMPOSE, &["down", "--volumes"], &[])?;
                Ok(())
            }
            "idle" => {
                info!(out = %rcon::rcon("setidletimeout 1")?, "idle timeout");
                let mut s = join("AfkBot1", DEV_SERVER.as_str()).await?;
                wait_for(&mut s, |e| matches!(e, Event::Spawn), Duration::from_secs(15)).await?;
                let started = Instant::now();
                observe(&mut s, 120).await;
                info!(elapsed = ?started.elapsed(), "idle observation over");
                info!(out = %rcon::rcon("setidletimeout 0")?, "idle timeout");
                Ok(())
            }
            "stop" => {
                let mut s = join("AfkBot1", DEV_SERVER.as_str()).await?;
                wait_for(&mut s, |e| matches!(e, Event::Spawn), Duration::from_secs(15)).await?;
                info!(out = %rcon::rcon("stop")?, "rcon stop");
                observe(&mut s, 20).await;
                s.client.exit();
                info!("restarting the dev server");
                rcon::dev_up(&[])?;
                Ok(())
            }
            "kill" => {
                let mut s = join("AfkBot1", DEV_SERVER.as_str()).await?;
                wait_for(&mut s, |e| matches!(e, Event::Spawn), Duration::from_secs(15)).await?;
                info!(out = %rcon::docker(&["kill", DEV_CONTAINER])?, "docker kill");
                observe(&mut s, 20).await;
                s.client.exit();
                info!("restarting the dev server");
                rcon::dev_up(&[])?;
                Ok(())
            }
            "pause" => {
                let mut s = join("AfkBot1", DEV_SERVER.as_str()).await?;
                wait_for(&mut s, |e| matches!(e, Event::Spawn), Duration::from_secs(15)).await?;
                let before = s.ticks.ticks.load(std::sync::atomic::Ordering::Relaxed);
                info!(out = %rcon::docker(&["pause", DEV_CONTAINER])?, "docker pause (server frozen, TCP open)");
                let started = Instant::now();
                observe(&mut s, 75).await;
                let after = s.ticks.ticks.load(std::sync::atomic::Ordering::Relaxed);
                info!(
                    ticks_during_pause = after - before,
                    since_last_tick_ms = ?s.ticks.since_last_tick_ms(),
                    elapsed = ?started.elapsed(),
                    "client ticks while the server was frozen"
                );
                rcon::docker(&["unpause", DEV_CONTAINER])?;
                info!("unpaused");
                observe(&mut s, 10).await;
                s.client.exit();
                Ok(())
            }
            // --- Plugins: reconnect and respawn ---
            "death" => {
                let mut s = join("AfkBot1", DEV_SERVER.as_str()).await?;
                wait_for(&mut s, |e| matches!(e, Event::Spawn), Duration::from_secs(15)).await?;
                info!(out = %rcon::rcon("kill AfkBot1")?, "rcon kill");
                observe(&mut s, 10).await;
                let health = {
                    let c = s.client.clone();
                    host.run(move || async move { c.health() }).await?
                };
                info!(health, "health 10 s after death (0 = still dead, not respawned)");
                s.client.exit();
                Ok(())
            }
            other => Err(format!("unknown scenario `{other}`").into()),
        }
    })
}

/// Waits for the first events after a connect to an address that fails.
async fn connect_failure(mut s: Session, secs: u64) -> Res {
    let started = Instant::now();
    timeout(Duration::from_secs(secs), async {
        while let Some(e) = s.events.recv().await {
            info!(elapsed = ?started.elapsed(), event = %describe_event(&e), "event");
            if let Event::ConnectionFailed(_) = e {
                break;
            }
        }
    })
    .await
    .map_err(|_| format!("no ConnectionFailed within {secs} s"))?;
    // With auto-reconnect off: does anything else happen? Does the runner stay?
    observe(&mut s, 8).await;
    let runner_ended = s.runner_end.try_recv().is_ok();
    info!(runner_ended, "8 s after ConnectionFailed");
    s.client.exit();
    Ok(())
}

/// Joins, triggers a kick with an RCON command, and logs what follows.
async fn kicked(mut s: Session, command: &str) -> Res {
    wait_for(
        &mut s,
        |e| matches!(e, Event::Spawn),
        Duration::from_secs(15),
    )
    .await?;
    info!(%command, out = %rcon::rcon(command)?, "rcon");
    // 10 s: long enough to see an auto-reconnect (default delay 5 s) if it's on.
    observe(&mut s, 10).await;
    let runner_ended = s.runner_end.try_recv().is_ok();
    info!(runner_ended, "10 s after the kick");
    s.client.exit();
    Ok(())
}

/// Logs every event for `secs` seconds (or until the channel closes) and
/// returns how many there were.
pub async fn observe(s: &mut Session, secs: u64) -> usize {
    let started = Instant::now();
    let mut count = 0;
    let _ = timeout(Duration::from_secs(secs), async {
        while let Some(e) = s.events.recv().await {
            count += 1;
            info!(bot = %s.name, elapsed = ?started.elapsed(), event = %describe_event(&e), "event");
        }
        info!(bot = %s.name, "event channel closed");
    })
    .await;
    count
}

/// One-line description of an event; kick reasons via [`describe`].
pub fn describe_event(e: &Event) -> String {
    match e {
        Event::Disconnect(Some(reason)) => format!("Disconnect({})", describe(reason)),
        Event::Disconnect(None) => "Disconnect(None)".to_owned(),
        Event::ConnectionFailed(err) => {
            let azalea::protocol::connect::ConnectionError::Io(io) = err.as_ref();
            format!("ConnectionFailed(Io kind={:?} msg={io})", io.kind())
        }
        Event::Chat(chat) => format!("Chat({})", describe(&chat.message())),
        Event::Death(packet) => format!("Death(packet={})", packet.is_some()),
        Event::AddPlayer(p) | Event::RemovePlayer(p) | Event::UpdatePlayer(p) => {
            let kind = match e {
                Event::AddPlayer(_) => "AddPlayer",
                Event::RemovePlayer(_) => "RemovePlayer",
                _ => "UpdatePlayer",
            };
            format!("{kind}({})", p.profile.name)
        }
        Event::Packet(_) => "Packet".to_owned(),
        Event::ReceiveChunk(_) => "ReceiveChunk".to_owned(),
        Event::KeepAlive(_) => "KeepAlive".to_owned(),
        other => format!("{other:?}").chars().take(60).collect(),
    }
}

/// Structure of a `FormattedText`: Text vs Translatable, key, args, siblings,
/// and the plain `Display` text.
pub fn describe(text: &FormattedText) -> String {
    match text {
        FormattedText::Translatable(t) => format!(
            "Translatable key={:?} fallback={:?} args=[{}] siblings=[{}] plain={:?}",
            t.key,
            t.fallback,
            t.args
                .iter()
                .map(|a| format!("{a:?}").chars().take(80).collect::<String>())
                .collect::<Vec<_>>()
                .join(", "),
            t.base
                .siblings
                .iter()
                .map(describe)
                .collect::<Vec<_>>()
                .join("; "),
            text.to_string()
        ),
        FormattedText::Text(t) => format!(
            "Text text={:?} siblings=[{}] plain={:?}",
            t.text,
            t.base
                .siblings
                .iter()
                .map(describe)
                .collect::<Vec<_>>()
                .join("; "),
            text.to_string()
        ),
    }
}
