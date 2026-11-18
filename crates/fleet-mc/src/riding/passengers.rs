//! [`Passengers`]: which vehicles near the bot someone sits in (Plan.md
//! P15.13; ADR-0036, D12).
//!
//! azalea doesn't track other entities' passengers (ADR-0026), so a bot
//! can't tell an occupied minecart from a free one. The server says it in
//! `SetPassengers`, which lists all of a vehicle's passengers at each
//! change; this index keeps, for the bot, the vehicles whose list isn't
//! empty.
//!
//! **It stays bounded,** whatever a server sends:
//!
//! - An entry is a vehicle the bot knows: a list for an entity the server
//!   never showed the bot is ignored.
//! - An entry goes when its vehicle's list is empty, and when the server
//!   removes the vehicle.
//! - The whole index goes at a login and at a respawn (another world), and
//!   with the bot's entity when its session ends.
//! - At [`Passengers::MOST`] entries it's swept of vehicles the bot no
//!   longer knows, and if it's still full, a new vehicle isn't taken.

use std::collections::HashSet;

use azalea::core::entity_id::MinecraftEntityId;
// The `Component` derive names its crate as `bevy_ecs`, which fleet-mc only
// has through azalea.
use azalea::ecs as bevy_ecs;
use azalea::ecs::component::Component;
use azalea::protocol::packets::game::ClientboundGamePacket;

/// On the bot: the vehicles someone sits in, by the server's ID.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Passengers {
    occupied: HashSet<MinecraftEntityId>,
}

impl Passengers {
    /// The most vehicles the index holds: far more than a bot sees at
    /// once, and little memory.
    pub(crate) const MOST: usize = 512;

    /// Whether someone sits in `vehicle`.
    #[must_use]
    pub(crate) fn is_occupied(&self, vehicle: MinecraftEntityId) -> bool {
        self.occupied.contains(&vehicle)
    }

