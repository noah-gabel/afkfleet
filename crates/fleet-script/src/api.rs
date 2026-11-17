//! The game objects a script acts and reads through: `keys`, `mouse`,
//! `look`, `hotbar`, `vehicle`, `chat`, `timer`, `random`, `log`, `bot` and
//! `world` (P15.8, P15.31; ADR-0036, D4).
//!
//! **Their functions are one module, built once and shared by every
//! engine.** A function holds no state: it gets a small handle that points
//! at its instance's [`Turn`], which the instance's variable resolver
//! makes when a script names an object. So the same works in a hook and in
//! a script's own functions, and an engine an instance costs almost
//! nothing.
//!
//! **Everything a script gets back is plain data:** numbers, texts, `true`
//! and `false`, `()` for nothing, and maps of those. A result is a map
//! with `ok` and `reason`; an entity, a held item and what the crosshair
//! is on are maps too. So a script can keep any of them in its state.
//!
//! **A wrong value is a result, a wrong program an error.** A number out
//! of range or a text where a number belongs answers `bad_value`, and the
//! script goes on. A key the mode doesn't declare, or a hold of a key that
//! only taps, stops the script with its line, as a misspelled method does:
//! no value could make it right ([`Misuse`]).
//!
//! [`describe`] is the one description of all of it, which the reference's
//! tables and the editor's autocomplete are made from.

use core::time::Duration;
use std::sync::LazyLock;

use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    BlockPosition, EntityFact, HeldItem, Identifier, Input, Key, LookingAt, MouseButton, Position,
    Refusal, Snapshot, TimerName,
};
use rhai::{
    Dynamic, EvalAltResult, FLOAT, FuncRegistration, INT, Map, Module, Position as At, Shared,
};

use crate::error::Misuse;
use crate::turn::{Answer, HoldLength, SharedTurn, Value, lock};

pub(crate) mod describe;

/// The game objects' names, in the reference's order. With `state`, these
/// are the names a script can't give a variable.
pub(crate) const OBJECTS: [&str; 11] = [
    "keys", "mouse", "look", "hotbar", "vehicle", "chat", "timer", "random", "log", "bot", "world",
];

/// What a function of the API returns to Rhai.
type Returns<T> = Result<T, Box<EvalAltResult>>;

/// `keys`: the keys of a vanilla keyboard.
#[derive(Debug, Clone)]
struct Keys(SharedTurn);
/// `mouse`: the two buttons.
#[derive(Debug, Clone)]
struct Mouse(SharedTurn);
/// A key or a button: `keys.forward`, `mouse.left`.
#[derive(Debug, Clone)]
struct InputHandle(SharedTurn, Input);
/// `look`: the mouse's movement.
#[derive(Debug, Clone)]
struct Look(SharedTurn);
/// `hotbar`: the number keys.
#[derive(Debug, Clone)]
struct Hotbar(SharedTurn);
/// `vehicle`: getting in and out.
#[derive(Debug, Clone)]
struct Vehicle(SharedTurn);
/// `chat`: declared messages out, declared patterns in.
#[derive(Debug, Clone)]
struct Chat(SharedTurn);
/// `timer`: named timers.
#[derive(Debug, Clone)]
struct Timer(SharedTurn);
/// `random`: the script's own random numbers.
#[derive(Debug, Clone)]
struct Random(SharedTurn);
/// `log`: the script's log.
#[derive(Debug, Clone)]
struct Log(SharedTurn);
/// `bot`: what the script knows about its own bot.
#[derive(Debug, Clone)]
struct Bot(SharedTurn);
/// `world`: what's near the bot.
#[derive(Debug, Clone)]
struct World(SharedTurn);

