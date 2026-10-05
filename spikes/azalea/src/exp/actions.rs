//! P1.5: actions. `cargo run -- actions` and `cargo run -- idle-actions`.
//!
//! Every action is sent to the host thread through the job channel (the
//! fleet-mc design) and checked, where possible, against the server's view
//! via RCON.

use std::{sync::atomic::Ordering, time::Duration};

use azalea::{
    Client, Event, Vec3, account::Account, interact::SwingArmEvent,
    protocol::packets::game::ClientboundGamePacket, respawn::PerformRespawnEvent,
};
use tokio::{runtime, time::sleep};
use tracing::info;

use crate::{
    DEV_SERVER, Res,
    exp::{fail::observe, host::wait_for},
    host::Host,
    rcon::rcon,
    session::{self, Session, Variant},
};

/// Runs `f(client)` on the host thread.
async fn on_host<T: Send + 'static>(
    host: &Host,
    client: &Client,
    f: impl FnOnce(&Client) -> T + Send + 'static,
) -> Res<T> {
    let c = client.clone();
    host.run(move || async move { f(&c) }).await
}

async fn connect(host: &Host, name: &'static str) -> Res<Session> {
    let mut s = host
        .run(move || {
            session::connect(
                Variant::Custom,
                Account::offline(name),
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
    Ok(s)
}

const TICK: Duration = Duration::from_millis(50);

pub fn run(_args: &[String]) -> Res {
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let host = Host::spawn("mc-host-0")?;
        let bot = connect(&host, "AfkBot1").await?;
        let mut observer = connect(&host, "AfkBot2").await?;
        let c = &bot.client;
        sleep(Duration::from_secs(1)).await;

        // --- look
        on_host(&host, c, |c| c.set_direction(45.0, -10.0)).await?;
        sleep(TICK * 4).await;
        let dir = on_host(&host, c, Client::direction).await?;
        info!(client = ?dir, server = %rcon("data get entity AfkBot1 Rotation")?, "look: set_direction(45, -10)");
        on_host(&host, c, |c| c.look_at(Vec3::new(0.5, -59.0, 100.0))).await?;
        sleep(TICK * 4).await;
        info!(client = ?on_host(&host, c, Client::direction).await?, "look: look_at(0.5, -59, 100)");

        // --- jump
        let y0 = on_host(&host, c, |c| c.position().y).await?;
        on_host(&host, c, Client::jump).await?;
        let mut ys = Vec::new();
        for _ in 0..10 {
            sleep(TICK).await;
            ys.push(on_host(&host, c, |c| c.position().y).await? - y0);
        }
        info!(?ys, "jump: y offset per tick after jump()");

        // --- sneak
        on_host(&host, c, |c| c.set_crouching(true)).await?;
        sleep(TICK * 3).await;
        info!(crouching = on_host(&host, c, Client::crouching).await?, "sneak: set_crouching(true)");
        on_host(&host, c, |c| c.set_crouching(false)).await?;

        // --- swing (seen by the observer as an Animate packet)
        observer.ticks.forward_packets.store(true, Ordering::Relaxed);
        while observer.events.try_recv().is_ok() {}
        on_host(&host, c, |c| {
            let entity = c.entity;
            c.ecs.write().trigger(SwingArmEvent { entity });
        })
        .await?;
        let mut animates = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(e) = observer.events.recv().await {
                if let Event::Packet(p) = e
                    && let ClientboundGamePacket::Animate(a) = p.as_ref()
                {
                    animates.push(format!("{:?}", a.action));
                }
            }
        })
        .await;
        observer.ticks.forward_packets.store(false, Ordering::Relaxed);
        info!(?animates, "swing: Animate packets the other bot saw");

        // --- hotbar
        rcon("item replace entity AfkBot1 hotbar.3 with minecraft:stick")?;
        on_host(&host, c, |c| c.set_selected_hotbar_slot(3)).await?;
        sleep(TICK * 4).await;
        info!(
            client = on_host(&host, c, Client::selected_hotbar_slot).await?,
            server = %rcon("execute if items entity AfkBot1 weapon.mainhand minecraft:stick")?,
            "hotbar: set_selected_hotbar_slot(3), stick in hotbar.3"
        );

        // --- use item (throw a snowball)
        rcon("item replace entity AfkBot1 hotbar.4 with minecraft:snowball 16")?;
        on_host(&host, c, |c| c.set_selected_hotbar_slot(4)).await?;
        sleep(TICK * 4).await;
        on_host(&host, c, Client::start_use_item).await?;
        sleep(TICK * 6).await;
        info!(server = %rcon("clear AfkBot1 minecraft:snowball 0")?, "use item: snowballs left (16 before)");

        // --- attack the entity in view
        on_host(&host, c, |c| c.set_selected_hotbar_slot(0)).await?;
        on_host(&host, c, |c| c.set_direction(0.0, 0.0)).await?;
        sleep(TICK * 4).await;
        rcon("kill @e[type=minecraft:pig]")?;
        rcon("execute at AfkBot1 rotated ~ 0 positioned ^ ^ ^2 run summon minecraft:pig ~ ~ ~ {NoAI:1b,Tags:[\"spike\"]}")?;
        sleep(Duration::from_millis(500)).await;
        let (pos, reach) = on_host(&host, c, |c| {
            (c.position(), c.attributes().entity_interaction_range.calculate())
        })
        .await?;
        // yaw 0 faces +z; aim at the pig's body, 2 blocks ahead.
        on_host(&host, c, move |c| c.look_at(Vec3::new(pos.x, pos.y + 0.45, pos.z + 2.0))).await?;
        sleep(TICK * 3).await;
        let hit = on_host(&host, c, |c| c.hit_result()).await?;
        let target = hit.as_entity_hit_result().map(|h| h.entity);
        info!(?reach, hit_is_entity = target.is_some(), "attack: hit_result after look_at(pig)");
        let health_before = rcon("data get entity @e[type=minecraft:pig,tag=spike,limit=1] Health")?;
        if let Some(entity) = target {
            on_host(&host, c, move |c| attack_raw(c, entity)).await??;
            let cooldown = on_host(&host, c, |c| (c.has_attack_cooldown(), c.attack_cooldown_remaining_ticks())).await?;
            sleep(TICK * 4).await;
            info!(
                %health_before,
                health_after = %rcon("data get entity @e[type=minecraft:pig,tag=spike,limit=1] Health")?,
                ?cooldown,
                "attack: attack_raw(entity) (workaround)"
            );
        }
        // Out of reach: what does hit_result give for an entity 6 blocks away?
        rcon("kill @e[type=minecraft:pig]")?;
        rcon("execute at AfkBot1 rotated ~ 0 positioned ^ ^ ^6 run summon minecraft:pig ~ ~ ~ {NoAI:1b,Tags:[\"spike\"]}")?;
        sleep(Duration::from_millis(500)).await;
        on_host(&host, c, move |c| c.look_at(Vec3::new(pos.x, pos.y + 0.45, pos.z + 6.0))).await?;
        sleep(TICK * 3).await;
        let far = on_host(&host, c, |c| c.hit_result().as_entity_hit_result().is_some()).await?;
        info!(hit_is_entity = far, "attack: hit_result for a pig 6 blocks away");
        rcon("kill @e[type=minecraft:pig]")?;

        // --- respawn
        let mut bot = bot;
        rcon("kill AfkBot1")?;
        wait_for(&mut bot, |e| matches!(e, Event::Death(_)), Duration::from_secs(5)).await?;
        sleep(Duration::from_secs(1)).await;
        let dead = on_host(&host, &bot.client, Client::health).await?;
        on_host(&host, &bot.client, |c| {
            let entity = c.entity;
            c.ecs.write().write_message(PerformRespawnEvent { entity });
        })
        .await?;
        sleep(Duration::from_secs(1)).await;
        info!(
            health_before = dead,
            health_after = on_host(&host, &bot.client, Client::health).await?,
            "respawn: PerformRespawnEvent"
        );

        bot.client.exit();
        observer.client.exit();
        Ok(())
    })
}

