//! The one description of the script API: every object, each of its
//! members, the hooks and the reasons (ADR-0036, D4).
//!
//! The reference's tables and the editor's autocomplete are made from
//! this; a test holds it equal to what the engine registers, so neither
//! can name something a script doesn't have.

use fleet_core::script::{Hook, Refusal};

/// What a member of an object is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemberKind {
    /// A function called with parentheses: `keys.jump.tap()`.
    Method,
    /// A value read without them: `bot.health`.
    Property,
    /// A function a script defines and the runtime calls.
    Hook,
}

/// A member of an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    /// Its name.
    pub name: &'static str,
    /// What it is.
    pub kind: MemberKind,
    /// How it's written, with its parameters: `hold_for(ms)`.
    pub signature: &'static str,
    /// What it does or is, in a line.
    pub summary: &'static str,
}

/// An object of the API, or a group of names that belong together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object {
    /// Its name in a script: `keys`. `input` is what `keys.forward` and
    /// `mouse.left` are, and `hooks` the functions a script defines.
    pub name: &'static str,
    /// What it's for, in a line.
    pub summary: &'static str,
    /// Its members.
    pub members: &'static [Member],
}

const fn method(name: &'static str, signature: &'static str, summary: &'static str) -> Member {
    Member {
        name,
        kind: MemberKind::Method,
        signature,
        summary,
    }
}

const fn property(name: &'static str, summary: &'static str) -> Member {
    Member {
        name,
        kind: MemberKind::Property,
        signature: name,
        summary,
    }
}

const fn hook(hook: Hook, signature: &'static str, summary: &'static str) -> Member {
    Member {
        name: hook.name(),
        kind: MemberKind::Hook,
        signature,
        summary,
    }
}

/// Every object of the API, in the reference's order, then `input` and
/// the hooks.
pub const API: &[Object] = &[
    Object {
        name: "keys",
        summary: "The keys of a vanilla keyboard.",
        members: &[
            property("forward", "W: walk forward."),
            property("back", "S: walk back."),
            property("left", "A: strafe left."),
            property("right", "D: strafe right."),
            property("jump", "Space."),
            property("sneak", "Shift."),
            property("sprint", "Ctrl."),
            property("drop", "Q: drops one of the held item. Tap only."),
            property("drop_stack", "Ctrl+Q: drops the held stack. Tap only."),
            property(
                "swap_offhand",
                "F: swaps the held item with the offhand's. Tap only.",
            ),
            method("release_all", "release_all()", "Lets go of every key."),
        ],
    },
    Object {
        name: "mouse",
        summary: "The two mouse buttons.",
        members: &[
            property("left", "Attack; held on a block, mine."),
            property("right", "Use."),
        ],
    },
    Object {
        name: "input",
        summary: "A key or a mouse button: keys.forward, mouse.left.",
        members: &[
            method("tap", "tap()", "Press and let go."),
            method("hold", "hold()", "Press and keep down until release()."),
            method(
                "hold_for",
                "hold_for(ms)",
                "Press, and let go by itself after ms milliseconds (50 to 600,000).",
            ),
            method(
                "hold_until_used",
                "hold_until_used()",
                "The right button only: hold until the item is used up once.",
            ),
            method("release", "release()", "Let go. Never refused."),
            property("held", "Whether it's down now."),
        ],
    },
    Object {
        name: "look",
        summary: "Where the bot looks: the mouse's movement.",
        members: &[
            method(
                "at",
                "at(yaw, pitch)",
                "Look in a direction, in degrees: yaw -180 to 180, pitch -90 (up) to 90 (down).",
            ),
            method(
                "turn",
                "turn(yaw, pitch)",
                "Turn by that much from where the bot looks now.",
            ),
            method(
                "at_position",
                "at_position(x, y, z)",
                "Look at a point in the world.",
            ),
            method(
                "at_entity",
                "at_entity(entity)",
                "Look at an entity world returned.",
            ),
            property("yaw", "Where the bot looks, left and right."),
            property("pitch", "Where the bot looks, up and down."),
        ],
    },
    Object {
        name: "hotbar",
        summary: "The hotbar: the number keys.",
        members: &[
            method("select", "select(n)", "Select slot 1 to 9."),
            property("selected", "The selected slot, 1 to 9."),
        ],
    },
    Object {
        name: "vehicle",
        summary: "Getting into a vehicle and out of it.",
        members: &[
            method(
                "mount",
                "mount(kind)",
                "Get into the nearest free vehicle of that kind within reach: \"minecart\".",
            ),
            method("dismount", "dismount()", "Get out."),
        ],
    },
    Object {
        name: "chat",
        summary: "The mode's declared messages and chat patterns.",
        members: &[
            method(
                "send",
                "send(key, values)",
                "Send the declared message key; values is a map of its values, left out for none.",
            ),
            method(
                "expect",
                "expect(key, ms)",
                "After a declared command in the same hook: read the reply pattern key for ms milliseconds (250 to 30,000).",
            ),
        ],
    },
    Object {
        name: "timer",
        summary: "Named timers that call on_timer(name).",
        members: &[
            method(
                "after",
                "after(ms, name)",
                "Call on_timer(name) once, ms milliseconds from now (250 to 86,400,000).",
            ),
            method(
                "every",
                "every(ms, name)",
                "Call on_timer(name) every ms milliseconds, first after ms.",
            ),
            method("cancel", "cancel(name)", "Remove that timer."),
        ],
    },
    Object {
        name: "random",
        summary: "Random numbers.",
        members: &[
            method(
                "between",
                "between(low, high)",
                "A whole number from low to high, both included.",
            ),
            method("chance", "chance(percent)", "true that often, in percent."),
        ],
    },
    Object {
        name: "log",
        summary: "The script's log, shown on the bot's page.",
        members: &[method(
            "info",
            "info(text)",
            "Write a line: up to 200 characters, 20 lines a minute.",
        )],
    },
    Object {
        name: "bot",
        summary: "What the script knows about its own bot. Read-only.",
        members: &[
            property("health", "Health, 0 to 20."),
            property("food", "Food, 0 to 20."),
            property("x", "Where its feet are: east (positive) and west."),
            property("y", "Where its feet are: up (positive) and down."),
            property("z", "Where its feet are: south (positive) and north."),
            property(
                "dimension",
                "The world it's in, such as \"minecraft:overworld\".",
            ),
            property("riding", "Whether it sits in a vehicle."),
            property(
                "held",
                "The item in its hand, or () for an empty hand: id, count, durability.",
            ),
            property(
                "looking_at",
                "What its crosshair is on within reach, or (): kind, id, x, y, z.",
            ),
        ],
    },
    Object {
        name: "world",
        summary: "What's near the bot. Read-only.",
        members: &[
            method(
                "nearest",
                "nearest(kind)",
                "The nearest entity of that kind within 16 blocks, or (): kind, x, y, z, distance, occupied.",
            ),
            method(
                "nearest_free",
                "nearest_free(kind)",
                "The same, skipping vehicles someone sits in.",
            ),
            method(
                "block",
                "block(x, y, z)",
                "The block's ID there, or () further than 4 blocks from the bot.",
            ),
        ],
    },
    Object {
        name: "hooks",
        summary: "The functions a script defines for the runtime to call.",
        members: &[
            hook(
                Hook::Join,
                "fn on_join(first)",
                "The bot is in the world after connecting; first is false after a reconnect.",
            ),
            hook(
                Hook::WorldChange,
                "fn on_world_change()",
                "The server moved the bot to another world without a disconnect.",
            ),
            hook(Hook::Death, "fn on_death()", "The bot died."),
            hook(
                Hook::Respawn,
                "fn on_respawn()",
                "The bot lives again after a death.",
            ),
            hook(
                Hook::Timer,
                "fn on_timer(name)",
                "A timer the script set is due.",
            ),
            hook(
                Hook::Chat,
                "fn on_chat(key, values)",
                "The server printed a line that fits one of the mode's declared patterns.",
            ),
            hook(
                Hook::Stop,
                "fn on_stop()",
                "The script stops in an orderly way.",
            ),
        ],
    },
];