/// The game object `name` for the instance whose turn is `turn`.
pub(crate) fn object(name: &str, turn: &SharedTurn) -> Option<Dynamic> {
    let turn = SharedTurn::clone(turn);
    Some(match name {
        "keys" => Dynamic::from(Keys(turn)),
        "mouse" => Dynamic::from(Mouse(turn)),
        "look" => Dynamic::from(Look(turn)),
        "hotbar" => Dynamic::from(Hotbar(turn)),
        "vehicle" => Dynamic::from(Vehicle(turn)),
        "chat" => Dynamic::from(Chat(turn)),
        "timer" => Dynamic::from(Timer(turn)),
        "random" => Dynamic::from(Random(turn)),
        "log" => Dynamic::from(Log(turn)),
        "bot" => Dynamic::from(Bot(turn)),
        "world" => Dynamic::from(World(turn)),
        _ => return None,
    })
}

/// A result as a script reads it: `ok`, and `reason`, which is empty for
/// an action that was taken.
fn result(answer: Answer) -> Map {
    let mut map = Map::new();
    map.insert("ok".into(), Dynamic::from_bool(answer.is_ok()));
    map.insert(
        "reason".into(),
        Dynamic::from(answer.err().map_or("", Refusal::as_str)),
    );
    map
}

/// A mistake that stops the script, as an error Rhai carries to the
/// instance with the call's own line.
fn misuse(kind: Misuse) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(kind), At::NONE)
}

/// The result of an action that a [`Misuse`] can stop.
fn strict(answered: Result<Answer, Misuse>) -> Returns<Map> {
    answered.map(result).map_err(|kind| misuse(kind).into())
}

/// `value` as a number, whole or decimal: never NaN or infinite, and a
/// whole number only where a decimal one holds it exactly.
fn number(value: &Dynamic) -> Option<f64> {
    if let Ok(whole) = value.as_int() {
        return i32::try_from(whole).ok().map(f64::from);
    }
    value.as_float().ok().filter(|number| number.is_finite())
}

/// `value` as a whole number: a decimal one counts down to the next whole
/// one.
fn whole(value: &Dynamic) -> Option<i64> {
    if let Ok(whole) = value.as_int() {
        return Some(whole);
    }
    let floored = value.as_float().ok()?.floor();
    // Written out and read back: the one conversion from a decimal to a
    // whole number that can't silently wrap. What isn't a number, or is
    // too large for one, doesn't read.
    format!("{floored:.0}").parse().ok()
}

/// `value` as a length of time in milliseconds.
fn milliseconds(value: &Dynamic) -> Option<Duration> {
    whole(value)
        .and_then(|ms| u64::try_from(ms).ok())
        .map(Duration::from_millis)
}

/// `value` as text.
fn text(value: &Dynamic) -> Option<String> {
    value
        .as_immutable_string_ref()
        .ok()
        .map(|text| text.to_string())
}

/// `value` as a registry identifier.
fn identifier(value: &Dynamic) -> Option<Identifier> {
    text(value).and_then(|text| Identifier::try_from(text.as_str()).ok())
}

/// Three values as a point in the world.
fn point(x: &Dynamic, y: &Dynamic, z: &Dynamic) -> Option<Position> {
    Position::try_new(number(x)?, number(y)?, number(z)?).ok()
}

/// A whole number of ours as Rhai's.
fn int(value: impl Into<i64>) -> Dynamic {
    Dynamic::from_int(value.into())
}

/// An item as a script reads it: `id`, `count`, and `durability`, which
/// is `()` for one that doesn't wear.
fn item(held: &HeldItem) -> Dynamic {
    let mut map = Map::new();
    map.insert("id".into(), Dynamic::from(held.id.as_str().to_owned()));
    map.insert("count".into(), int(held.count));
    map.insert(
        "durability".into(),
        held.durability.map_or(Dynamic::UNIT, int),
    );
    Dynamic::from_map(map)
}

