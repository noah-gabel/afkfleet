//! Game actions with azalea (Plan.md P3.6; ADR-0008 §8, ADR-0011).
//!
//! [`perform`] and [`respawn`] run on the session's host thread, inside one
//! synchronous job each. Several of azalea's `Client` methods panic when a
//! component is missing, which happens once the bot has left its world, so
//! every component an action needs is checked first; a missing one is
//! [`SessionError::NotInWorld`]. No ECS guard is held while a `Client` method
//! runs, since azalea's locks aren't reentrant.
//!
//! The pure parts are tested here: the direction after a turn, the check for
//! non-finite angles, and the attack packet's bytes. The slow suite checks
//! every action's effect on a real server.

use azalea::attack::TicksSinceLastAttack;
use azalea::connection::RawConnection;
use azalea::ecs::component::Component;
use azalea::entity::LookDirection;
use azalea::entity::indexing::EntityIdIndex;
use azalea::interact::SwingArmEvent;
use azalea::interact::pick::HitResultComponent;
use azalea::protocol::packets::ProtocolPacket as _;
use azalea::protocol::packets::game::{ServerboundAttack, ServerboundGamePacket};
use azalea::respawn::PerformRespawnEvent;
use azalea::{Client, PhysicsState};
use fleet_core::mc::SessionError;
use fleet_core::mode::GameAction;
use tracing::debug;

/// A look direction, in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Direction {
    /// Turning right is positive; 0 faces south.
    pub(crate) yaw: f32,
    /// −90 is straight up, 90 straight down.
    pub(crate) pitch: f32,
}

/// The direction after turning `current` by `delta`: the yaw wrapped to
/// −180..180, the pitch clamped to −90..=90 (P3.6).
#[must_use]
pub(crate) fn turned(current: Direction, delta: Direction) -> Direction {
    Direction {
        yaw: (current.yaw + delta.yaw + 180.0).rem_euclid(360.0) - 180.0,
        pitch: (current.pitch + delta.pitch).clamp(-90.0, 90.0),
    }
}

/// Whether every angle of `action` is finite. Mode validation already
/// refuses others; the session skips them as a safety net (ADR-0010).
#[must_use]
pub(crate) fn has_finite_angles(action: &GameAction) -> bool {
    match action {
        GameAction::Look { yaw, pitch } | GameAction::Turn { yaw, pitch } => {
            yaw.is_finite() && pitch.is_finite()
        }
        GameAction::Jump
        | GameAction::Sneak { .. }
        | GameAction::SwingArm
        | GameAction::UseItem
        | GameAction::AttackFacingEntity
        | GameAction::SelectHotbarSlot { .. } => true,
    }
}

/// The bytes of a serverbound attack packet: the packet ID and the target's
/// entity ID, each a `VarInt`.
///
/// azalea 0.16.0 writes the entity ID as a fixed-width integer, which 26.1
/// servers kick the bot for, so fleet-mc writes the packet itself (ADR-0008
/// §8). Re-checked at every azalea bump (ADR-0003).
#[must_use]
pub(crate) fn attack_packet(packet_id: u32, entity_id: i32) -> Vec<u8> {
    let mut packet = Vec::with_capacity(10);
    write_var_int(&mut packet, packet_id);
    // A VarInt of the ID's two's-complement bits, as the protocol wants it.
    write_var_int(&mut packet, u32::from_ne_bytes(entity_id.to_ne_bytes()));
    packet
}

/// Appends `value` as a Minecraft `VarInt`: seven bits a byte, low bits first.
fn write_var_int(buf: &mut Vec<u8>, mut value: u32) {
    loop {
        let low = u8::try_from(value & 0x7f).unwrap_or_default();
        value >>= 7;
        if value == 0 {
            buf.push(low);
            return;
        }
        buf.push(low | 0x80);
    }
}

/// Performs `action` with `client`, on the host thread.
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot has left its world: a component
/// the action needs is gone, or its connection is.
pub(crate) fn perform(client: &Client, action: GameAction) -> Result<(), SessionError> {
    match action {
        GameAction::Look { yaw, pitch } => look(client, Direction { yaw, pitch }),
        GameAction::Turn { yaw, pitch } => {
            let current = client
                .get_component::<LookDirection>()
                .map(|direction| Direction {
                    yaw: direction.y_rot(),
                    pitch: direction.x_rot(),
                })
                .ok_or(SessionError::NotInWorld)?;
            look(client, turned(current, Direction { yaw, pitch }))
        }
        // These only queue a message for the next tick.
        GameAction::Jump => {
            client.jump();
            Ok(())
        }
        GameAction::UseItem => {
            client.start_use_item();
            Ok(())
        }
        GameAction::Sneak { on } => {
            require::<PhysicsState>(client)?;
            client.set_crouching(on);
            Ok(())
        }
        GameAction::SwingArm => {
            swing(client);
            Ok(())
        }
        // The slot type keeps it to 0..=8; azalea panics above (ADR-0008 §8).
        GameAction::SelectHotbarSlot { slot } => {
            client.set_selected_hotbar_slot(slot.get());
            Ok(())
        }
        GameAction::AttackFacingEntity => attack(client),
    }
}

/// Respawns the bot after it died, on the host thread. azalea has no
/// `Client` method for it (ADR-0008 §8).
pub(crate) fn respawn(client: &Client) {
    client.ecs.write().write_message(PerformRespawnEvent {
        entity: client.entity,
    });
}

/// Fails with [`SessionError::NotInWorld`] if the bot lacks a `T`, which
/// some of azalea's `Client` methods would panic on. The read guard is
/// dropped before this returns.
fn require<T: Component>(client: &Client) -> Result<(), SessionError> {
    if client.get_component::<T>().is_some() {
        Ok(())
    } else {
        Err(SessionError::NotInWorld)
    }
}

