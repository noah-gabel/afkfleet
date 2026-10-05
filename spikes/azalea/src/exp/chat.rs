//! P1.4: chat. `cargo run -- chat`
//!
//! AfkBot1 listens, AfkBot2 talks, and RCON plays the console. For every chat
//! packet AfkBot1 receives, log the packet kind, azalea's sender/content
//! split, the sender UUID, `is_whisper`, the registry chat kind, the
//! signature, and the `FormattedText` structure. All client calls run on the
//! host thread.

use std::time::Duration;

use azalea::{Client, Event, account::Account, chat::ChatPacket};
use tokio::{runtime, time::timeout};
use tracing::info;

use crate::{
    DEV_SERVER, Res,
    exp::{fail::describe, host::wait_for},
    host::Host,
    rcon,
    session::{self, Session, Variant},
};

enum Step {
    Talker(&'static str, String),
    Listener(&'static str, String),
    ListenerCommandPacket(&'static str),
    Rcon(&'static str),
}

pub fn run(_args: &[String]) -> Res {
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let host = Host::spawn("mc-host-0")?;
        let connect = |name: &'static str| {
            host.run(move || {
                session::connect(
                    Variant::Custom,
                    Account::offline(name),
                    DEV_SERVER.to_owned(),
                    None,
                )
            })
        };
        let mut listener = connect("AfkBot1").await??;
        wait_for(
            &mut listener,
            |e| matches!(e, Event::Spawn),
            Duration::from_secs(15),
        )
        .await?;
        let mut talker = connect("AfkBot2").await??;
        wait_for(
            &mut talker,
            |e| matches!(e, Event::Spawn),
            Duration::from_secs(15),
        )
        .await?;
        info!("--- AfkBot2 joined (join message)");
        drain(&mut listener, 1500).await;

        let steps = vec![
            Step::Talker("plain chat", "hello from AfkBot2".into()),
            Step::Talker("whisper /msg", "/msg AfkBot1 psst, a whisper".into()),
            Step::Talker("emote /me", "/me waves".into()),
            Step::Rcon("say Hello from the console"),
            Step::Rcon("tell AfkBot1 a whisper from the console"),
            Step::Rcon(r#"tellraw AfkBot1 {"text":"red text","color":"red"}"#),
            Step::Rcon("tellraw AfkBot1 \"legacy §ccodes§r in a text component\""),
            Step::Rcon(r#"tellraw AfkBot1 {"text":"<AfkBot7> I am not really AfkBot7"}"#),
            Step::Talker("control chars and §", "a§cb\u{7}c\td".into()),
            Step::Talker("300 characters", "x".repeat(300)),
            Step::Listener("command via chat()", "/list".into()),
            Step::ListenerCommandPacket("list"),
        ];
        for step in steps {
            match step {
                Step::Talker(label, text) => {
                    info!("--- {label}: AfkBot2 chat({} chars)", text.chars().count());
                    say(&host, &talker.client, text).await?;
                }
                Step::Listener(label, text) => {
                    info!("--- {label}: AfkBot1 chat({text:?})");
                    say(&host, &listener.client, text).await?;
                }
                Step::ListenerCommandPacket(cmd) => {
                    info!("--- AfkBot1 write_command_packet({cmd:?})");
                    let c = listener.client.clone();
                    host.run(move || async move { c.write_command_packet(cmd) })
                        .await?;
                }
                Step::Rcon(cmd) => {
                    info!("--- rcon {cmd:?}");
                    rcon::rcon(cmd)?;
                }
            }
            drain(&mut listener, 1500).await;
        }

        info!("--- AfkBot2 leaves (leave message)");
        talker.client.exit();
        drain(&mut listener, 1500).await;
        listener.client.exit();
        Ok(())
    })
}

async fn say(host: &Host, client: &Client, text: String) -> Res {
    let c = client.clone();
    host.run(move || async move { c.chat(text) }).await
}

/// Logs every chat event the session receives within `ms` milliseconds.
async fn drain(s: &mut Session, ms: u64) {
    let _ = timeout(Duration::from_millis(ms), async {
        while let Some(event) = s.events.recv().await {
            if let Event::Chat(packet) = event {
                info!(bot = %s.name, "{}", chat_details(&packet));
            } else if let Event::Disconnect(reason) = event {
                info!(bot = %s.name, ?reason, "DISCONNECTED");
            }
        }
    })
    .await;
}

fn chat_details(p: &ChatPacket) -> String {
    let (kind, extra) = match p {
        ChatPacket::System(s) => (
            if s.overlay {
                "System(overlay)"
            } else {
                "System"
            },
            String::new(),
        ),
        ChatPacket::Player(pc) => (
            "Player",
            format!(
                " chat_type={:?} signed={} unsigned_content={}",
                pc.chat_type.chat_type,
                pc.signature.is_some(),
                pc.unsigned_content.is_some()
            ),
        ),
        ChatPacket::Disguised(d) => (
            "Disguised",
            format!(" chat_type={:?}", d.chat_type.chat_type),
        ),
    };
    let (sender, content) = p.split_sender_and_content();
    format!(
        "{kind} sender={sender:?} sender_uuid={:?} is_whisper={} content_chars={} content={:?}{extra}\n      message: {}",
        p.sender_uuid(),
        p.is_whisper(),
        content.chars().count(),
        content.chars().take(80).collect::<String>(),
        describe(&p.message()).chars().take(400).collect::<String>()
    )
}
