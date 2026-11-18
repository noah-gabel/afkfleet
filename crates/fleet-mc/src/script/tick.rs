//! The session's tick for a scripted mode: where the bot was put, its
//! leash, and azalea's key state (Plan.md P15.10, P15.12; ADR-0036, D9,
//! D10).
//!
//! Three systems run each game tick, in this order and **before azalea's
//! physics**, which read the key state in the same tick:
//!
//! 1. [`place`]: a bot whose teleport came after a login or a respawn, and
//!    whose client has loaded, is placed. Its position is its leash's
//!    anchor, and the bridge hears of it, which is when a world change is
//!    reported.
//! 2. [`leash`]: a bot beyond its radius that got further since the last
//!    tick lets go of every movement key. fleet-core's [`Leash`] decides.
//! 3. [`apply_keys`]: azalea's key state is written from [`Held`], if the
//!    keys changed.
//!
//! [`end_taps`] runs after the physics: the tick that saw a tapped key is
//! over, so the key goes up at the next one.

use azalea::PhysicsState;
use azalea::ecs::query::With;
use azalea::ecs::system::Query;
use azalea::entity::{HasClientLoaded, Jumping, LocalEntity, Position};
use fleet_core::mc::LetGo;
use fleet_core::script::{Leash, LeashTick};
use tracing::debug;

use super::held::Held;
use super::{Arriving, Guarded, Placement, Reports};

/// The bot's feet as fleet-core holds a position; `None` for one that
/// isn't made of numbers, which nothing can be judged by.
#[must_use]
pub(super) fn feet(position: &Position) -> Option<fleet_core::script::Position> {
    fleet_core::script::Position::try_new(position.x, position.y, position.z).ok()
}

/// What [`place`] reads and writes of a bot.
type Arrived = (&'static Reports, &'static mut Placement, &'static Position);

/// A bot whose client has loaded.
type Loaded = (With<LocalEntity>, With<HasClientLoaded>);

/// What [`leash`] reads and writes of a bot.
type Leashed = (
    &'static Reports,
    &'static mut Placement,
    &'static mut Held,
    &'static Guarded,
    &'static Position,
);

/// Places a bot whose teleport has come and whose client has loaded: where
/// it stands is where the server put it.
pub(super) fn place(mut bots: Query<Arrived, Loaded>) {
    for (reports, mut placement, position) in &mut bots {
        if placement.arriving != Arriving::Teleported {
            continue;
        }
        let Some(at) = feet(position) else {
            continue;
        };
        placement.anchor = Some(at);
        placement.last = Some(at);
        placement.arriving = Arriving::Placed;
        debug!("the server placed the bot; its leash is anchored here");
        reports.0.placed();
    }
}

/// Lets go of every movement key of a bot that is beyond its leash's
/// radius and got further since the last game tick.
///
/// What moved the bot doesn't matter, and a bot that holds no movement key
/// has nothing to let go of: the leash stops walking, not being carried.
pub(super) fn leash(mut bots: Query<Leashed, With<LocalEntity>>) {
    for (reports, mut placement, mut held, guarded, position) in &mut bots {
        let (Some(anchor), Some(now)) = (placement.anchor, feet(position)) else {
            continue;
        };
        let before = placement.last.unwrap_or(now);
        placement.last = Some(now);
        if Leash::new(guarded.0.leash, anchor).tick(before, now) == LeashTick::Stop
            && held.release_movement()
        {
            debug!("the bot is at the end of its leash; its movement keys are up");
            reports.0.let_go(LetGo::Leash);
        }
    }
}

/// Writes azalea's key state from what's held, if that changed: where the
/// bot walks, and whether it sprints, sneaks and jumps.
pub(super) fn apply_keys(
    mut bots: Query<(&mut Held, &mut PhysicsState, &mut Jumping), With<LocalEntity>>,
) {
    for (mut held, mut state, mut jumping) in &mut bots {
        if !held.changed {
            continue;
        }
        held.changed = false;
        let keys = held.key_state();
        state.move_direction = keys.walk;
        state.trying_to_sprint = keys.sprint;
        state.trying_to_crouch = keys.crouch;
        if **jumping != keys.jump {
            **jumping = keys.jump;
        }
    }
}

/// Ends the taps: the game tick that saw them is over.
pub(super) fn end_taps(mut bots: Query<&mut Held, With<LocalEntity>>) {
    for mut held in &mut bots {
        // Not through `DerefMut` unless something was tapped: nothing
        // changed for a bot that tapped nothing.
        if !held.taps.is_empty() {
            held.end_taps();
        }
    }
}
