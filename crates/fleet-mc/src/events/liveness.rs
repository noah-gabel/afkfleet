//! [`LivenessStamps`]: when a session last ticked and last heard from the
//! server, and [`PacketLivenessPlugin`], which stamps every received packet.

use core::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use azalea::app::{App, Plugin, Update};
use azalea::ecs::message::MessageReader;
use azalea::packet::game::ReceiveGamePacketEvent;
use fleet_core::mc::Liveness;

/// A session's liveness stamps (ADR-0008 §4–5, ADR-0010), updated in place
/// instead of queueing an event per tick or packet.
///
/// Both start at creation, so they're never empty, and they never move
/// backwards. They're stored as offsets from that start, so the host thread
/// writes them without a lock.
#[derive(Debug)]
pub(crate) struct LivenessStamps {
    /// When the session was created.
    epoch: Instant,
    /// Nanoseconds from `epoch` to the last tick.
    last_tick: AtomicU64,
    /// Nanoseconds from `epoch` to the last packet.
    last_packet: AtomicU64,
}

impl LivenessStamps {
    /// Creates the stamps, both set to `now`.
    pub(crate) const fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            last_tick: AtomicU64::new(0),
            last_packet: AtomicU64::new(0),
        }
    }

    /// Records a game tick at `now`.
    pub(crate) fn stamp_tick(&self, now: Instant) {
        // `fetch_max` keeps the latest stamp, whatever order writes land in.
        // Each stamp is a value of its own, so no ordering beyond the atomic
        // itself is needed.
        self.last_tick
            .fetch_max(self.offset(now), Ordering::Relaxed);
    }

    /// Records a packet from the server at `now`.
    pub(crate) fn stamp_packet(&self, now: Instant) {
        self.last_packet
            .fetch_max(self.offset(now), Ordering::Relaxed);
    }

    /// Reads both stamps.
    pub(crate) fn read(&self) -> Liveness {
        Liveness {
            last_tick: self.instant(self.last_tick.load(Ordering::Relaxed)),
            last_packet: self.instant(self.last_packet.load(Ordering::Relaxed)),
        }
    }

    /// `now` as nanoseconds from the epoch; a time before it counts as the
    /// epoch, and one more than 584 years later as the maximum.
    fn offset(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.epoch).as_nanos()).unwrap_or(u64::MAX)
    }

    /// The instant `offset` nanoseconds after the epoch.
    fn instant(&self, offset: u64) -> Instant {
        self.epoch
            .checked_add(Duration::from_nanos(offset))
            .unwrap_or(self.epoch)
    }
}

/// Stamps the session's packet liveness for every packet it receives,
/// through azalea's `ReceiveGamePacketEvent`, so azalea's `packet-event`
/// feature stays off (ADR-0011). One App runs one session, so its system
/// holds that session's stamps.
pub(crate) struct PacketLivenessPlugin(pub(crate) Arc<LivenessStamps>);

impl Plugin for PacketLivenessPlugin {
    fn build(&self, app: &mut App) {
        let stamps = Arc::clone(&self.0);
        // azalea registers the message too; registering it again is a no-op,
        // and it lets the plugin work on its own. The system runs in
        // `Update`, after azalea read the packets in `PreUpdate`. Messages
        // live for two frames and `Update` runs every frame, so none is
        // missed.
        app.add_message::<ReceiveGamePacketEvent>().add_systems(
            Update,
            move |mut packets: MessageReader<ReceiveGamePacketEvent>| {
                // Any packet is a sign of life; reading them all marks them
                // as seen.
                if packets.read().count() > 0 {
                    stamps.stamp_packet(Instant::now());
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea::ecs::entity::Entity;
    use azalea::protocol::packets::game::{ClientboundGamePacket, ClientboundKeepAlive};

    /// A start a minute in the past, so a stamp taken now is later.
    fn past() -> Instant {
        Instant::now().checked_sub(Duration::from_secs(60)).unwrap()
    }

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn both_stamps_start_at_creation() {
        let start = past();

        let liveness = LivenessStamps::new(start).read();

        assert_eq!(liveness.last_tick, start);
        assert_eq!(liveness.last_packet, start);
    }

    #[test]
    fn tick_moves_only_the_tick_stamp() {
        let start = past();
        let stamps = LivenessStamps::new(start);

        stamps.stamp_tick(at(start, 5));

        assert_eq!(stamps.read().last_tick, at(start, 5));
        assert_eq!(stamps.read().last_packet, start);
    }

    #[test]
    fn packet_moves_only_the_packet_stamp() {
        let start = past();
        let stamps = LivenessStamps::new(start);

        stamps.stamp_packet(at(start, 7));

        assert_eq!(stamps.read().last_packet, at(start, 7));
        assert_eq!(stamps.read().last_tick, start);
    }

    #[test]
    fn stamps_never_move_backwards() {
        let start = past();
        let stamps = LivenessStamps::new(at(start, 10));

        stamps.stamp_tick(at(start, 15));
        stamps.stamp_tick(at(start, 12));
        stamps.stamp_packet(start);

        assert_eq!(stamps.read().last_tick, at(start, 15));
        assert_eq!(stamps.read().last_packet, at(start, 10));
    }

    /// An App with only the plugin, and the stamps it writes.
    fn app() -> (App, Arc<LivenessStamps>, Instant) {
        let start = past();
        let stamps = Arc::new(LivenessStamps::new(start));
        let mut app = App::new();
        app.add_plugins(PacketLivenessPlugin(Arc::clone(&stamps)));
        (app, stamps, start)
    }

    #[test]
    fn received_packets_stamp_only_the_packet_time() {
        let (mut app, stamps, start) = app();
        app.world_mut().write_message(ReceiveGamePacketEvent {
            entity: Entity::PLACEHOLDER,
            packet: Arc::new(ClientboundGamePacket::KeepAlive(ClientboundKeepAlive {
                id: 1,
            })),
        });

        app.update();

        assert!(stamps.read().last_packet > start);
        assert_eq!(stamps.read().last_tick, start);
    }

    #[test]
    fn frames_without_packets_leave_the_packet_time() {
        let (mut app, stamps, start) = app();

        app.update();

        assert_eq!(stamps.read().last_packet, start);
    }
}
