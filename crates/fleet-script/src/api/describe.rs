//! The one description of the script API: every object, each of its
//! members, the hooks and the reasons (ADR-0036, D4).
//!
//! The reference's tables and the editor's autocomplete are made from
//! this; a test holds it equal to what the engine registers, so neither
//! can name something a script doesn't have.

use fleet_core::script::{Hook, Refusal, ScriptApiLevel};

/// What a member of an object is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemberKind {
    /// A function called with parentheses: `keys.jump.tap()`.
    Method,
    /// A value read without them: `bot.health`.
    Property,
    /// A function a script defines and the runtime calls.
    Hook,
    /// A field of a map the API hands a script (a slot's `count`): plain
    /// data, read as a map's.
    Field,
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
    /// The object of this API that a property is, when it's one: each of
    /// `keys`' keys and `mouse`'s buttons is an `input`, so the editor knows
    /// what follows `keys.forward.` (Plan.md P15.25).
    pub object: Option<&'static str>,
    /// The map of this API that a method answers, when it answers one:
    /// `menu.slot(n)` answers a `slot`, so the editor knows what follows
    /// `menu.slot(3).` (Plan.md P16.16).
    pub answers: Option<&'static str>,
    /// The API level it came with: a script below it can't use it
    /// (ADR-0036, D13).
    pub level: ScriptApiLevel,
}

impl Member {
    /// The member, as API level 2's.
    const fn v2(self) -> Self {
        Self {
            level: ScriptApiLevel::V2,
            ..self
        }
    }

    /// The method, answering a `map` of this API.
    const fn answering(self, map: &'static str) -> Self {
        Self {
            answers: Some(map),
            ..self
        }
    }
}

/// What an object of the API is to a script (Plan.md P16.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// A name a script uses to reach the game: `keys`, `menu`.
    Game,
    /// What a member is or a method answers, never named by a script:
    /// `input` (what `keys.forward` is), a `slot` map, a `time` map.
    Value,
    /// The functions a script defines.
    Hooks,
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
    /// The API level it came with: a script below it doesn't have it
    /// (ADR-0036, D13). An object of level 1 may still hold a member of
    /// level 2 (`bot.name`).
    pub level: ScriptApiLevel,
}

impl Object {
    /// What it is to a script: the functions it defines, a value a member
    /// is or a method answers, or a name it uses to reach the game.
    #[must_use]
    pub fn kind(&self) -> ObjectKind {
        let all = |kind: MemberKind| {
            !self.members.is_empty() && self.members.iter().all(|member| member.kind == kind)
        };
        if all(MemberKind::Hook) {
            return ObjectKind::Hooks;
        }
        let answered = API
            .iter()
            .flat_map(|object| object.members)
            .any(|member| member.object == Some(self.name) || member.answers == Some(self.name));
        if answered || all(MemberKind::Field) {
            ObjectKind::Value
        } else {
            ObjectKind::Game
        }
    }
}

const fn method(name: &'static str, signature: &'static str, summary: &'static str) -> Member {
    Member {
        name,
        kind: MemberKind::Method,
        signature,
        summary,
        object: None,
        answers: None,
        level: ScriptApiLevel::V1,
    }
}

const fn property(name: &'static str, summary: &'static str) -> Member {
    Member {
        name,
        kind: MemberKind::Property,
        signature: name,
        summary,
        object: None,
        answers: None,
        level: ScriptApiLevel::V1,
    }
}

/// A field of a map the API hands a script, at API level 2.
const fn field(name: &'static str, summary: &'static str) -> Member {
    Member {
        name,
        kind: MemberKind::Field,
        signature: name,
        summary,
        object: None,
        answers: None,
        level: ScriptApiLevel::V2,
    }
}

/// A key or a mouse button: a property that is an `input`.
const fn input(name: &'static str, summary: &'static str) -> Member {
    Member {
        name,
        kind: MemberKind::Property,
        signature: name,
        summary,
        object: Some("input"),
        answers: None,
        level: ScriptApiLevel::V1,
    }
}

const fn hook(hook: Hook, signature: &'static str, summary: &'static str) -> Member {
    Member {
        name: hook.name(),
        kind: MemberKind::Hook,
        signature,
        summary,
        object: None,
        answers: None,
        level: hook.level(),
    }
}

