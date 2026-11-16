//! Throwaway spike for ADR-0036 (Phase 15's planning): what Rhai 1.27.0 does
//! with the limits and the API shape the ADR proposes, and what both time
//! models cost at 200 bots. Not a workspace member, not built by CI.
//!
//! `cargo run --release -- all` prints everything the ADR quotes.

#![allow(unused_must_use, deprecated)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rhai::{
    AST, Array, CallFnOptions, Dynamic, Engine, EvalAltResult, INT, ImmutableString, Map, Scope,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const MINECART_FARM: &str = include_str!("../scripts/minecart_farm.rhai");
const AUCTION_SCAN: &str = include_str!("../scripts/auction_scan.rhai");
const AUCTION_SCAN_PLAIN: &str = include_str!("../scripts/auction_scan_plain.rhai");

/// The operations one hook may use, as the ADR's first guess.
const HOOK_BUDGET: u64 = 5_000;

// ---------------------------------------------------------------------------
// What a hook sees and does: one `Turn` per call, handed over as the call's tag.
// ---------------------------------------------------------------------------

/// The facts a hook reads and the actions it leaves behind.
struct Turn {
    actions: Vec<String>,
    /// The script's state: a shared object map, so a write through the
    /// resolver's value reaches it (the resolver's values are constants).
    state: Dynamic,
    riding: bool,
    slots: Array,
}

impl Default for Turn {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            state: Dynamic::from(Map::new()).into_shared(),
            riding: false,
            slots: Array::new(),
        }
    }
}

type Ctx = Arc<Mutex<Turn>>;

fn act(ctx: &Ctx, action: String) -> Outcome {
    ctx.lock().unwrap().actions.push(action);
    Outcome { ok: true, reason: "" }
}

/// What every action returns: `ok`, or a reason from a closed list.
#[derive(Clone)]
struct Outcome {
    ok: bool,
    reason: &'static str,
}