/// `idle-actions`: which actions reset the server's idle timer? With
/// `setidletimeout 1`, each bot repeats one action every 20 s; the ones that
/// get kicked within 100 s don't count as activity.
pub fn idle(_args: &[String]) -> Res {
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        rcon("setidletimeout 1")?;
        let host = Host::spawn("mc-host-0")?;
        let names = ["AfkBot1", "AfkBot2", "AfkBot3", "AfkBot4", "AfkBot5", "AfkBot6"];
        let labels = ["none", "rotate", "swing", "jump", "sneak", "hotbar"];
        let mut bots = Vec::new();
        for name in names {
            bots.push(connect(&host, name).await?);
        }
        for round in 0..5u8 {
            for (i, s) in bots.iter().enumerate() {
                let c = s.client.clone();
                let r = f32::from(round);
                let _ = host
                    .run(move || async move {
                        match i {
                            1 => c.set_direction(r * 10.0, 0.0),
                            2 => {
                                let entity = c.entity;
                                c.ecs.write().trigger(SwingArmEvent { entity });
                            }
                            3 => c.jump(),
                            4 => c.set_crouching(round % 2 == 0),
                            5 => c.set_selected_hotbar_slot(round % 9),
                            _ => {}
                        }
                    })
                    .await;
            }
            sleep(Duration::from_secs(20)).await;
        }
        for (label, mut s) in labels.into_iter().zip(bots) {
            let n = observe(&mut s, 0).await;
            let kicked = s.events.try_recv().is_err() && n == 0;
            info!(action = label, still_online = %rcon("list")?.contains(&s.name), events_left = n, kicked_probe = kicked, "after 100 s");
            s.client.exit();
        }
        rcon("setidletimeout 0")?;
        Ok(())
    })
}

