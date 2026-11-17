//! The sandboxed Rhai engine: what a script's language holds and every
//! limit it runs under (ADR-0036, D3; ADR-0040).
//!
//! **An engine a script instance, over shared packages.** Rhai's progress
//! callback gets only an operation count, with no way to say whose hook is
//! running, so the callback, the variable resolver and the operation limit
//! are each instance's own. What costs something to build, the packages
//! and the API's functions, is built once and shared.
//!
//! **The packages are picked by hand.** `Engine::new()` would bring Rhai's
//! language-core package with it, whose `sleep` blocks the thread that
//! runs the hook, and `exit`, `parse_json`, blobs and bit fields, which no
//! script needs. A script gets arithmetic, logic, text, numbers, arrays,
//! maps and ranges.
//!
//! **No anonymous functions.** A closure that captures a variable makes it
//! a shared value, and Rhai waits 50 ms on a shared value that is locked,
//! each time a script touches one and catches the error. Without them the
//! state is a script's only shared value, and its resolver refuses at once
//! while it's in use ([`crate::state`]).

use std::sync::LazyLock;

use fleet_core::script::limits;
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMapPackage, BasicMathPackage,
    BasicStringPackage, LogicPackage, MoreStringPackage, Package,
};
use rhai::{Dynamic, Engine, EvalAltResult, EvalContext, Module, OptimizationLevel, Shared};

use crate::api;

/// The name of the state object.
pub(crate) const STATE: &str = "state";

/// The names a script can't give a variable or a parameter: the game
/// objects and the state. A variable resolver answers for them, so one of
/// the script's own would silently lose.
pub(crate) fn is_reserved(name: &str) -> bool {
    name == STATE || api::OBJECTS.contains(&name)
}

/// The most variables alive at once in one function. Not in ADR-0036's
/// table: Rhai's own default is far higher, and each variable can hold a
/// value as large as the data limits allow (ADR-0040).
pub(crate) const MAX_VARIABLES: usize = 64;

/// Rhai's standard packages a script gets, built once.
static PACKAGES: LazyLock<[Shared<Module>; 8]> = LazyLock::new(|| {
    [
        ArithmeticPackage::new().as_shared_module(),
        LogicPackage::new().as_shared_module(),
        BasicStringPackage::new().as_shared_module(),
        MoreStringPackage::new().as_shared_module(),
        BasicMathPackage::new().as_shared_module(),
        BasicIteratorPackage::new().as_shared_module(),
        BasicArrayPackage::new().as_shared_module(),
        BasicMapPackage::new().as_shared_module(),
    ]
});

/// An engine with the language, the API's functions and every limit, and
/// nothing bound to it yet.
fn sandbox() -> Engine {
    let mut engine = Engine::new_raw();
    for package in PACKAGES.iter() {
        engine.register_global_module(package.clone());
    }
    engine.register_global_module(api::module());

    // Never 0: to Rhai, 0 is "no limit" for each of these.
    engine.set_max_operations(limits::OPERATIONS_PER_HOOK);
    engine.set_max_call_levels(limits::MAX_CALL_DEPTH);
    engine.set_max_expr_depths(
        limits::MAX_EXPRESSION_DEPTH,
        limits::MAX_FUNCTION_EXPRESSION_DEPTH,
    );
    engine.set_max_functions(limits::MAX_FUNCTIONS);
    engine.set_max_variables(MAX_VARIABLES);
    engine.set_max_string_size(limits::MAX_STRING_BYTES);
    engine.set_max_array_size(limits::MAX_ARRAY_ITEMS);
    engine.set_max_map_size(limits::MAX_MAP_ENTRIES);

    engine.set_fail_on_invalid_map_property(true);
    engine.set_allow_anonymous_fn(false);
    engine.disable_symbol("eval");
    // `print` and `debug` go nowhere: a script's only output is its log.
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});
    engine
}

/// What answers for a reserved name where a script uses it.
pub(crate) type Resolver =
    dyn Fn(&str) -> Result<Option<Dynamic>, Box<EvalAltResult>> + Send + Sync;