#[derive(Clone)]
struct Keys(Ctx);
#[derive(Clone)]
struct Mouse(Ctx);
#[derive(Clone)]
struct Input(Ctx, &'static str);
#[derive(Clone)]
struct Look(Ctx);
#[derive(Clone)]
struct Hotbar(Ctx);
#[derive(Clone)]
struct Vehicle(Ctx);
#[derive(Clone)]
struct Timer(Ctx);
#[derive(Clone)]
struct Bot(Ctx);
#[derive(Clone)]
struct Menu(Ctx);

/// A menu slot as a script reads it.
#[derive(Clone)]
struct Slot {
    index: INT,
    id: ImmutableString,
    lore: Vec<ImmutableString>,
}

/// "1,250", "12.5k" or "3M" as a whole number.
fn parse_number(text: &str) -> Option<INT> {
    let text = text.trim();
    let (digits, factor) = match text.chars().last()? {
        'k' | 'K' => (&text[..text.len() - 1], 1_000.0),
        'm' | 'M' => (&text[..text.len() - 1], 1_000_000.0),
        _ => (text, 1.0),
    };
    let plain: String = digits.chars().filter(|c| *c != ',').collect();
    let value: f64 = plain.parse().ok()?;
    Some((value * factor).round() as INT)
}

/// The names a script may use for the game objects and its state.
const OBJECTS: [&str; 10] = [
    "keys", "mouse", "look", "hotbar", "vehicle", "timer", "bot", "menu", "state", "chat",
];

fn object(name: &str, ctx: &Ctx) -> Option<Dynamic> {
    let ctx = ctx.clone();
    Some(match name {
        "keys" => Dynamic::from(Keys(ctx)),
        "mouse" => Dynamic::from(Mouse(ctx)),
        "look" => Dynamic::from(Look(ctx)),
        "hotbar" => Dynamic::from(Hotbar(ctx)),
        "vehicle" => Dynamic::from(Vehicle(ctx)),
        "timer" => Dynamic::from(Timer(ctx)),
        "bot" => Dynamic::from(Bot(ctx)),
        "menu" => Dynamic::from(Menu(ctx)),
        "state" => ctx.lock().unwrap().state.clone(),
        _ => return None,
    })
}

/// How the game objects reach a script.
#[derive(Clone, Copy, PartialEq)]
enum Binding {
    /// A variable resolver (`Engine::on_var`) builds them from the call's tag.
    Resolver,
    /// Constants pushed into the call's scope.
    ScopeConstants,
}

/// The operations the last evaluation used, as `on_progress` last saw them.
static OPERATIONS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The stop flag of the bot this thread runs, for the thread model.
    static STOP: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

fn stopped() -> bool {
    STOP.with(|stop| {
        stop.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    })
}

/// The sandboxed engine with the API registered.
fn engine(binding: Binding, max_operations: u64, count_operations: bool) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(max_operations);
    engine.set_max_call_levels(16);
    engine.set_max_expr_depths(32, 16);
    engine.set_max_string_size(1024);
    engine.set_max_array_size(256);
    engine.set_max_map_size(64);
    engine.set_max_variables(64);
    engine.set_max_functions(64);
    engine.disable_symbol("eval");
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});
    if count_operations {
        engine.on_progress(|operations| {
            OPERATIONS.store(operations, Ordering::Relaxed);
            stopped().then_some(Dynamic::UNIT)
        });
    }
    // A script may not declare a variable with a game object's name: the
    // resolver would answer for it anyway.
    engine.on_def_var(|_runtime, info, _context| Ok(!OBJECTS.contains(&info.name())));
    if binding == Binding::Resolver {
        engine.on_var(|name, _index, context| {
            if !OBJECTS.contains(&name) {
                return Ok(None);
            }
            let Some(ctx) = context.tag().clone().try_cast::<Ctx>() else {
                return Ok(None);
            };
            Ok(object(name, &ctx))
        });
    }

    engine
        .register_type_with_name::<Outcome>("outcome")
        .register_get("ok", |outcome: &mut Outcome| outcome.ok)
        .register_get("reason", |outcome: &mut Outcome| outcome.reason.to_owned());

    for key in ["forward", "back", "left", "right", "jump", "sneak", "sprint"] {
        engine.register_get(key, move |keys: &mut Keys| Input(keys.0.clone(), key));
    }
    for button in ["left", "right"] {
        engine.register_get(button, move |mouse: &mut Mouse| Input(mouse.0.clone(), button));
    }
    engine
        .register_type_with_name::<Keys>("keys")
        .register_type_with_name::<Mouse>("mouse")
        .register_type_with_name::<Input>("input")
        .register_fn("tap", |input: &mut Input| act(&input.0, format!("tap {}", input.1)))
        .register_fn("hold", |input: &mut Input| act(&input.0, format!("hold {}", input.1)))
        .register_fn("hold_for", |input: &mut Input, ms: INT| {
            act(&input.0, format!("hold {} for {ms}", input.1))
        })
        .register_fn("release", |input: &mut Input| {
            act(&input.0, format!("release {}", input.1))
        });
    engine
        .register_type_with_name::<Look>("look")
        .register_fn("at", |look: &mut Look, yaw: f64, pitch: f64| {
            act(&look.0, format!("look {yaw} {pitch}"))
        });
    engine
        .register_type_with_name::<Hotbar>("hotbar")
        .register_fn("select", |hotbar: &mut Hotbar, slot: INT| {
            act(&hotbar.0, format!("hotbar {slot}"))
        });
    engine
        .register_type_with_name::<Vehicle>("vehicle")
        .register_fn("mount", |vehicle: &mut Vehicle, kind: &str| {
            act(&vehicle.0, format!("mount {kind}"))
        });
    engine
        .register_type_with_name::<Timer>("timer")
        .register_fn("after", |timer: &mut Timer, ms: INT, name: &str| {
            act(&timer.0, format!("after {ms} {name}"))
        })
        .register_fn("every", |timer: &mut Timer, ms: INT, name: &str| {
            act(&timer.0, format!("every {ms} {name}"))
        });
    engine
        .register_type_with_name::<Bot>("bot")
        .register_get("riding", |bot: &mut Bot| bot.0.lock().unwrap().riding);
    engine
        .register_type_with_name::<Menu>("menu")
        .register_get("slots", |menu: &mut Menu| menu.0.lock().unwrap().slots.clone())
        .register_fn("click", |menu: &mut Menu, slot: INT| {
            act(&menu.0, format!("click {slot}"))
        });
    engine
        .register_type_with_name::<Slot>("slot")
        .register_get("index", |slot: &mut Slot| slot.index)
        .register_get("id", |slot: &mut Slot| slot.id.clone())
        .register_get("lore", |slot: &mut Slot| -> Array {
            slot.lore.iter().cloned().map(Dynamic::from).collect()
        })
        .register_fn("lore_number", |slot: &mut Slot, label: &str| -> Dynamic {
            slot.lore
                .iter()
                .find_map(|line| line.strip_prefix(label)?.strip_prefix(':'))
                .and_then(parse_number)
                .map_or(Dynamic::UNIT, Dynamic::from)
        });
    // The thread model's `wait`: parks the bot's thread, woken early by a stop.
    engine.register_fn("wait", |ms: INT| {
        let until = Instant::now() + Duration::from_millis(ms.unsigned_abs());
        while !stopped() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            thread::park_timeout(left);
        }
    });
    engine
}