/// Workaround for azalea 0.16.0's `ServerboundAttack`, which encodes the entity
/// id as a fixed 4-byte int where 26.1 expects a VarInt (the server kicks with
/// "found 3 bytes extra"). Writes the packet by hand: VarInt packet id +
/// VarInt entity id, then swings like azalea's own attack system. Must run on
/// the host thread.
pub fn attack_raw(c: &Client, target: azalea::ecs::entity::Entity) -> Result<(), String> {
    use azalea::{
        entity::indexing::EntityIdIndex,
        protocol::packets::{
            ProtocolPacket,
            game::{ServerboundAttack, ServerboundGamePacket},
        },
    };
    let entity_id = {
        let ecs = c.ecs.read();
        let index = ecs
            .get::<EntityIdIndex>(c.entity)
            .ok_or("no EntityIdIndex")?;
        index
            .get_by_ecs_entity(target)
            .ok_or("target not in EntityIdIndex")?
    };
    let packet_id = ServerboundGamePacket::Attack(ServerboundAttack { entity_id }).id();
    let mut raw = Vec::with_capacity(10);
    write_var_int(&mut raw, packet_id);
    write_var_int(&mut raw, u32::from_ne_bytes(entity_id.0.to_ne_bytes()));
    c.with_raw_connection_mut(|mut conn| match conn.net_conn() {
        Some(net) => net
            .write_raw(&raw)
            .map_err(|e| format!("write_raw failed: {e}")),
        None => Err("not connected".to_owned()),
    })?;
    let entity = c.entity;
    c.ecs.write().trigger(SwingArmEvent { entity });
    Ok(())
}

/// Minecraft VarInt (LEB128 over the two's-complement bits).
fn write_var_int(buf: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = u8::try_from(value & 0x7F).unwrap_or(0);
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}