/// What the crosshair is on as a script reads it: `kind`, `id`, `x`, `y`
/// and `z`, whole numbers for a block.
fn looking_at(target: &LookingAt) -> Dynamic {
    let mut map = Map::new();
    match target {
        LookingAt::Block { id, at } => {
            map.insert("kind".into(), Dynamic::from("block"));
            map.insert("id".into(), Dynamic::from(id.as_str().to_owned()));
            map.insert("x".into(), int(at.x));
            map.insert("y".into(), int(at.y));
            map.insert("z".into(), int(at.z));
        }
        LookingAt::Entity { kind, at } => {
            map.insert("kind".into(), Dynamic::from("entity"));
            map.insert("id".into(), Dynamic::from(kind.as_str().to_owned()));
            map.insert("x".into(), Dynamic::from_float(at.x()));
            map.insert("y".into(), Dynamic::from_float(at.y()));
            map.insert("z".into(), Dynamic::from_float(at.z()));
        }
    }
    Dynamic::from_map(map)
}

/// An entity as a script reads it: `kind`, `x`, `y`, `z`, `distance` and
/// `occupied`.
fn entity(facts: &Snapshot, found: &EntityFact) -> Dynamic {
    let mut map = Map::new();
    map.insert("kind".into(), Dynamic::from(found.kind.as_str().to_owned()));
    map.insert("x".into(), Dynamic::from_float(found.position.x()));
    map.insert("y".into(), Dynamic::from_float(found.position.y()));
    map.insert("z".into(), Dynamic::from_float(found.position.z()));
    map.insert(
        "distance".into(),
        Dynamic::from_float(facts.distance_to(found)),
    );
    map.insert("occupied".into(), Dynamic::from_bool(found.occupied));
    Dynamic::from_map(map)
}

/// The point an entity a script holds is at: a map with `x`, `y` and `z`.
fn entity_point(value: &Dynamic) -> Option<Position> {
    let map = value.as_map_ref().ok()?;
    point(map.get("x")?, map.get("y")?, map.get("z")?)
}

/// The values a script gave for a declared message: a map of names to
/// numbers and texts.
fn values(given: &Dynamic) -> Option<Vec<(String, Value)>> {
    let map = given.as_map_ref().ok()?;
    Some(
        map.iter()
            .map(|(name, value)| {
                let value = if let Ok(whole) = value.as_int() {
                    Value::Integer(whole)
                } else if let Ok(decimal) = value.as_float() {
                    Value::Decimal(decimal)
                } else if let Some(text) = text(value) {
                    Value::Text(text)
                } else {
                    Value::Other
                };
                (name.to_string(), value)
            })
            .collect(),
    )
}

/// Reads something from the facts of the running hook; `otherwise`
/// outside one.
fn fact<T>(turn: &SharedTurn, otherwise: T, read: impl FnOnce(&Snapshot) -> T) -> T {
    lock(turn).facts().map_or(otherwise, read)
}

/// Registers a function of an object; `$registered` collects each
/// object's members for the test that holds the description equal.
macro_rules! method {
    ($module:expr, $registered:expr, $object:literal, $name:literal, $function:expr) => {{
        $registered.push(($object, $name));
        FuncRegistration::new($name)
            .in_global_namespace()
            // Callable on what the resolver hands out, which is constant.
            .with_purity(true)
            // Never computed ahead of time from its arguments.
            .with_volatility(true)
            .set_into_module($module, $function);
    }};
}

/// Registers a property of an object, as [`method!`] does a function.
macro_rules! property {
    ($module:expr, $registered:expr, $object:literal, $name:literal, $function:expr) => {{
        $registered.push(($object, $name));
        FuncRegistration::new_getter($name)
            .in_global_namespace()
            .with_purity(true)
            .with_volatility(true)
            .set_into_module($module, $function);
    }};
}

/// Each object's members as registered: the object's name and the
/// member's.
type Registered = Vec<(&'static str, &'static str)>;