/// Binds the reserved names: `resolver` answers for them where a script
/// uses one, and no script defines a variable of such a name.
///
/// Rhai marks both callbacks `#[deprecated]` to say their API is volatile,
/// not that it's going away. The variable resolver is the one mechanism
/// that hands a script's own functions the game objects and its state
/// (ADR-0036, D4), so the pin on Rhai is exact, and the tests of an object
/// inside a helper function fail at the first bump that changes it.
#[expect(
    deprecated,
    reason = "Rhai marks on_var and on_def_var as volatile, not as deprecated; ADR-0036 D4, ADR-0040"
)]
fn bind(engine: &mut Engine, resolver: Option<Box<Resolver>>) {
    engine.on_def_var(|_, definition, _| Ok(!is_reserved(definition.name())));
    if let Some(resolver) = resolver {
        engine.on_var(move |name, _, _: EvalContext<'_, '_, '_, '_, '_, '_>| {
            if is_reserved(name) {
                resolver(name)
            } else {
                Ok(None)
            }
        });
    }
}

/// The engine that reads a script when it's saved: strict about every
/// variable's name, and never optimizing.
///
/// Compiling against placeholders makes a misspelled object an error at
/// its line. The optimizer is off because it would copy a placeholder into
/// the program wherever it can prove a name constant, and the script would
/// then act on the placeholder instead of its own bot (ADR-0040).
pub(crate) fn compiler() -> Engine {
    let mut engine = sandbox();
    engine.set_strict_variables(true);
    engine.set_optimization_level(OptimizationLevel::None);
    bind(&mut engine, None);
    engine
}

/// The engine one instance runs its hooks with: `resolver` answers for the
/// game objects and the state, and `progress` sees every operation's
/// number and may stop the hook with a token.
pub(crate) fn runner(
    resolver: Box<Resolver>,
    progress: impl Fn(u64) -> Option<Dynamic> + Send + Sync + 'static,
) -> Engine {
    let mut engine = sandbox();
    bind(&mut engine, Some(resolver));
    engine.on_progress(progress);
    engine
}

#[cfg(test)]
mod tests {
    use rhai::Scope;

    use super::*;

    /// A raw engine with the standard packages a script gets, for the
    /// facts about Rhai itself that the sandbox relies on.
    fn plain() -> Engine {
        let mut engine = Engine::new_raw();
        for package in PACKAGES.iter() {
            engine.register_global_module(package.clone());
        }
        engine
    }

    const LOOP: &str = "let n = 0; for i in 0..5000 { n += 1; } n";

    // The user's reviewer's finding 2, pinned for the next bump: to Rhai,
    // 0 means "no limit". A budget that is computed must never reach it.
    #[test]
    fn rhai_takes_zero_operations_for_no_limit() {
        let mut engine = plain();

        engine.set_max_operations(0);
        assert_eq!(engine.max_operations(), 0);
        assert_eq!(engine.eval::<i64>(LOOP).unwrap(), 5000);

        engine.set_max_operations(1);
        assert!(matches!(
            *engine.eval::<i64>(LOOP).unwrap_err(),
            EvalAltResult::ErrorTooManyOperations(_)
        ));
    }

    #[test]
    fn rhai_takes_zero_for_no_limit_on_texts_arrays_and_maps() {
        let mut engine = plain();
        let text = "let s = \"x\"; for i in 0..12 { s += s; } s.len";
        let array = "let a = []; a.pad(3000, 1); a.len";

        engine.set_max_string_size(8);
        engine.set_max_array_size(8);
        assert!(engine.eval::<i64>(text).is_err());
        assert!(engine.eval::<i64>(array).is_err());

        engine.set_max_string_size(0);
        engine.set_max_array_size(0);
        engine.set_max_map_size(0);
        assert_eq!(engine.eval::<i64>(text).unwrap(), 4096);
        assert_eq!(engine.eval::<i64>(array).unwrap(), 3000);
        assert_eq!(
            (
                engine.max_string_size(),
                engine.max_array_size(),
                engine.max_map_size()
            ),
            (0, 0, 0)
        );
    }

    #[test]
    fn rhai_takes_zero_for_no_limit_on_an_expressions_depth() {
        let mut engine = plain();
        let deep = format!("{}1{}", "(".repeat(40), ")".repeat(40));

        engine.set_max_expr_depths(8, 8);
        assert!(engine.compile(&deep).is_err());

        engine.set_max_expr_depths(0, 0);
        assert!(engine.compile(&deep).is_ok());
    }

    #[test]
    fn no_limit_of_the_sandbox_is_zero() {
        let engine = sandbox();

        assert_eq!(engine.max_operations(), 10_000);
        assert_eq!(engine.max_call_levels(), 16);
        assert_eq!(engine.max_expr_depth(), 32);
        assert_eq!(engine.max_function_expr_depth(), 16);
        assert_eq!(engine.max_functions(), 64);
        assert_eq!(engine.max_variables(), 64);
        assert_eq!(engine.max_string_size(), 1024);
        assert_eq!(engine.max_array_size(), 256);
        assert_eq!(engine.max_map_size(), 64);
    }