fn look(client: &Client, direction: Direction) -> Result<(), SessionError> {
    require::<LookDirection>(client)?;
    client.set_direction(direction.yaw, direction.pitch);
    Ok(())
}

/// azalea has no `Client` method for a swing (ADR-0008 §8).
fn swing(client: &Client) {
    client.ecs.write().trigger(SwingArmEvent {
        entity: client.entity,
    });
}

/// Attacks the entity the bot looks at, if one is within reach: azalea's
/// target already respects the reach (ADR-0008 §8). Nothing in reach is no
/// error, just nothing to do.
fn attack(client: &Client) -> Result<(), SessionError> {
    let target = client
        .get_component::<HitResultComponent>()
        .ok_or(SessionError::NotInWorld)?
        .as_entity_hit_result()
        .map(|hit| hit.entity);
    let Some(target) = target else {
        debug!("nothing to attack within reach");
        return Ok(());
    };
    let entity_id = client
        .get_component::<EntityIdIndex>()
        .ok_or(SessionError::NotInWorld)?
        .get_by_ecs_entity(target);
    let Some(entity_id) = entity_id else {
        debug!("the target has no server-side ID yet");
        return Ok(());
    };
    let packet = attack_packet(
        ServerboundGamePacket::Attack(ServerboundAttack { entity_id }).id(),
        entity_id.0,
    );
    let written = client.try_query_self::<&mut RawConnection, _>(|mut connection| {
        connection.net_conn().map(|net| net.write_raw(&packet))
    });
    match written {
        Ok(Some(Ok(()))) => {}
        Ok(Some(Err(error))) => {
            debug!(%error, "the attack packet couldn't be written");
            return Err(SessionError::NotInWorld);
        }
        Ok(None) | Err(_) => return Err(SessionError::NotInWorld),
    }
    // What azalea's own attack does besides the packet.
    swing(client);
    let _ = client.try_query_self::<&mut TicksSinceLastAttack, _>(|mut ticks| **ticks = 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_core::mode::HotbarSlot;
    use rstest::rstest;

    const fn dir(yaw: f32, pitch: f32) -> Direction {
        Direction { yaw, pitch }
    }

    #[rstest]
    #[case::plain(dir(10.0, 5.0), dir(20.0, -15.0), dir(30.0, -10.0))]
    #[case::yaw_wraps_past_180(dir(170.0, 0.0), dir(20.0, 0.0), dir(-170.0, 0.0))]
    #[case::yaw_wraps_past_minus_180(dir(-170.0, 0.0), dir(-20.0, 0.0), dir(170.0, 0.0))]
    #[case::yaw_180_is_minus_180(dir(179.5, 0.0), dir(0.5, 0.0), dir(-180.0, 0.0))]
    #[case::a_full_turn_ends_where_it_started(dir(45.0, 0.0), dir(360.0, 0.0), dir(45.0, 0.0))]
    #[case::many_turns(dir(0.0, 0.0), dir(-1090.0, 0.0), dir(-10.0, 0.0))]
    #[case::pitch_clamps_down(dir(0.0, 80.0), dir(0.0, 30.0), dir(0.0, 90.0))]
    #[case::pitch_clamps_up(dir(0.0, -80.0), dir(0.0, -30.0), dir(0.0, -90.0))]
    fn a_turn_wraps_the_yaw_and_clamps_the_pitch(
        #[case] current: Direction,
        #[case] delta: Direction,
        #[case] expected: Direction,
    ) {
        assert_eq!(turned(current, delta), expected);
    }

    #[rstest]
    #[case::look_nan_yaw(GameAction::Look { yaw: f32::NAN, pitch: 0.0 })]
    #[case::look_infinite_pitch(GameAction::Look { yaw: 0.0, pitch: f32::INFINITY })]
    #[case::turn_nan_pitch(GameAction::Turn { yaw: 0.0, pitch: f32::NAN })]
    #[case::turn_infinite_yaw(GameAction::Turn { yaw: f32::NEG_INFINITY, pitch: 0.0 })]
    fn a_non_finite_angle_is_found(#[case] action: GameAction) {
        assert!(!has_finite_angles(&action));
    }

    #[rstest]
    #[case::look(GameAction::Look { yaw: -180.0, pitch: 90.0 })]
    #[case::turn(GameAction::Turn { yaw: 1000.0, pitch: -1000.0 })]
    #[case::jump(GameAction::Jump)]
    #[case::sneak(GameAction::Sneak { on: true })]
    #[case::swing(GameAction::SwingArm)]
    #[case::use_item(GameAction::UseItem)]
    #[case::attack(GameAction::AttackFacingEntity)]
    #[case::hotbar(GameAction::SelectHotbarSlot { slot: HotbarSlot::LAST })]
    fn finite_and_angle_free_actions_pass(#[case] action: GameAction) {
        assert!(has_finite_angles(&action));
    }

    #[rstest]
    #[case::zero(0x10, 0, &[0x10, 0x00])]
    #[case::one_byte_max(0x10, 127, &[0x10, 0x7f])]
    #[case::two_bytes(0x10, 128, &[0x10, 0x80, 0x01])]
    #[case::large(0x10, 300_000, &[0x10, 0xe0, 0xa7, 0x12])]
    #[case::negative_is_five_bytes(0x10, -1, &[0x10, 0xff, 0xff, 0xff, 0xff, 0x0f])]
    #[case::two_byte_packet_id(300, 1, &[0xac, 0x02, 0x01])]
    fn the_attack_packet_is_two_var_ints(
        #[case] packet_id: u32,
        #[case] entity_id: i32,
        #[case] expected: &[u8],
    ) {
        assert_eq!(attack_packet(packet_id, entity_id), expected);
    }
}