/// The API's functions, and each object's members as registered.
fn build() -> (Module, Registered) {
    let mut module = Module::new();
    let mut registered = Vec::new();
    let (m, r) = (&mut module, &mut registered);

    m.set_custom_type::<Keys>("keys");
    m.set_custom_type::<Mouse>("mouse");
    m.set_custom_type::<InputHandle>("input");
    m.set_custom_type::<Look>("look");
    m.set_custom_type::<Hotbar>("hotbar");
    m.set_custom_type::<Vehicle>("vehicle");
    m.set_custom_type::<Chat>("chat");
    m.set_custom_type::<Timer>("timer");
    m.set_custom_type::<Random>("random");
    m.set_custom_type::<Log>("log");
    m.set_custom_type::<Bot>("bot");
    m.set_custom_type::<World>("world");

    register_keys(m, r);
    register_mouse(m, r);
    register_input(m, r);
    register_look(m, r);
    register_hotbar(m, r);
    register_vehicle(m, r);
    register_chat(m, r);
    register_timer(m, r);
    register_random(m, r);
    register_log(m, r);
    register_bot(m, r);
    register_world(m, r);
    (module, registered)
}

/// `keys`: each key, and `release_all`.
fn register_keys(m: &mut Module, r: &mut Registered) {
    property!(m, r, "keys", "forward", |keys: &mut Keys| key(
        keys,
        Key::Forward
    ));
    property!(m, r, "keys", "back", |keys: &mut Keys| key(keys, Key::Back));
    property!(m, r, "keys", "left", |keys: &mut Keys| key(keys, Key::Left));
    property!(m, r, "keys", "right", |keys: &mut Keys| key(
        keys,
        Key::Right
    ));
    property!(m, r, "keys", "jump", |keys: &mut Keys| key(keys, Key::Jump));
    property!(m, r, "keys", "sneak", |keys: &mut Keys| key(
        keys,
        Key::Sneak
    ));
    property!(m, r, "keys", "sprint", |keys: &mut Keys| key(
        keys,
        Key::Sprint
    ));
    property!(m, r, "keys", "drop", |keys: &mut Keys| key(keys, Key::Drop));
    property!(m, r, "keys", "drop_stack", |keys: &mut Keys| key(
        keys,
        Key::DropStack
    ));
    property!(m, r, "keys", "swap_offhand", |keys: &mut Keys| key(
        keys,
        Key::SwapOffhand
    ));
    method!(m, r, "keys", "release_all", |keys: &mut Keys| {
        lock(&keys.0).release_keys();
        result(Ok(()))
    });
}

/// `mouse`: the two buttons.
fn register_mouse(m: &mut Module, r: &mut Registered) {
    property!(m, r, "mouse", "left", |mouse: &mut Mouse| InputHandle(
        SharedTurn::clone(&mouse.0),
        Input::Mouse(MouseButton::Left)
    ));
    property!(m, r, "mouse", "right", |mouse: &mut Mouse| InputHandle(
        SharedTurn::clone(&mouse.0),
        Input::Mouse(MouseButton::Right)
    ));
}

/// A key or a button: `tap`, `hold`, `hold_for`, `hold_until_used`, `release` and `held`.
fn register_input(m: &mut Module, r: &mut Registered) {
    method!(m, r, "input", "tap", |input: &mut InputHandle| result(
        lock(&input.0).tap(input.1)
    ));
    method!(m, r, "input", "hold", |input: &mut InputHandle| strict(
        lock(&input.0).hold(input.1, HoldLength::Open)
    ));
    method!(
        m,
        r,
        "input",
        "hold_for",
        |input: &mut InputHandle, ms: Dynamic| strict(
            lock(&input.0).hold(input.1, HoldLength::Time(milliseconds(&ms)))
        )
    );
    method!(
        m,
        r,
        "input",
        "hold_until_used",
        |input: &mut InputHandle| strict(lock(&input.0).hold(input.1, HoldLength::UntilUsed))
    );
    method!(m, r, "input", "release", |input: &mut InputHandle| {
        lock(&input.0).release(input.1);
        result(Ok(()))
    });
    property!(m, r, "input", "held", |input: &mut InputHandle| lock(
        &input.0
    )
    .is_held(input.1));
}