/// Every object of the API, in the reference's order, then `input` and
/// the hooks.
pub const API: &[Object] = &[
    Object {
        name: "keys",
        summary: "The keys of a vanilla keyboard.",
        members: &[
            input("forward", "W: walk forward."),
            input("back", "S: walk back."),
            input("left", "A: strafe left."),
            input("right", "D: strafe right."),
            input("jump", "Space."),
            input("sneak", "Shift."),
            input("sprint", "Ctrl."),
            input("drop", "Q: drops one of the held item. Tap only."),
            input("drop_stack", "Ctrl+Q: drops the held stack. Tap only."),
            input(
                "swap_offhand",
                "F: swaps the held item with the offhand's. Tap only.",
            ),
            method("release_all", "release_all()", "Lets go of every key."),
        ],
        level: ScriptApiLevel::V1,
    },
    Object {
        name: "mouse",
        summary: "The two mouse buttons.",
        members: &[
            input("left", "Attack; held on a block, mine."),
            input("right", "Use."),
        ],
        level: ScriptApiLevel::V1,
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
        level: ScriptApiLevel::V1,
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
        level: ScriptApiLevel::V1,
    },
    Object {
        name: "hotbar",
        summary: "The hotbar: the number keys.",
        members: &[
            method("select", "select(n)", "Select slot 1 to 9."),
            property("selected", "The selected slot, 1 to 9."),
        ],
        level: ScriptApiLevel::V1,
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
        level: ScriptApiLevel::V1,
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
        level: ScriptApiLevel::V1,
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
        level: ScriptApiLevel::V1,
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
        level: ScriptApiLevel::V1,
    },
    Object {
        name: "log",
        summary: "The script's log, shown on the bot's page.",
        members: &[method(
            "info",
            "info(text)",
            "Write a line: up to 200 characters, 20 lines a minute.",
        )],
        level: ScriptApiLevel::V1,
    },
    Object {
        name: "bot",
        summary: "What the script knows about its own bot. Read-only.",
        members: &[
            property(
                "name",
                "The bot's own player name, as its account has it. API level 2.",
            )
            .v2(),
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
        level: ScriptApiLevel::V1,
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
        level: ScriptApiLevel::V1,
    },
    Object {
        name: "inventory",
        summary: "The bot's own slots: its armour, its 36 slots and its offhand, numbered as its inventory screen numbers them.",
        members: &[
            property(
                "slots",
                "Every slot's number: the armour 5 to 8, the main inventory 9 to 35, the hotbar 36 to 44, the offhand 45.",
            )
            .v2(),
            method("slot", "slot(n)", "Slot n, or ().")
                .v2()
                .answering("slot"),
            method(
                "lore_has",
                "lore_has(slot, text)",
                "Whether a lore line of the bot's slot holds text, as on menu.",
            )
            .v2(),
            method(
                "lore_number",
                "lore_number(slot, label)",
                "The first number after label in the bot's slot's lore, as on menu.",
            )
            .v2(),
            method(
                "find",
                "find(id)",
                "The first slot that holds the item id, or ().",
            )
            .v2()
            .answering("slot"),
            method(
                "count",
                "count(id)",
                "How many of the item id the bot carries.",
            )
            .v2(),
            method(
                "hotbar",
                "hotbar(n)",
                "The hotbar slot of number key n, 1 to 9, or ().",
            )
            .v2()
            .answering("slot"),
            method(
                "open",
                "open()",
                "E: opens the inventory screen; only with no screen open. Nothing goes to the server.",
            )
            .v2(),
        ],
        level: ScriptApiLevel::V2,
    },
    Object {
        name: "menu",
        summary: "The screen that's open: a server's, or the bot's own inventory.",
        members: &[
            property("open", "Whether a screen is open.").v2(),
            property(
                "screen",
                "Which screen it is: a number new at every opening, or () with none.",
            )
            .v2(),
            property(
                "kind",
                "Its kind: generic_9x3, anvil, merchant, inventory, …; () with none.",
            )
            .v2(),
            property("title", "Its title, as plain text.").v2(),
            property("rows", "A grid's rows; () for a screen that isn't one.").v2(),
            property("columns", "A grid's columns; () for a screen that isn't one.").v2(),
            property(
                "slots",
                "The numbers of the screen's own slots, from 0: for n in menu.slots { let slot = menu.slot(n); … }.",
            )
            .v2(),
            property(
                "player",
                "The numbers of the bot's 36 slots in this screen: four rows of nine, the hotbar last.",
            )
            .v2(),
            method(
                "slot",
                "slot(n)",
                "Slot n of the screen, its own or the bot's, or ().",
            )
            .v2()
            .answering("slot"),
            method(
                "at",
                "at(row, column)",
                "A grid's slot by its row and column, from 0 at the top left, or ().",
            )
            .v2()
            .answering("slot"),
            method(
                "find",
                "find(id)",
                "The first of the screen's own slots that holds the item id, or ().",
            )
            .v2()
            .answering("slot"),
            property("cursor", "The item on the cursor, or ().").v2(),
            property("trades", "How many trades a merchant offers; 0 for any other screen.").v2(),
            method(
                "trade",
                "trade(n)",
                "Trade n, from 0: a map of cost_id, cost_count, second_id, second_count, result_id, result_count, result_name, result_potion, result_enchantments, result_stored_enchantments, out_of_stock; or ().",
            )
            .v2(),
            property(
                "pending",
                "Whether an action of the script's still waits for its fate.",
            )
            .v2(),
            method(
                "lore_has",
                "lore_has(slot, text)",
                "Whether a lore line of the slot (its number, or a slot read on this screen) holds text. An empty text is in none.",
            )
            .v2(),
            method(
                "lore_number",
                "lore_number(slot, label)",
                "The first number after label on the slot's first lore line that starts with it, or (); lore_number(slot, label, \",\") for a comma as the decimal mark.",
            )
            .v2(),
            method(
                "number",
                "number(text)",
                "The first number in text as servers write amounts (1,250.5, 2k, 1.5M, 3B), or (); number(text, \",\") for a comma as the decimal mark.",
            )
            .v2(),
            method(
                "click",
                "click(slot)",
                "A left click on a slot: its number, or a slot a hook read. Its fate comes in on_click.",
            )
            .v2(),
            method("right_click", "right_click(slot)", "A right click on a slot.").v2(),
            method(
                "shift_click",
                "shift_click(slot)",
                "Shift and a left click: moves the stack to the other part of the screen.",
            )
            .v2(),
            method(
                "swap",
                "swap(slot, n)",
                "Number key n over a slot: swaps it with hotbar slot n, 1 to 9.",
            )
            .v2(),
            method("drop", "drop(slot)", "Q over a slot: drops one item.").v2(),
            method(
                "drop_stack",
                "drop_stack(slot)",
                "Ctrl+Q over a slot: drops the stack.",
            )
            .v2(),
            method(
                "button",
                "button(n)",
                "A screen's own button: an enchanting table's 0 to 2, a stonecutter's or a loom's.",
            )
            .v2(),
            method(
                "select_trade",
                "select_trade(n)",
                "Selects a merchant's trade n, from 0.",
            )
            .v2(),
            method(
                "rename",
                "rename(key, values)",
                "Sets the anvil's name to a declared message: rename(key) without values.",
            )
            .v2(),
            method(
                "close",
                "close()",
                "Esc: closes the screen. Never limited, never refused while one is open.",
            )
            .v2(),
        ],
        level: ScriptApiLevel::V2,
    },
    Object {
        name: "sign",
        summary: "The sign editor the server opened, while it waits for its answer.",
        members: &[
            property("open", "Whether a sign editor waits for its answer.").v2(),
            property("x", "The sign's block: x.").v2(),
            property("y", "The sign's block: y.").v2(),
            property("z", "The sign's block: z.").v2(),
            property("side", "\"front\" or \"back\".").v2(),
            method(
                "write",
                "write(key, values)",
                "Answers with a declared sign: write(key) without values. Once an editor.",
            )
            .v2(),
            method(
                "cancel",
                "cancel()",
                "Answers with nothing written. Once an editor.",
            )
            .v2(),
        ],
        level: ScriptApiLevel::V2,
    },
    Object {
        name: "clock",
        summary: "The time of day of the hook's moment.",
        members: &[method(
            "local",
            "local(zone)",
            "The time in an IANA time zone such as \"Europe/Berlin\", summer time included: a time map.",
        )
        .v2()
        .answering("time")],
        level: ScriptApiLevel::V2,
    },
    Object {
        name: "slot",
        summary: "A slot as a script reads it: a map of plain data, from menu.slot(n) or inventory.slot(n).",
        members: &[
            field("index", "Its number in the screen, or in the inventory."),
            field("row", "Its row in a grid, from 0 at the top; () outside one."),
            field("column", "Its column in a grid, from 0 at the left; () outside one."),
            field(
                "screen",
                "The screen it was read on (menu.screen); () for the inventory's own.",
            ),
            field("empty", "Whether it holds nothing."),
            field("id", "The item's registry ID; () when empty."),
            field("count", "How many; 0 when empty."),
            field("name", "The name the client shows: the custom name, or the item's."),
            field(
                "lore",
                "The lore's lines, plain text: the first ones, while a value holds them. menu.lore_has and lore_number read them all.",
            ),
            field("durability", "The uses left; () for an item that doesn't wear."),
            field(
                "potion",
                "A potion's, a splash or lingering potion's or a tipped arrow's kind, such as \"minecraft:turtle_master\"; ().",
            ),
            field(
                "enchantments",
                "A map of each enchantment's ID to its level.",
            ),
            field(
                "stored_enchantments",
                "An enchanted book's, the same way.",
            ),
        ],
        level: ScriptApiLevel::V2,
    },
    Object {
        name: "time",
        summary: "A time of day, as clock.local answers it: a map of whole numbers.",
        members: &[
            field("year", "The year."),
            field("month", "1 to 12."),
            field("day", "The day of the month, 1 to 31."),
            field("hour", "0 to 23."),
            field("minute", "0 to 59."),
            field("second", "0 to 59."),
            field("weekday", "1 for Monday to 7 for Sunday."),
            field(
                "offset",
                "Minutes ahead of UTC: 60 in Berlin's winter, 120 in its summer.",
            ),
        ],
        level: ScriptApiLevel::V2,
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
            hook(
                Hook::MenuOpen,
                "fn on_menu_open()",
                "A screen opened: a server's, or the inventory. Everything held was let go.",
            ),
            hook(
                Hook::MenuUpdate,
                "fn on_menu_update()",
                "The open screen's content changed: at most once a game tick.",
            ),
            hook(
                Hook::MenuClose,
                "fn on_menu_close()",
                "The screen closed.",
            ),
            hook(
                Hook::Click,
                "fn on_click(result)",
                "What became of a click, a button, a trade or a name: ok, reason and slot.",
            ),
            hook(
                Hook::SignOpen,
                "fn on_sign_open()",
                "The server opened a sign editor.",
            ),
        ],
        level: ScriptApiLevel::V1,
    },
];