/// Calls `hook` with `args`, the objects bound as `binding` says.
fn call(
    engine: &Engine,
    binding: Binding,
    ast: &AST,
    ctx: &Ctx,
    hook: &str,
    args: impl rhai::FuncArgs,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let mut scope = Scope::new();
    if binding == Binding::ScopeConstants {
        for name in OBJECTS {
            if let Some(value) = object(name, ctx) {
                scope.push_constant_dynamic(name, value);
            }
        }
    }
    let options = CallFnOptions::new()
        .eval_ast(false)
        .with_tag(Dynamic::from(ctx.clone()));
    engine.call_fn_with_options(options, &mut scope, ast, hook, args)
}

fn set_state(ctx: &Ctx, key: &str, value: INT) {
    let mut state = ctx.lock().unwrap().state.clone();
    state.write_lock::<Map>().unwrap().insert(key.into(), Dynamic::from(value));
}

fn get_state(ctx: &Ctx, key: &str) -> Option<Dynamic> {
    let state = ctx.lock().unwrap().state.clone();
    state.read_lock::<Map>().unwrap().get(key).cloned()
}

fn ctx() -> Ctx {
    Arc::new(Mutex::new(Turn::default()))
}

// ---------------------------------------------------------------------------
// Facts: what works, with Rhai's own words for what doesn't.
// ---------------------------------------------------------------------------

fn show<T: std::fmt::Debug>(label: &str, result: Result<T, String>) {
    match result {
        Ok(value) => println!("FACT ok   | {label} | {value:?}"),
        Err(error) => println!("FACT FAIL | {label} | {error}"),
    }
}

/// Compiles `source` and runs its `hook` once; the actions and the state after it.
fn run_once(binding: Binding, source: &str, hook: &str) -> Result<(Vec<String>, String), String> {
    let engine = engine(binding, HOOK_BUDGET, false);
    let ast = engine.compile(source).map_err(|error| format!("compile: {error}"))?;
    let ctx = ctx();
    call(&engine, binding, &ast, &ctx, hook, ())
        .map_err(|error| format!("run: {error} [position: {:?}]", error.position()))?;
    let turn = ctx.lock().unwrap();
    Ok((turn.actions.clone(), format!("{:?}", turn.state)))
}