/// `look`.
fn register_look(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "look",
        "at",
        |look: &mut Look, yaw: Dynamic, pitch: Dynamic| result(
            lock(&look.0).look_at(number(&yaw), number(&pitch))
        )
    );
    method!(
        m,
        r,
        "look",
        "turn",
        |look: &mut Look, yaw: Dynamic, pitch: Dynamic| result(
            lock(&look.0).look_turn(number(&yaw), number(&pitch))
        )
    );
    method!(
        m,
        r,
        "look",
        "at_position",
        |look: &mut Look, x: Dynamic, y: Dynamic, z: Dynamic| result(
            lock(&look.0).look_at_position(point(&x, &y, &z))
        )
    );
    method!(
        m,
        r,
        "look",
        "at_entity",
        |look: &mut Look, target: Dynamic| result(
            lock(&look.0).look_at_position(entity_point(&target))
        )
    );
    property!(m, r, "look", "yaw", |look: &mut Look| fact(
        &look.0,
        0.0,
        |facts| FLOAT::from(facts.bot().yaw)
    ));
    property!(m, r, "look", "pitch", |look: &mut Look| fact(
        &look.0,
        0.0,
        |facts| FLOAT::from(facts.bot().pitch)
    ));
}

/// `hotbar`.
fn register_hotbar(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "hotbar",
        "select",
        |hotbar: &mut Hotbar, number: Dynamic| {
            // The numbers on the keys: 1 to 9.
            let slot = whole(&number)
                .and_then(|number| number.checked_sub(1))
                .and_then(|slot| u8::try_from(slot).ok())
                .and_then(|slot| HotbarSlot::try_from(slot).ok());
            result(lock(&hotbar.0).select(slot))
        }
    );
    property!(m, r, "hotbar", "selected", |hotbar: &mut Hotbar| fact(
        &hotbar.0,
        1,
        |facts| INT::from(facts.bot().selected.get()) + 1
    ));
}

/// `vehicle`.
fn register_vehicle(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "vehicle",
        "mount",
        |vehicle: &mut Vehicle, kind: Dynamic| result(lock(&vehicle.0).mount(identifier(&kind)))
    );
    method!(m, r, "vehicle", "dismount", |vehicle: &mut Vehicle| result(
        lock(&vehicle.0).dismount()
    ));
}

/// `chat`: `send`, with and without values, and `expect`.
fn register_chat(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "chat",
        "send",
        |chat: &mut Chat, key: Dynamic| strict(
            lock(&chat.0).send(text(&key).as_deref(), Some(&[]))
        )
    );
    // The same name with a second parameter: its values.
    FuncRegistration::new("send")
        .in_global_namespace()
        .with_purity(true)
        .with_volatility(true)
        .set_into_module(m, |chat: &mut Chat, key: Dynamic, given: Dynamic| {
            strict(lock(&chat.0).send(text(&key).as_deref(), values(&given).as_deref()))
        });
    method!(
        m,
        r,
        "chat",
        "expect",
        |chat: &mut Chat, key: Dynamic, ms: Dynamic| strict(
            lock(&chat.0).expect(text(&key).as_deref(), milliseconds(&ms))
        )
    );
}

/// `timer`.
fn register_timer(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "timer",
        "after",
        |timer: &mut Timer, ms: Dynamic, name: Dynamic| result(lock(&timer.0).timer(
            milliseconds(&ms),
            timer_name(&name),
            false
        ))
    );
    method!(
        m,
        r,
        "timer",
        "every",
        |timer: &mut Timer, ms: Dynamic, name: Dynamic| result(lock(&timer.0).timer(
            milliseconds(&ms),
            timer_name(&name),
            true
        ))
    );
    method!(
        m,
        r,
        "timer",
        "cancel",
        |timer: &mut Timer, name: Dynamic| {
            if let Some(name) = text(&name) {
                lock(&timer.0).timers.cancel(&name);
            }
            result(Ok(()))
        }
    );
}