    // Rhai's text limit is in bytes.
    #[test]
    fn rhais_text_limit_counts_bytes() {
        let mut engine = plain();
        engine.set_max_string_size(10);

        assert_eq!(
            engine.eval::<i64>("let s = \"abcdefghij\"; s.len").unwrap(),
            10
        );
        // Six letters of two bytes each.
        assert!(
            engine
                .eval::<i64>("let s = \"\u{e4}\u{e4}\u{e4}\"; s += s; s.len")
                .is_err()
        );
    }

    #[test]
    fn the_sandbox_has_no_function_that_blocks_ends_or_reads_outside() {
        let engine = sandbox();

        for call in [
            "sleep(1)",
            "sleep(0.5)",
            "exit()",
            "exit(1)",
            "timestamp()",
            "parse_json(\"{}\")",
            "blob(10)",
            "let a = [1]; take(a)",
        ] {
            let error = engine.eval::<Dynamic>(call).unwrap_err();

            assert!(
                matches!(*error, EvalAltResult::ErrorFunctionNotFound(..)),
                "{call}: {error}"
            );
        }
    }

    #[test]
    fn the_sandbox_has_no_eval_import_or_anonymous_function() {
        let engine = sandbox();

        for source in [
            "eval(\"1\")",
            "import \"x\" as x;",
            "let f = |x| x + 1;",
            "let f = || 1;",
        ] {
            assert!(engine.compile(source).is_err(), "{source}");
        }
    }

    // The user's reviewer's finding 6, pinned: `eval` can't be reached
    // through a function pointer either.
    #[test]
    fn eval_cant_be_called_through_a_function_pointer() {
        let engine = sandbox();

        for source in [
            "Fn(\"eval\").call(\"1 + 1\")",
            "call(Fn(\"eval\"), \"1 + 1\")",
        ] {
            let error = engine.eval::<Dynamic>(source).unwrap_err();

            assert!(
                error
                    .to_string()
                    .starts_with("Syntax error: 'eval' is a reserved keyword"),
                "{source}: {error}"
            );
        }
    }

    #[test]
    fn print_and_debug_reach_nothing() {
        let engine = sandbox();

        assert!(engine.run("print(\"x\"); debug(42); print(1.5);").is_ok());
    }

    #[test]
    fn a_script_gets_arithmetic_text_arrays_maps_and_ranges() {
        let engine = sandbox();

        assert_eq!(
            engine
                .eval::<i64>("let a = [3, 1, 2]; a.sort(); a[0] + a.len")
                .unwrap(),
            4
        );
        assert_eq!(
            engine
                .eval::<i64>("let m = #{ a: 1 }; m.b = 2; m.len()")
                .unwrap(),
            2
        );
        assert_eq!(
            engine.eval::<String>("`n is ${1 + 2}`.to_upper()").unwrap(),
            "N IS 3"
        );
        assert_eq!(
            engine
                .eval::<i64>("let n = 0; for i in 0..10 { n += i; } n")
                .unwrap(),
            45
        );
        assert_eq!(
            engine
                .eval::<i64>("parse_int(\"12\") + 12.7.to_int()")
                .unwrap(),
            24
        );
        assert!(engine.eval::<bool>("12500.5 - 10000 >= 1").unwrap());
    }

    #[test]
    fn the_compiler_knows_only_the_names_it_is_given() {
        let engine = compiler();
        let mut scope = Scope::new();
        scope.push_constant("keys", 1_i64);

        assert!(engine.compile_with_scope(&scope, "fn f() { keys }").is_ok());
        assert!(
            engine
                .compile_with_scope(&scope, "fn f() { kees }")
                .is_err()
        );
        assert!(
            engine
                .compile_with_scope(&scope, "fn f() { let keys = 1; }")
                .is_err()
        );
        assert!(
            engine
                .compile_with_scope(&scope, "fn f() { let state = 1; }")
                .is_err()
        );
        assert!(
            engine
                .compile_with_scope(&scope, "fn f() { let other = 1; other }")
                .is_ok()
        );
    }