    /// How many vehicles the index holds.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.occupied.len()
    }

    /// Takes in what `packet` says. `knows` says whether the bot knows an
    /// entity by the server's ID.
    pub(crate) fn read(
        &mut self,
        packet: &ClientboundGamePacket,
        knows: impl Fn(MinecraftEntityId) -> bool,
    ) {
        match packet {
            ClientboundGamePacket::SetPassengers(list) if list.passengers.is_empty() => {
                self.occupied.remove(&list.vehicle);
            }
            ClientboundGamePacket::SetPassengers(list) => self.take(list.vehicle, knows),
            ClientboundGamePacket::RemoveEntities(removed) => {
                for entity in &removed.entity_ids {
                    self.occupied.remove(entity);
                }
            }
            // Another world: none of these vehicles is in it.
            ClientboundGamePacket::Login(_) | ClientboundGamePacket::Respawn(_) => {
                self.occupied.clear();
            }
            _ => {}
        }
    }

    /// Notes that someone sits in `vehicle`, if the bot knows it and the
    /// index has room, or makes room by forgetting what the bot no longer
    /// knows.
    fn take(&mut self, vehicle: MinecraftEntityId, knows: impl Fn(MinecraftEntityId) -> bool) {
        if self.occupied.contains(&vehicle) || !knows(vehicle) {
            return;
        }
        if self.occupied.len() >= Self::MOST {
            self.occupied.retain(|known| knows(*known));
        }
        if self.occupied.len() < Self::MOST {
            self.occupied.insert(vehicle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{login, passengers, removed, respawn};

    const BOT: MinecraftEntityId = MinecraftEntityId(1);
    const CART: MinecraftEntityId = MinecraftEntityId(2);
    const OTHER_CART: MinecraftEntityId = MinecraftEntityId(3);
    const RIDER: MinecraftEntityId = MinecraftEntityId(9);

    /// An index that knows every entity and read `packets`.
    fn after(packets: &[ClientboundGamePacket]) -> Passengers {
        let mut index = Passengers::default();
        for packet in packets {
            index.read(packet, |_| true);
        }
        index
    }

    #[test]
    fn a_vehicle_with_a_passenger_is_occupied_and_another_isnt() {
        let index = after(&[passengers(CART, &[RIDER])]);

        assert!(index.is_occupied(CART));
        assert!(!index.is_occupied(OTHER_CART));
        assert!(!index.is_occupied(RIDER));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn a_vehicle_the_bot_itself_sits_in_is_occupied() {
        let index = after(&[passengers(CART, &[BOT])]);

        assert!(index.is_occupied(CART));
    }

    #[test]
    fn an_empty_list_frees_the_vehicle_and_drops_its_entry() {
        let index = after(&[passengers(CART, &[RIDER, BOT]), passengers(CART, &[])]);

        assert!(!index.is_occupied(CART));
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn a_list_replaces_the_one_before_it() {
        let index = after(&[
            passengers(CART, &[RIDER]),
            passengers(CART, &[BOT]),
            passengers(OTHER_CART, &[RIDER]),
        ]);

        assert!(index.is_occupied(CART) && index.is_occupied(OTHER_CART));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn an_entry_goes_when_its_vehicle_is_removed() {
        let index = after(&[
            passengers(CART, &[RIDER]),
            passengers(OTHER_CART, &[BOT]),
            removed(&[CART, RIDER]),
        ]);

        assert!(!index.is_occupied(CART));
        assert!(index.is_occupied(OTHER_CART));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn the_whole_index_goes_at_a_world_change() {
        for packet in [login(BOT), respawn()] {
            let index = after(&[
                passengers(CART, &[RIDER]),
                passengers(OTHER_CART, &[BOT]),
                packet,
            ]);

            assert_eq!(index.len(), 0);
            assert!(!index.is_occupied(CART));
        }
    }

    #[test]
    fn a_list_for_a_vehicle_the_bot_doesnt_know_is_ignored() {
        let mut index = Passengers::default();

        index.read(&passengers(CART, &[RIDER]), |id| id != CART);
        index.read(&passengers(OTHER_CART, &[RIDER]), |id| id != CART);

        assert!(!index.is_occupied(CART));
        assert!(index.is_occupied(OTHER_CART));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn a_server_that_lists_vehicles_without_end_never_grows_it_past_its_bound() {
        let mut index = Passengers::default();

        // Every one of them known when its list comes, none ever removed.
        for id in 10..10_000 {
            index.read(&passengers(MinecraftEntityId(id), &[RIDER]), |_| true);
        }

        assert_eq!(index.len(), Passengers::MOST);
        // The first ones are kept; a new one isn't taken while it's full.
        assert!(index.is_occupied(MinecraftEntityId(10)));
        assert!(!index.is_occupied(MinecraftEntityId(9_999)));
    }

    #[test]
    fn a_full_index_is_swept_of_vehicles_the_bot_no_longer_knows() {
        let mut index = Passengers::default();
        for id in 10..10 + i32::try_from(Passengers::MOST).unwrap() {
            index.read(&passengers(MinecraftEntityId(id), &[RIDER]), |_| true);
        }
        assert_eq!(index.len(), Passengers::MOST);

        // By now the bot knows only the newest hundred of them, and the
        // one that comes.
        let known = |id: MinecraftEntityId| id.0 >= 422;
        index.read(&passengers(MinecraftEntityId(5_000), &[RIDER]), known);

        assert_eq!(index.len(), 101);
        assert!(index.is_occupied(MinecraftEntityId(5_000)));
        assert!(index.is_occupied(MinecraftEntityId(521)));
        assert!(!index.is_occupied(MinecraftEntityId(10)));
    }

    #[test]
    fn other_packets_change_nothing() {
        let mut index = after(&[passengers(CART, &[RIDER])]);
        let before = index.clone();

        index.read(&crate::testing::teleport(azalea::Vec3::ZERO), |_| true);
        index.read(&removed(&[OTHER_CART]), |_| true);
        index.read(&passengers(OTHER_CART, &[]), |_| true);

        assert_eq!(index, before);
    }
}