/// `random`.
fn register_random(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "random",
        "between",
        |random: &mut Random, low: Dynamic, high: Dynamic| {
            // What isn't a number counts as 0.
            let (low, high) = (whole(&low).unwrap_or(0), whole(&high).unwrap_or(0));
            lock(&random.0).random_between(low, high)
        }
    );
    method!(
        m,
        r,
        "random",
        "chance",
        |random: &mut Random, percent: Dynamic| {
            // A whole number too large to be a percentage is "always" or
            // "never" by its sign; what isn't a number is "never".
            let percent = number(&percent)
                .or_else(|| whole(&percent).map(|whole| if whole > 0 { 100.0 } else { 0.0 }))
                .unwrap_or(0.0);
            lock(&random.0).random_chance(percent)
        }
    );
}

/// `log`.
fn register_log(m: &mut Module, r: &mut Registered) {
    method!(m, r, "log", "info", |log: &mut Log, line: Dynamic| {
        // A text as it is; anything else as Rhai shows it.
        let line = text(&line).unwrap_or_else(|| line.to_string());
        result(lock(&log.0).log(&line))
    });
}

/// `bot`: what the snapshot says about the script's own bot.
fn register_bot(m: &mut Module, r: &mut Registered) {
    property!(m, r, "bot", "health", |bot: &mut Bot| fact(
        &bot.0,
        0.0,
        |facts| FLOAT::from(facts.bot().health)
    ));
    property!(m, r, "bot", "food", |bot: &mut Bot| fact(
        &bot.0,
        0,
        |facts| { INT::from(facts.bot().food) }
    ));
    property!(m, r, "bot", "x", |bot: &mut Bot| fact(
        &bot.0,
        0.0,
        |facts| { facts.bot().position.x() }
    ));
    property!(m, r, "bot", "y", |bot: &mut Bot| fact(
        &bot.0,
        0.0,
        |facts| { facts.bot().position.y() }
    ));
    property!(m, r, "bot", "z", |bot: &mut Bot| fact(
        &bot.0,
        0.0,
        |facts| { facts.bot().position.z() }
    ));
    property!(m, r, "bot", "dimension", |bot: &mut Bot| fact(
        &bot.0,
        String::new(),
        |facts| facts.bot().dimension.as_str().to_owned()
    ));
    property!(m, r, "bot", "riding", |bot: &mut Bot| fact(
        &bot.0,
        false,
        |facts| facts.bot().riding.is_some()
    ));
    property!(m, r, "bot", "held", |bot: &mut Bot| fact(
        &bot.0,
        Dynamic::UNIT,
        |facts| facts.bot().held.as_ref().map_or(Dynamic::UNIT, item)
    ));
    property!(m, r, "bot", "looking_at", |bot: &mut Bot| fact(
        &bot.0,
        Dynamic::UNIT,
        |facts| facts
            .bot()
            .looking_at
            .as_ref()
            .map_or(Dynamic::UNIT, looking_at)
    ));
}

/// `world`: what the snapshot holds near the bot.
fn register_world(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "world",
        "nearest",
        |world: &mut World, kind: Dynamic| fact(&world.0, Dynamic::UNIT, |facts| {
            identifier(&kind)
                .and_then(|kind| facts.nearest(&kind))
                .map_or(Dynamic::UNIT, |found| entity(facts, found))
        })
    );
    method!(
        m,
        r,
        "world",
        "nearest_free",
        |world: &mut World, kind: Dynamic| fact(&world.0, Dynamic::UNIT, |facts| {
            identifier(&kind)
                .and_then(|kind| facts.nearest_free(&kind))
                .map_or(Dynamic::UNIT, |found| entity(facts, found))
        })
    );
    method!(
        m,
        r,
        "world",
        "block",
        |world: &mut World, x: Dynamic, y: Dynamic, z: Dynamic| fact(
            &world.0,
            Dynamic::UNIT,
            |facts| {
                let coordinate = |value: &Dynamic| whole(value).and_then(|c| i32::try_from(c).ok());
                let place = coordinate(&x)
                    .zip(coordinate(&y))
                    .zip(coordinate(&z))
                    .map(|((x, y), z)| BlockPosition { x, y, z });
                place
                    .and_then(|place| facts.blocks().get(place))
                    .map_or(Dynamic::UNIT, |id| Dynamic::from(id.as_str().to_owned()))
            }
        )
    );
}