fn facts() {
    println!("== facts (rhai 1.27.0: sync, no_module, no_time, no_custom_syntax) ==");

    for (name, binding) in [
        ("resolver", Binding::Resolver),
        ("scope constants", Binding::ScopeConstants),
    ] {
        show(
            &format!("{name}: a game object inside a hook"),
            run_once(binding, "fn on_join() { keys.forward.hold(); }", "on_join"),
        );
        show(
            &format!("{name}: a game object inside the script's own function"),
            run_once(
                binding,
                "fn on_join() { walk(); } fn walk() { keys.forward.hold_for(500); }",
                "on_join",
            ),
        );
        show(
            &format!("{name}: state set and added to inside the script's own function"),
            run_once(
                binding,
                "fn on_join() { state.n = 1; more(); more(); } fn more() { state.n += 1; }",
                "on_join",
            ),
        );
        show(
            &format!("{name}: state through an index"),
            run_once(binding, r#"fn on_join() { state["n"] = 5; state["n"] += 1; }"#, "on_join"),
        );
        show(
            &format!("{name}: a result tested, and one ignored"),
            run_once(
                binding,
                "fn on_join() { mouse.left.tap(); if mouse.right.tap().ok { keys.jump.tap(); } }",
                "on_join",
            ),
        );
        show(
            &format!("{name}: a script may not replace a game object"),
            run_once(binding, "fn on_join() { keys = 1; }", "on_join"),
        );
        show(
            &format!("{name}: a local variable may shadow one"),
            run_once(binding, "fn on_join() { let keys = 1; keys += 1; }", "on_join"),
        );
    }

    show(
        "resolver: a script may not replace its state",
        run_once(Binding::Resolver, "fn on_join() { state = 5; }", "on_join"),
    );
    show(
        "resolver: a parameter named like a game object",
        run_once(
            Binding::Resolver,
            "fn on_join() { f(1); } fn f(keys) { keys.forward.hold(); }",
            "on_join",
        ),
    );
    show(
        "resolver: state kept from one hook to the next",
        (|| -> Result<String, String> {
            let engine = engine(Binding::Resolver, HOOK_BUDGET, false);
            let ast = engine
                .compile("fn on_join() { state.n = 1; } fn on_respawn() { state.n += 41; }")
                .map_err(|error| error.to_string())?;
            let ctx = ctx();
            call(&engine, Binding::Resolver, &ast, &ctx, "on_join", ()).map_err(|e| e.to_string())?;
            call(&engine, Binding::Resolver, &ast, &ctx, "on_respawn", ()).map_err(|e| e.to_string())?;
            Ok(format!("{:?}", ctx.lock().unwrap().state))
        })(),
    );
    // Strict variables at compile time with placeholder constants, the
    // resolver at run time: do the actions reach the call's own turn?
    show(
        "strict compile with placeholders, then the resolver at run time",
        (|| -> Result<(Vec<String>, Vec<String>), String> {
            let mut engine = engine(Binding::Resolver, HOOK_BUDGET, false);
            engine.set_strict_variables(true);
            let placeholder = ctx();
            let mut scope = Scope::new();
            for name in OBJECTS {
                if let Some(value) = object(name, &placeholder) {
                    scope.push_constant_dynamic(name, value);
                }
            }
            let ast = engine
                .compile_with_scope(&scope, "fn on_join() { walk(); } fn walk() { keys.forward.hold(); state.n = 1; }")
                .map_err(|error| format!("compile: {error}"))?;
            let real = ctx();
            call(&engine, Binding::Resolver, &ast, &real, "on_join", ()).map_err(|e| e.to_string())?;
            Ok((real.lock().unwrap().actions.clone(), placeholder.lock().unwrap().actions.clone()))
        })(),
    );

    // State as `this`, the mechanism Rhai documents.
    let this_state = || -> Result<String, String> {
        let engine = engine(Binding::Resolver, HOOK_BUDGET, false);
        let source = "fn on_join() { this.n = 1; this.more(); more(); } fn more() { this.n += 1; }";
        let ast = engine.compile(source).map_err(|error| format!("compile: {error}"))?;
        let mut this = Dynamic::from(Map::new());
        let options = CallFnOptions::new().eval_ast(false).bind_this_ptr(&mut this);
        engine
            .call_fn_with_options::<Dynamic>(options, &mut Scope::new(), &ast, "on_join", ())
            .map_err(|error| format!("run: {error}; state {this:?}"))?;
        Ok(format!("{this:?}"))
    };
    show("state as `this`: a helper called plainly after one called as this.more()", this_state());

    // The minecart farm with helper functions, twice: on foot and seated.
    for binding in [Binding::Resolver, Binding::ScopeConstants] {
        let farm = || -> Result<Vec<String>, String> {
            let engine = engine(binding, HOOK_BUDGET, false);
            let ast = engine.compile(MINECART_FARM).map_err(|error| format!("compile: {error}"))?;
            let ctx = ctx();
            call(&engine, binding, &ast, &ctx, "on_join", (true,)).map_err(|e| e.to_string())?;
            ctx.lock().unwrap().riding = true;
            for _ in 0..3 {
                call(&engine, binding, &ast, &ctx, "on_timer", ("swing",))
                    .map_err(|e| e.to_string())?;
            }
            let turn = ctx.lock().unwrap();
            Ok(turn.actions.iter().cloned().chain([format!("{:?}", turn.state)]).collect())
        };
        show("the minecart farm: join on foot, then three swings seated", farm());
    }

    // What the sandbox refuses, and where.
    let compile = |source: &str| -> Result<&'static str, String> {
        engine(Binding::Resolver, HOOK_BUDGET, false)
            .compile(source)
            .map(|_| "compiled")
            .map_err(|error| error.to_string())
    };
    show("import", compile("fn on_join() {\n  import \"x\" as x;\n}"));
    show("eval", compile("fn on_join() {\n  eval(\"1 + 1\");\n}"));
    show("a syntax error on line 3", compile("fn on_join() {\n  let a = 1;\n  let b = ;\n}"));
    show("a closure that captures", compile("fn on_join() {\n  let a = 1;\n  let f = || a + 1;\n}"));
    show(
        "a misspelled method compiles",
        compile("fn on_join() {\n  keys.forward.hodl();\n}"),
    );
    show(
        "and fails where it runs",
        run_once(Binding::Resolver, "fn on_join() {\n  keys.forward.hodl();\n}", "on_join"),
    );
    show(
        "a misspelled object fails where it runs",
        run_once(Binding::Resolver, "fn on_join() {\n  kees.forward.hold();\n}", "on_join"),
    );
    let strict = |source: &str| -> Result<&'static str, String> {
        let mut engine = engine(Binding::ScopeConstants, HOOK_BUDGET, false);
        engine.set_strict_variables(true);
        let mut scope = Scope::new();
        let ctx = ctx();
        for name in OBJECTS {
            if let Some(value) = object(name, &ctx) {
                scope.push_constant_dynamic(name, value);
            }
        }
        engine
            .compile_with_scope(&scope, source)
            .map(|_| "compiled")
            .map_err(|error| error.to_string())
    };
    show(
        "strict variables: a known object inside a function",
        strict("fn on_join() {\n  keys.forward.hold();\n}"),
    );
    show(
        "strict variables: a misspelled object inside a function",
        strict("fn on_join() {\n  kees.forward.hold();\n}"),
    );
    for name in [
        "keys", "mouse", "look", "hotbar", "vehicle", "chat", "bot", "world", "timer", "random",
        "log", "state", "inventory", "menu", "move",
    ] {
        // A plain engine: is the word free in Rhai itself?
        if let Err(error) = Engine::new().compile(format!("fn f() {{ let {name} = 1; {name} }}")) {
            println!("FACT FAIL | `{name}` is a word Rhai keeps for itself | {error}");
        }
    }
    println!("FACT ok   | Rhai keeps none of the other proposed object names for itself");

    // Limits: each with Rhai's own error, and whether a script can catch it.
    show(
        "a busy loop",
        run_once(Binding::Resolver, "fn on_join() {\n  loop { }\n}", "on_join"),
    );
    show(
        "a busy loop inside try/catch",
        run_once(
            Binding::Resolver,
            "fn on_join() {\n  try { loop { } } catch { keys.jump.tap(); }\n  keys.sneak.tap();\n}",
            "on_join",
        ),
    );
    show(
        "endless recursion",
        run_once(Binding::Resolver, "fn on_join() {\n  on_join();\n}", "on_join"),
    );
    show(
        "a string that grows",
        run_once(
            Binding::Resolver,
            "fn on_join() {\n  let s = \"x\";\n  loop { s += s; }\n}",
            "on_join",
        ),
    );
    show(
        "an array that grows",
        run_once(
            Binding::Resolver,
            "fn on_join() {\n  let a = [];\n  loop { a.push(1); }\n}",
            "on_join",
        ),
    );
    show(
        "a state that grows past the map limit",
        run_once(
            Binding::Resolver,
            "fn on_join() {
  for i in 0..100 { state[`k${i}`] = i; }
}",
            "on_join",
        ),
    );
    show(
        "a hook that isn't defined",
        run_once(Binding::Resolver, "fn on_join() { }", "on_respawn"),
    );
}

