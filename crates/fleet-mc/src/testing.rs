//! Packets and frames for the unit tests of what a session's App does with
//! them.

use std::sync::Arc;

use azalea::app::App;
use azalea::core::entity_id::MinecraftEntityId;
use azalea::core::game_type::{GameMode, OptionalGameType};
use azalea::core::position::Vec3;
use azalea::ecs::entity::Entity;
use azalea::entity::LookDirection;
use azalea::packet::game::ReceiveGamePacketEvent;
use azalea::protocol::common::movements::{PositionMoveRotation, RelativeMovements};
use azalea::protocol::packets::common::CommonPlayerSpawnInfo;
use azalea::protocol::packets::game::{
    ClientboundGamePacket, ClientboundLogin, ClientboundPlayerPosition, ClientboundRemoveEntities,
    ClientboundRespawn, ClientboundSetPassengers,
};
use azalea::registry::DataRegistry as _;
use azalea::registry::data::DimensionKind;
use azalea::registry::identifier::Identifier;

/// What a Login and a Respawn say about the world, for `dimension`.
fn spawn_info(dimension: &str) -> CommonPlayerSpawnInfo {
    CommonPlayerSpawnInfo {
        dimension_type: DimensionKind::new_raw(0),
        dimension: Identifier::new(dimension),
        seed: 0,
        game_type: GameMode::Survival,
        previous_game_type: OptionalGameType(None),
        is_debug: false,
        is_flat: true,
        last_death_location: None,
        portal_cooldown: 0,
        sea_level: 63,
    }
}

/// A Login packet for the bot with the server's ID `bot`.
pub(crate) fn login(bot: MinecraftEntityId) -> ClientboundGamePacket {
    ClientboundGamePacket::Login(ClientboundLogin {
        player_id: bot,
        hardcore: false,
        levels: vec![Identifier::new("minecraft:overworld")],
        max_players: 20,
        chunk_radius: 8,
        simulation_distance: 8,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        common: spawn_info("minecraft:overworld"),
        enforces_secure_chat: false,
    })
}

/// A Respawn packet into the nether.
pub(crate) fn respawn() -> ClientboundGamePacket {
    ClientboundGamePacket::Respawn(ClientboundRespawn {
        common: spawn_info("minecraft:the_nether"),
        data_to_keep: 0,
    })
}

/// A server teleport of the bot to `to`, absolute.
pub(crate) fn teleport(to: Vec3) -> ClientboundGamePacket {
    ClientboundGamePacket::PlayerPosition(ClientboundPlayerPosition {
        id: 1,
        change: PositionMoveRotation {
            pos: to,
            delta: Vec3::ZERO,
            look_direction: LookDirection::default(),
        },
        relative: RelativeMovements::all_absolute(),
    })
}

/// A vehicle's list of passengers.
pub(crate) fn passengers(
    vehicle: MinecraftEntityId,
    passengers: &[MinecraftEntityId],
) -> ClientboundGamePacket {
    ClientboundGamePacket::SetPassengers(ClientboundSetPassengers {
        vehicle,
        passengers: passengers.to_vec(),
    })
}

/// The server removed these entities.
pub(crate) fn removed(entities: &[MinecraftEntityId]) -> ClientboundGamePacket {
    ClientboundGamePacket::RemoveEntities(ClientboundRemoveEntities {
        entity_ids: entities.to_vec(),
    })
}

/// Hands `packet` to the App as received by `entity`, as azalea does in
/// `PreUpdate`; the next `update` reads it.
pub(crate) fn receive(app: &mut App, entity: Entity, packet: ClientboundGamePacket) {
    app.world_mut().write_message(ReceiveGamePacketEvent {
        entity,
        packet: Arc::new(packet),
    });
}