/// Every reason an action's result can carry in `result.reason`, as a
/// script reads it.
#[must_use]
pub fn reasons() -> [&'static str; Refusal::ALL.len()] {
    Refusal::ALL.map(Refusal::as_str)
}

/// What `reason` means, in a line: the reference's table of reasons.
#[must_use]
pub const fn reason_meaning(reason: Refusal) -> &'static str {
    match reason {
        Refusal::TooSoon => "The action came faster than its limit allows (see Limits).",
        Refusal::Dead => "The bot is dead.",
        Refusal::NotInWorld => "The bot is between worlds, or its session is ending.",
        Refusal::Leashed => "The bot is at the end of its leash, and this would take it further.",
        Refusal::MiningGuard => "The block reaches below the bot's feet.",
        Refusal::ToolGuard => "The held tool is nearly used up.",
        Refusal::BlockGuard => "The block isn't one the mode lists for mining.",
        Refusal::BadValue => {
            "A number outside its range, an angle that isn't a number, an unknown kind."
        }
        Refusal::TooMany => "No room for another timer.",
        Refusal::NoCommand => {
            "chat.expect without a declared command sent before it in the same hook."
        }
        Refusal::ScreenOpen => {
            "A screen is open: keys, buttons, looks, the hotbar and vehicles wait, and the inventory doesn't open."
        }
        Refusal::NoMenu => "No screen is open.",
        Refusal::ScreenChanged => {
            "Another screen is open than the one the hook saw, or than the one the slot was read on."
        }
        Refusal::ContentChanged => "The screen's content changed since the hook saw it.",
        Refusal::SlotChanged => {
            "A slot the click touches holds something else than the hook or the slot saw."
        }
        Refusal::ClickPending => "An earlier action's fate hasn't come yet.",
        Refusal::Unanswered => {
            "No answer came within 5 s: the server may or may not have done it. The screen takes no click until it's opened again."
        }
        Refusal::NoSign => "No sign editor waits for an answer.",
        Refusal::SignChanged => "Another sign editor is open than the one the hook saw.",
        Refusal::SignAnswered => "The sign editor was answered already.",
        Refusal::Rejected => {
            "The server corrected the screen: it put things back or did otherwise. A plugin's button answers so though it acted; judge by the screen that follows."
        }
        Refusal::Unclear => {
            "The server corrected the screen after a click on a result slot (a trade, an anvil's, a craft's), a button, a trade or a name: it may have done it, once or more. Judge by the screen."
        }
        Refusal::Replaced => {
            "The screen closed, or another opened, before the answer: the server may have done it."
        }
    }
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
                    MemberKind::Property | MemberKind::Field => {
                        assert_eq!(member.signature, member.name);
                    }
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
    fn every_key_and_mouse_button_is_an_input() {
        let inputs: Vec<(&str, &str)> = API
            .iter()
            .flat_map(|object| {
                object
                    .members
                    .iter()
                    .filter(|member| member.object == Some("input"))
                    .map(|member| (object.name, member.name))
            })
            .collect();
        let mut expected: Vec<(&str, &str)> =
            Key::ALL.iter().map(|key| ("keys", key.name())).collect();
        expected.extend([("mouse", "left"), ("mouse", "right")]);

        assert_eq!(inputs, expected);
    }

    /// What the editor completes after `menu.slot(3).` (Plan.md P16.16).
    #[test]
    fn a_method_that_answers_a_map_names_one_of_the_apis_maps() {
        let answering: Vec<(&str, &str, &str)> = API
            .iter()
            .flat_map(|object| {
                object.members.iter().filter_map(move |member| {
                    member.answers.map(|map| (object.name, member.name, map))
                })
            })
            .collect();

        assert_eq!(
            answering,
            [
                ("inventory", "slot", "slot"),
                ("inventory", "find", "slot"),
                ("inventory", "hotbar", "slot"),
                ("menu", "slot", "slot"),
                ("menu", "at", "slot"),
                ("menu", "find", "slot"),
                ("clock", "local", "time"),
            ]
        );
        for (object, member, map) in answering {
            let answered = API.iter().find(|known| known.name == map).unwrap();
            assert_eq!(answered.kind(), ObjectKind::Value, "{object}.{member}");
        }
    }

    #[test]
    fn every_object_has_its_kind() {
        let kinds: Vec<(&str, ObjectKind)> = API
            .iter()
            .map(|object| (object.name, object.kind()))
            .filter(|(_, kind)| *kind != ObjectKind::Game)
            .collect();

        assert_eq!(
            kinds,
            [
                ("input", ObjectKind::Value),
                ("slot", ObjectKind::Value),
                ("time", ObjectKind::Value),
                ("hooks", ObjectKind::Hooks),
            ]
        );
    }

    #[test]
    fn a_member_that_is_an_object_is_a_property_and_names_one_the_api_has() {
        for object in API {
            for member in object.members {
                if let Some(name) = member.object {
                    assert_eq!(
                        member.kind,
                        MemberKind::Property,
                        "{}.{}",
                        object.name,
                        member.name
                    );
                    assert!(
                        API.iter()
                            .any(|known| known.name == name && known.name != object.name),
                        "{}.{} is {name}",
                        object.name,
                        member.name
                    );
                }
            }
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
                "block_guard",
                "bad_value",
                "too_many",
                "no_command",
                "screen_open",
                "no_menu",
                "screen_changed",
                "content_changed",
                "slot_changed",
                "click_pending",
                "unanswered",
                "no_sign",
                "sign_changed",
                "sign_answered",
                "rejected",
                "unclear",
                "replaced"
            ]
        );
    }

    #[test]
    fn level_2s_objects_are_level_2_and_level_1s_hold_only_bot_name_of_it() {
        for object in API {
            let newer = matches!(
                object.name,
                "inventory" | "menu" | "sign" | "clock" | "slot" | "time"
            );
            assert_eq!(
                object.level,
                if newer {
                    ScriptApiLevel::V2
                } else {
                    ScriptApiLevel::V1
                },
                "{}",
                object.name
            );
            for member in object.members {
                assert!(
                    member.level >= object.level,
                    "{}.{}",
                    object.name,
                    member.name
                );
                if object.level == ScriptApiLevel::V1 && member.level == ScriptApiLevel::V2 {
                    assert!(
                        (object.name, member.name) == ("bot", "name") || object.name == "hooks",
                        "{}.{}",
                        object.name,
                        member.name
                    );
                }
            }
        }
    }

    #[test]
    fn only_a_maps_description_has_fields() {
        for object in API {
            for member in object.members {
                if member.kind == MemberKind::Field {
                    assert!(
                        matches!(object.name, "slot" | "time"),
                        "{}.{}",
                        object.name,
                        member.name
                    );
                }
            }
        }
    }
}