// ---------------------------------------------------------------------------
// Numbers.
// ---------------------------------------------------------------------------

struct Meter {
    system: System,
    pid: Pid,
}

impl Meter {
    fn new() -> Self {
        Self {
            system: System::new().unwrap(),
            pid: sysinfo::get_current_pid().unwrap(),
        }
    }

    /// The CPU time used so far, the memory held and the virtual memory.
    fn read(&mut self) -> (Duration, u64, u64) {
        let what = ProcessRefreshKind::nothing().with_cpu().with_memory();
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::Some(&[self.pid]), true, what);
        let process = self.system.process(self.pid).expect("this process");
        (
            Duration::from_millis(process.accumulated_cpu_time()),
            process.memory(),
            process.virtual_memory(),
        )
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// A page of 54 listings, every one the item the scan looks for.
fn auction_page() -> Array {
    (0..54)
        .map(|index: INT| {
            let price = 900 + (index * 7919) % 5000;
            let shown = if index % 3 == 0 {
                format!("Price: {}.{}k", price / 1000, (price % 1000) / 100)
            } else {
                format!("Price: {},{:03}", price / 1000, price % 1000)
            };
            Dynamic::from(Slot {
                index,
                id: "minecraft:diamond".into(),
                lore: vec!["Seller: AfkBot1".into(), shown.into(), "Click to buy".into()],
            })
        })
        .collect()
}

fn numbers() {
    println!("== numbers (one thread, release build) ==");

    // Operations a second: a counting loop, with and without the progress hook.
    for counted in [false, true] {
        let engine = engine(Binding::Resolver, 0, counted);
        let ast = engine
            .compile("fn run() { let x = 0; while x < 1_000_000 { x += 1; } x }")
            .unwrap();
        let started = Instant::now();
        let _: Dynamic = call(&engine, Binding::Resolver, &ast, &ctx(), "run", ()).unwrap();
        let took = started.elapsed();
        if counted {
            let operations = OPERATIONS.load(Ordering::Relaxed);
            println!(
                "NUMBER | a loop of 1,000,000 additions, counted | {took:?} | {operations} operations | {:.1} million operations a second",
                operations as f64 / took.as_secs_f64() / 1e6
            );
        } else {
            println!("NUMBER | a loop of 1,000,000 additions, no progress hook | {took:?}");
        }
    }

    // Compiling a script of 16 KiB.
    let mut big = String::new();
    let mut copy = 0;
    while big.len() < 16 * 1024 {
        big.push_str(&MINECART_FARM.replace("fn ", &format!("fn c{copy}_")));
        copy += 1;
    }
    let mut unlimited = engine(Binding::Resolver, HOOK_BUDGET, false);
    unlimited.set_max_functions(10_000);
    let started = Instant::now();
    for _ in 0..50 {
        unlimited.compile(&big).unwrap();
    }
    println!(
        "NUMBER | compiling a script of {} bytes ({} functions) | {:?} each",
        big.len(),
        copy * 5,
        started.elapsed() / 50
    );

    // One hook call, for both bindings.
    for (name, binding) in [
        ("resolver", Binding::Resolver),
        ("scope constants", Binding::ScopeConstants),
    ] {
        let engine = engine(binding, HOOK_BUDGET, true);
        let source = format!("{MINECART_FARM}\nfn on_death() {{ }}");
        let ast = engine.compile(&source).unwrap();
        let ctx = ctx();
        ctx.lock().unwrap().riding = true;
        set_state(&ctx, "swings", 0);
        for (label, hook, arg) in [("an empty hook", "on_death", None), ("the farm's swing", "on_timer", Some("swing"))] {
            // The farm's functions can't see scope constants (see the facts).
            if binding == Binding::ScopeConstants && arg.is_some() {
                continue;
            }
            const CALLS: u32 = 200_000;
            let started = Instant::now();
            for _ in 0..CALLS {
                match arg {
                    Some(arg) => call(&engine, binding, &ast, &ctx, hook, (arg,)).unwrap(),
                    None => call(&engine, binding, &ast, &ctx, hook, ()).unwrap(),
                };
                ctx.lock().unwrap().actions.clear();
            }
            println!(
                "NUMBER | {name}: {label} | {:?} a call | {} operations",
                started.elapsed() / CALLS,
                OPERATIONS.load(Ordering::Relaxed)
            );
        }
    }

    // The auction page's scan.
    for (label, source) in [
        ("with the native number helper", AUCTION_SCAN),
        ("with Rhai's own string functions", AUCTION_SCAN_PLAIN),
    ] {
        let engine = engine(Binding::Resolver, 0, true);
        let ast = engine.compile(source).unwrap();
        let ctx = ctx();
        ctx.lock().unwrap().slots = auction_page();
        const CALLS: u32 = 2_000;
        let started = Instant::now();
        for _ in 0..CALLS {
            call(&engine, Binding::Resolver, &ast, &ctx, "on_menu_update", ()).unwrap();
        }
        let each = started.elapsed() / CALLS;
        let turn = ctx.lock().unwrap();
        println!(
            "NUMBER | scanning 54 slots {label} | {each:?} a call | {} operations | last action: {:?}",
            OPERATIONS.load(Ordering::Relaxed),
            turn.actions.last()
        );
    }

    // How long a busy loop runs before the budget ends it.
    let engine = engine(Binding::Resolver, HOOK_BUDGET, false);
    let ast = engine.compile("fn on_join() { let x = 0; loop { x += 1; } }").unwrap();
    let started = Instant::now();
    for _ in 0..1_000 {
        call(&engine, Binding::Resolver, &ast, &ctx(), "on_join", ()).unwrap_err();
    }
    println!(
        "NUMBER | a busy loop, ended by the budget of {HOOK_BUDGET} operations | {:?}",
        started.elapsed() / 1_000
    );
}

/// The tick model: hooks run to their end on one thread, a timer every 250 ms a bot.
fn ticks(bots: usize, seconds: u64) {
    println!("== hooks that run to their end: {bots} bots, one thread, {seconds} s ==");
    let mut meter = Meter::new();
    let (_, before, before_virtual) = meter.read();
    let engine = engine(Binding::Resolver, HOOK_BUDGET, false);
    let ast = engine.compile(MINECART_FARM).unwrap();
    let instances: Vec<Ctx> = (0..bots)
        .map(|_| {
            let ctx = ctx();
            call(&engine, Binding::Resolver, &ast, &ctx, "on_join", (true,)).unwrap();
            ctx.lock().unwrap().riding = true;
            ctx
        })
        .collect();
    let (_, with_instances, with_virtual) = meter.read();
    println!(
        "TICKS | {bots} instances | memory +{:.2} MiB | virtual +{:.2} MiB | threads +0",
        mib(with_instances.saturating_sub(before)),
        mib(with_virtual.saturating_sub(before_virtual)),
    );

    // Idle: nothing is due, so nothing runs.
    let (cpu_before, ..) = meter.read();
    thread::sleep(Duration::from_secs(seconds));
    let (cpu_after, ..) = meter.read();
    println!(
        "TICKS | idle for {seconds} s | CPU {:?}",
        cpu_after.saturating_sub(cpu_before)
    );

    // A timer every 250 ms a bot.
    let (cpu_before, ..) = meter.read();
    let started = Instant::now();
    let mut hooks = 0u64;
    let mut busy = Duration::ZERO;
    let mut next = started;
    while started.elapsed() < Duration::from_secs(seconds) {
        let turn = Instant::now();
        for ctx in &instances {
            call(&engine, Binding::Resolver, &ast, ctx, "on_timer", ("swing",)).unwrap();
            ctx.lock().unwrap().actions.clear();
            hooks += 1;
        }
        busy += turn.elapsed();
        next += Duration::from_millis(250);
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    let (cpu_after, memory, _) = meter.read();
    println!(
        "TICKS | a timer every 250 ms a bot for {seconds} s | {hooks} hooks | CPU {:?} ({:.2} % of a core) | in hooks {busy:?} | memory {:.2} MiB",
        cpu_after.saturating_sub(cpu_before),
        cpu_after.saturating_sub(cpu_before).as_secs_f64() / seconds as f64 * 100.0,
        mib(memory),
    );
}

/// The thread model: a thread a bot, its script waiting in `wait()`.
fn threads(bots: usize, seconds: u64) {
    println!("== a thread a bot with wait(): {bots} bots, {seconds} s ==");
    for (label, body) in [
        ("idle in one long wait", "fn run() { loop { wait(3_600_000); } }".to_owned()),
        (
            "a wait of 250 ms and the farm's swing",
            format!("{MINECART_FARM}\nfn run() {{ loop {{ wait(250); swing(); }} }}"),
        ),
        ("a busy loop", "fn run() { let x = 0; loop { x += 1; } }".to_owned()),
    ] {
        let mut meter = Meter::new();
        let (_, before, before_virtual) = meter.read();
        let engine = Arc::new(engine(Binding::Resolver, 0, true));
        let ast = Arc::new(engine.compile(&body).unwrap());
        let busy = label == "a busy loop";
        let count = if busy { 1 } else { bots };
        let stops: Vec<Arc<AtomicBool>> =
            (0..count).map(|_| Arc::new(AtomicBool::new(false))).collect();
        let handles: Vec<_> = stops
            .iter()
            .map(|stop| {
                let (engine, ast, stop) = (engine.clone(), ast.clone(), stop.clone());
                thread::spawn(move || {
                    STOP.with(|slot| *slot.borrow_mut() = Some(stop));
                    let ctx = ctx();
                    ctx.lock().unwrap().riding = true;
                    set_state(&ctx, "swings", 0);
                    let result = call(&engine, Binding::Resolver, &ast, &ctx, "run", ());
                    let swings = get_state(&ctx, "swings");
                    (result.err().map(|error| error.to_string()), swings)
                })
            })
            .collect();
        thread::sleep(Duration::from_millis(500));
        let (cpu_before, memory, virtual_memory) = meter.read();
        if busy {
            let asked = Instant::now();
            stops[0].store(true, Ordering::Relaxed);
            for handle in handles {
                let (error, _) = handle.join().unwrap();
                println!(
                    "THREADS | {label} | stopped {:?} after the flag | {error:?}",
                    asked.elapsed()
                );
            }
            continue;
        }
        println!(
            "THREADS | {label} | threads +{count} | memory +{:.2} MiB | virtual +{:.2} MiB",
            mib(memory.saturating_sub(before)),
            mib(virtual_memory.saturating_sub(before_virtual)),
        );
        thread::sleep(Duration::from_secs(seconds));
        let (cpu_after, ..) = meter.read();
        let asked = Instant::now();
        for stop in &stops {
            stop.store(true, Ordering::Relaxed);
        }
        let mut swings = 0;
        let mut last_error = None;
        for handle in handles {
            handle.thread().unpark();
            let (error, count) = handle.join().unwrap();
            swings += count.and_then(|count| count.as_int().ok()).unwrap_or(0);
            last_error = error;
        }
        println!(
            "THREADS | {label} for {seconds} s | {swings} swings | CPU {:?} ({:.2} % of a core) | all stopped in {:?} | {last_error:?}",
            cpu_after.saturating_sub(cpu_before),
            cpu_after.saturating_sub(cpu_before).as_secs_f64() / seconds as f64 * 100.0,
            asked.elapsed(),
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map_or("all", String::as_str);
    let bots = args.get(1).and_then(|n| n.parse().ok()).unwrap_or(200);
    let seconds = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(10);
    match command {
        "facts" => facts(),
        "numbers" => numbers(),
        "ticks" => ticks(bots, seconds),
        "threads" => threads(bots, seconds),
        "all" => {
            facts();
            numbers();
            ticks(bots, seconds);
            threads(bots, seconds);
        }
        other => eprintln!("unknown command `{other}`: facts, numbers, ticks, threads or all"),
    }
}