/// The handle of `key`.
fn key(keys: &Keys, key: Key) -> InputHandle {
    InputHandle(SharedTurn::clone(&keys.0), Input::Key(key))
}

/// `value` as a timer's name.
fn timer_name(value: &Dynamic) -> Option<TimerName> {
    text(value).and_then(|name| TimerName::try_from(name.as_str()).ok())
}

/// The API's functions, built once.
static MODULE: LazyLock<Shared<Module>> = LazyLock::new(|| Shared::new(build().0));

/// The API's functions, for an engine to register.
pub(crate) fn module() -> Shared<Module> {
    Shared::clone(&MODULE)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::describe::{API, MemberKind};
    use super::*;

    #[test]
    fn the_description_names_exactly_what_is_registered() {
        let registered: BTreeSet<(&str, &str)> = build().1.into_iter().collect();
        let described: BTreeSet<(&str, &str)> = API
            .iter()
            .flat_map(|object| {
                object
                    .members
                    .iter()
                    .filter(|member| member.kind != MemberKind::Hook)
                    .map(|member| (object.name, member.name))
            })
            .collect();

        assert_eq!(registered, described);
    }

    #[test]
    fn every_game_object_has_a_handle_and_a_description() {
        for name in OBJECTS {
            assert!(API.iter().any(|object| object.name == name), "{name}");
        }
        assert!(API.iter().any(|object| object.name == "input"));
    }

    #[test]
    fn a_whole_number_reads_from_a_whole_or_a_decimal_one() {
        assert_eq!(whole(&Dynamic::from_int(-7)), Some(-7));
        assert_eq!(whole(&Dynamic::from_float(63.9)), Some(63));
        assert_eq!(whole(&Dynamic::from_float(-0.5)), Some(-1));
        assert_eq!(whole(&Dynamic::from_float(-0.0)), Some(0));
        assert_eq!(whole(&Dynamic::from_float(1500.0)), Some(1500));
        assert_eq!(whole(&Dynamic::from_float(f64::NAN)), None);
        assert_eq!(whole(&Dynamic::from_float(f64::INFINITY)), None);
        assert_eq!(whole(&Dynamic::from_float(1e30)), None);
        assert_eq!(whole(&Dynamic::from("5")), None);
        assert_eq!(whole(&Dynamic::UNIT), None);
    }

    #[test]
    fn a_number_is_whole_or_decimal_and_always_finite() {
        assert_eq!(number(&Dynamic::from_int(90)), Some(90.0));
        assert_eq!(number(&Dynamic::from_float(10.5)), Some(10.5));
        assert_eq!(number(&Dynamic::from_float(f64::NAN)), None);
        assert_eq!(number(&Dynamic::from_float(f64::NEG_INFINITY)), None);
        // A whole number a decimal one can't hold exactly isn't taken.
        assert_eq!(number(&Dynamic::from_int(i64::MAX)), None);
        assert_eq!(number(&Dynamic::from_bool(true)), None);
    }

    #[test]
    fn a_length_of_time_is_a_whole_number_of_milliseconds_from_zero() {
        assert_eq!(
            milliseconds(&Dynamic::from_int(250)),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            milliseconds(&Dynamic::from_float(1500.7)),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(milliseconds(&Dynamic::from_int(-1)), None);
        assert_eq!(milliseconds(&Dynamic::from("long")), None);
    }

    #[test]
    fn a_result_is_a_map_of_ok_and_reason() {
        let taken = result(Ok(()));
        let refused = result(Err(Refusal::TooSoon));

        assert_eq!(taken.len(), 2);
        assert!(taken.get("ok").unwrap().as_bool().unwrap());
        assert_eq!(
            taken.get("reason").unwrap().clone().into_string().unwrap(),
            ""
        );
        assert!(!refused.get("ok").unwrap().as_bool().unwrap());
        assert_eq!(
            refused
                .get("reason")
                .unwrap()
                .clone()
                .into_string()
                .unwrap(),
            "too_soon"
        );
    }
}
