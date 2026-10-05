//! P1.1: one offline bot joins the dev server with plain `Client::join`.
//!
//! `cargo run -- join [secs]`: stays online for `secs` seconds (default: until
//! Ctrl-C), then calls `exit()`.

use std::time::Duration;

use azalea::{Client, Event, account::Account};
use tokio::{runtime, task::LocalSet};
use tracing::info;

use crate::{DEV_SERVER, Res};

pub fn run(args: &[String]) -> Res {
    let hold = args
        .first()
        .map(|s| s.parse::<u64>())
        .transpose()?
        .map(Duration::from_secs);

    // azalea's ECS runner uses `spawn_local`, so `Client::join` must run inside
    // a LocalSet on a current-thread runtime.
    let rt = runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    LocalSet::new().block_on(&rt, async move {
        let (bot, mut events) = Client::join(Account::offline("AfkBot1"), DEV_SERVER).await?;
        let deadline = async {
            match hold {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(Event::Login) => info!("login"),
                    Some(Event::Spawn) => info!(position = ?bot.position(), "spawn"),
                    Some(Event::Disconnect(reason)) => {
                        info!(reason = ?reason.map(|r| r.to_string()), "disconnected");
                        break;
                    }
                    Some(Event::ConnectionFailed(e)) => {
                        info!(error = %e, "connection failed");
                        break;
                    }
                    Some(_) => {}
                    None => {
                        info!("event channel closed");
                        break;
                    }
                },
                _ = &mut deadline => {
                    info!("hold time over");
                    break;
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("ctrl-c");
                    break;
                }
            }
        }
        bot.exit();
        Ok(())
    })
}