/// Every reason an action's result can carry in `result.reason`, as a
/// script reads it.
#[must_use]
pub fn reasons() -> [&'static str; Refusal::ALL.len()] {
    Refusal::ALL.map(Refusal::as_str)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fleet_core::script::Key;

    use super::*;

    fn object(name: &str) -> &'static Object {
        API.iter().find(|object| object.name == name).unwrap()
    }

    #[test]
    fn every_hook_is_described_with_its_parameters() {
        let hooks = object("hooks");

        assert_eq!(hooks.members.len(), Hook::ALL.len());
        for hook in Hook::ALL {
            let member = hooks
                .members
                .iter()
                .find(|member| member.name == hook.name())
                .unwrap();
            let expected = format!("fn {}({})", hook.name(), hook.parameters().join(", "));

            assert_eq!(member.signature, expected);
            assert_eq!(member.kind, MemberKind::Hook);
        }
    }

    #[test]
    fn every_key_is_a_property_of_keys() {
        let keys = object("keys");

        for key in Key::ALL {
            assert!(
                keys.members
                    .iter()
                    .any(|member| member.name == key.name() && member.kind == MemberKind::Property),
                "{}",
                key.name()
            );
        }
    }

    #[test]
    fn a_methods_signature_starts_with_its_name_and_a_property_is_its_own() {
        for object in API {
            for member in object.members {
                match member.kind {
                    MemberKind::Method => {
                        assert!(
                            member.signature.starts_with(&format!("{}(", member.name))
                                && member.signature.ends_with(')'),
                            "{}.{}",
                            object.name,
                            member.name
                        );
                    }
                    MemberKind::Property => assert_eq!(member.signature, member.name),
                    MemberKind::Hook => assert!(member.signature.starts_with("fn on_")),
                }
                assert_ne!(member.summary.len(), 0);
            }
            assert_ne!(object.summary.len(), 0);
        }
    }

    #[test]
    fn no_object_names_a_member_twice_and_no_name_is_an_objects_twice() {
        let names: BTreeSet<&str> = API.iter().map(|object| object.name).collect();
        assert_eq!(names.len(), API.len());

        for object in API {
            let members: BTreeSet<&str> = object.members.iter().map(|member| member.name).collect();
            assert_eq!(members.len(), object.members.len(), "{}", object.name);
        }
    }

    #[test]
    fn the_reasons_are_fleet_cores_closed_list() {
        assert_eq!(
            reasons(),
            [
                "too_soon",
                "dead",
                "not_in_world",
                "leashed",
                "mining_guard",
                "tool_guard",
                "bad_value",
                "too_many",
                "no_command"
            ]
        );
    }
}