    /// How many bytes of its thread's stack a script takes whose function
    /// `down` calls itself until the call limit ends it. `None` if it
    /// doesn't compile, or doesn't end at the call limit.
    fn stack_of(source: &str) -> Option<usize> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let ast = compiler().compile(source).ok()?;
        let lowest = Arc::new(AtomicUsize::new(usize::MAX));
        let seen = Arc::clone(&lowest);
        // Where the stack is at every operation: the address of a local.
        let engine = runner(Box::new(|_| Ok(None)), move |_| {
            let here = 0_u8;
            seen.fetch_min((&raw const here).addr(), Ordering::Relaxed);
            None
        });
        let top = 0_u8;
        let top = (&raw const top).addr();

        let ended = engine.call_fn::<Dynamic>(&mut Scope::new(), &ast, "down", ());

        let overflowed = matches!(
            ended.as_ref().map_err(|error| error.unwrap_inner()),
            Err(EvalAltResult::ErrorStackOverflow(_))
        );
        overflowed.then(|| top - lowest.load(Ordering::Relaxed))
    }

    /// The ways a function nests the call that goes one level down: each
    /// is what stands before the call and what stands after it, to be
    /// repeated as often as still compiles.
    const NESTS: [(&str, &str, &str, &str); 9] = [
        ("parentheses", "(", "down()", ")"),
        ("blocks", "if true { ", "down();", " }"),
        ("loops", "for i in 0..1 { ", "down();", " }"),
        ("arguments", "pass(", "down()", ")"),
        ("texts", "`${", "down()", "}`"),
        ("arrays", "[", "down()", "]"),
        ("sums", "1 + (", "down()", ")"),
        (
            "functions handed to an array's method",
            "(",
            "[1].map(Fn(\"down\"))",
            ")",
        ),
        ("functions called by name", "(", "call(Fn(\"down\"))", ")"),
    ];

    /// The most stack any of [`NESTS`] takes, each at the deepest it
    /// compiles, with its name and how deep that is.
    fn deepest_script() -> (&'static str, usize, usize) {
        NESTS
            .iter()
            .filter_map(|(name, open, call, close)| {
                (0..=12).rev().find_map(|depth| {
                    let source = format!(
                        "fn down() {{ {}{call}{} }}\nfn pass(x) {{ x }}",
                        open.repeat(depth),
                        close.repeat(depth)
                    );
                    stack_of(&source).map(|used| (*name, depth, used))
                })
            })
            .max_by_key(|(_, _, used)| *used)
            .unwrap()
    }

    /// The stack a thread of the async runtime has, and so a hook.
    const RUNTIME_THREAD_STACK: usize = 2 * 1024 * 1024;

    // What Rhai's own limits let a script take of the stack, measured in
    // whatever build the tests are: 0.3 MiB in a release build, 1.8 MiB in
    // a debug build (ADR-0040). The limits count levels, and a level's
    // size is the build's; so the instance measures the stack itself and
    // ends a hook at half a runtime thread's (`instance::STACK_BUDGET`).
    // This holds only what must be true in every build: that some nest
    // is possible, and that what it takes isn't the whole thread's.
    #[test]
    fn what_the_deepest_script_takes_of_the_stack_is_measured() {
        let (name, depth, used) = std::thread::Builder::new()
            .stack_size(16 * RUNTIME_THREAD_STACK)
            .spawn(deepest_script)
            .unwrap()
            .join()
            .unwrap();

        println!(
            "STACK the deepest script: {} KiB, {depth} nested {name} in each of {} calls",
            used / 1024,
            limits::MAX_CALL_DEPTH + 1
        );
        assert!(depth >= 2, "{name}: {depth}");
        assert!(used < RUNTIME_THREAD_STACK, "{name}: {used} bytes");
    }

    #[test]
    fn every_way_to_nest_a_call_ends_at_the_call_limit() {
        let measured = std::thread::Builder::new()
            .stack_size(16 * RUNTIME_THREAD_STACK)
            .spawn(|| {
                NESTS
                    .iter()
                    .map(|(name, open, call, close)| {
                        let source =
                            format!("fn down() {{ {open}{call}{close} }}\nfn pass(x) {{ x }}");
                        let used = stack_of(&source);
                        println!(
                            "STACK one level of {name}: {:?} KiB",
                            used.map(|used| used / 1024)
                        );
                        (*name, used)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap()
            .join()
            .unwrap();

        for (name, used) in measured {
            assert!(used.is_some(), "{name}");
        }
    }

    #[test]
    fn every_game_object_and_the_state_is_reserved() {
        for name in [
            "keys", "mouse", "look", "hotbar", "vehicle", "chat", "timer", "random", "log", "bot",
            "world", "state",
        ] {
            assert!(is_reserved(name), "{name}");
        }
        assert!(!is_reserved("key"));
        assert!(!is_reserved("State"));
    }
}
