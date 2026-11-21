//! [`compile`]: a script checked and turned into a [`Program`] (P15.6;
//! ADR-0036, D14).

use std::sync::LazyLock;

use fleet_core::script::{Hook, Script};
use fleet_core::text;
use rhai::{AST, Dynamic, Engine, LexError, ParseError, ParseErrorType, Position, Scope};

use crate::api;
use crate::engine::{self, STATE};
use crate::error::{CompileError, CompileErrorKind, SourcePosition};
use crate::scan::{Scanned, scan};

/// The engine every script is compiled with.
static COMPILER: LazyLock<Engine> = LazyLock::new(engine::compiler);

/// What stands for a game object while a script is compiled: a value of a
/// type of its own, so nothing a script does with it has a meaning.
#[derive(Debug, Clone)]
struct Placeholder;

/// A script that passed every check, compiled: what an [`Instance`] runs.
///
/// It holds the script it was compiled from, with the mode's settings and
/// declarations, so an instance needs nothing else. Compiled once, it's
/// shared by every bot that runs the mode.
///
/// [`Instance`]: crate::Instance
#[derive(Debug, Clone)]
pub struct Program {
    ast: AST,
    script: Script,
    /// Which hooks the script defines, in [`Hook::ALL`]'s order.
    hooks: [bool; Hook::ALL.len()],
}

impl Program {
    /// The script it was compiled from: its source, its leash and tool
    /// guard, its declared messages and chat patterns.
    #[must_use]
    pub const fn script(&self) -> &Script {
        &self.script
    }

    /// Whether the script defines `hook`.
    #[must_use]
    pub fn defines(&self, hook: Hook) -> bool {
        Hook::ALL
            .iter()
            .zip(self.hooks)
            .any(|(known, defined)| *known == hook && defined)
    }

    /// The compiled functions.
    pub(crate) const fn ast(&self) -> &AST {
        &self.ast
    }
}

/// Checks `script`'s source and compiles it (ADR-0036, D14).
///
/// What the type [`Script`] already holds isn't checked again: the
/// source's size and characters, the settings, every declared message and
/// pattern. This checks the program:
/// - it's Rhai as the sandbox has it: no `import`, no `eval`, no
///   anonymous function, within the limits for functions and nesting;
///   a text, an array or a map written out is within its limit too;
/// - every name it uses is a variable of its own or a game object;
/// - no variable, parameter, loop variable or caught error is named like
///   a game object or `state`;
/// - a function whose name starts with `on_` is a hook, with that hook's
///   number of parameters;
/// - nothing stands outside a function: it would never run.
///
/// A misspelled method isn't found: Rhai looks a method up when the line
/// runs.
///
/// Compiling takes about half a millisecond for the longest script; the
/// server and the agent run it off the async runtime all the same.
///
/// # Errors
/// [`CompileError`] for the first problem, with its line and column where
/// they're known. Rhai's own findings come first, in the order of the
/// text; then what stands outside a function, a loop variable's or a
/// caught error's name, and the functions in the order they're written.
pub fn compile(script: &Script) -> Result<Program, CompileError> {
    let source = script.source().as_str();
    let mut scope = Scope::new();
    for name in api::OBJECTS.into_iter().chain([STATE]) {
        scope.push_constant_dynamic(name, Dynamic::from(Placeholder));
    }
    let scanned = scan(source);
    let ast = COMPILER
        .compile_with_scope(&scope, source)
        .map_err(|error| before_raw_string(parse_error(&error), scanned.raw_string))?;
    if let Some(at) = scanned.raw_string {
        return Err(CompileError {
            kind: CompileErrorKind::RawString,
            at: Some(at),
        });
    }

    // Rhai's public API shows a program's statements only as a slice of a
    // type it doesn't name; its length is all that's needed.
    if !AsRef::<[_]>::as_ref(&ast).is_empty() {
        return Err(CompileError {
            kind: CompileErrorKind::TopLevelStatement,
            at: scanned.outside,
        });
    }
    if let Some(binding) = scanned
        .bindings
        .iter()
        .find(|binding| engine::is_reserved(&binding.name))
    {
        return Err(CompileError {
            kind: CompileErrorKind::ReservedName {
                name: binding.name.clone(),
            },
            at: Some(binding.at),
        });
    }
    let hooks = functions(&ast, &scanned)?;
    Ok(Program {
        ast,
        script: script.clone(),
        hooks,
    })
}

/// Checks every function of the program and says which hooks it defines.
fn functions(ast: &AST, scanned: &Scanned) -> Result<[bool; Hook::ALL.len()], CompileError> {
    // In the order they're written, so the first problem in the text is
    // the one reported; Rhai lists them in no order.
    let mut functions: Vec<_> = ast
        .iter_functions()
        .map(|function| {
            let at = scanned.definition(function.name, function.params.len());
            (at, function)
        })
        .collect();
    functions.sort_by_key(|(at, function)| {
        (
            at.map_or((u32::MAX, u32::MAX), |at| (at.line, at.column)),
            function.params.len(),
        )
    });

    let mut hooks = [false; Hook::ALL.len()];
    for (at, function) in functions {
        if let Some(name) = function
            .params
            .iter()
            .find(|parameter| engine::is_reserved(parameter))
        {
            return Err(CompileError {
                kind: CompileErrorKind::ReservedName {
                    name: (*name).to_owned(),
                },
                at,
            });
        }
        if !function.name.starts_with(Hook::PREFIX) {
            continue;
        }
        let Some(hook) = Hook::from_name(function.name) else {
            return Err(CompileError {
                // Rhai puts no bound on a name's length; a reserved name
                // above is a game object's and short by itself.
                kind: CompileErrorKind::UnknownHook {
                    name: message(function.name, CompileError::MAX_MESSAGE_CHARS),
                },
                at,
            });
        };
        if function.params.len() != hook.parameters().len() {
            return Err(CompileError {
                kind: CompileErrorKind::HookParameters {
                    hook,
                    found: function.params.len(),
                },
                at,
            });
        }
        for (known, defined) in Hook::ALL.iter().zip(hooks.iter_mut()) {
            *defined |= *known == hook;
        }
    }
    Ok(hooks)
}

/// Rhai's own `error`, unless a raw string comes no later in the text:
/// then the raw string, which a script may not hold. A place Rhai gives
/// after one can be a line off, since Rhai 1.27.0's tokenizer loses a line
/// when a raw string holds a quote right before a line break (ADR-0040's
/// amendment, A6); a place before it is true.
fn before_raw_string(error: CompileError, raw_string: Option<SourcePosition>) -> CompileError {
    match (raw_string, error.at) {
        (Some(raw), Some(at)) if (at.line, at.column) < (raw.line, raw.column) => error,
        (Some(raw), _) => CompileError {
            kind: CompileErrorKind::RawString,
            at: Some(raw),
        },
        (None, _) => error,
    }
}

/// A place Rhai names, as ours.
pub(crate) fn position(at: Position) -> Option<SourcePosition> {
    let line = u32::try_from(at.line()?).ok()?;
    // Rhai gives no column at a line's very start.
    let column = u32::try_from(at.position().unwrap_or(1)).ok()?;
    Some(SourcePosition { line, column })
}

/// Rhai's own message as one line of bounded length: it can repeat a
/// script's text, escapes resolved.
pub(crate) fn message(text: &str, max_chars: usize) -> String {
    text::sanitize_untrusted(text, max_chars).text
}

/// Rhai's parse error as ours.
fn parse_error(error: &ParseError) -> CompileError {
    let kind = match error.err_type() {
        ParseErrorType::VariableUndefined(name) => CompileErrorKind::UndefinedVariable {
            name: message(name, CompileError::MAX_MESSAGE_CHARS),
        },
        ParseErrorType::ForbiddenVariable(name) => CompileErrorKind::ReservedName {
            name: message(name, CompileError::MAX_MESSAGE_CHARS),
        },
        ParseErrorType::Reserved(word) if word == "import" => CompileErrorKind::Import,
        ParseErrorType::BadInput(LexError::ImproperSymbol(symbol, _)) if symbol == "eval" => {
            CompileErrorKind::Eval
        }
        // With anonymous functions off, Rhai meets the bar that would
        // start one's parameters as a character it doesn't expect.
        ParseErrorType::BadInput(LexError::UnexpectedInput(found))
            if found == "|" || found == "||" =>
        {
            CompileErrorKind::AnonymousFunction
        }
        ParseErrorType::TooManyFunctions => CompileErrorKind::TooManyFunctions,
        ParseErrorType::ExprTooDeep => CompileErrorKind::ExpressionTooDeep,
        other => CompileErrorKind::Syntax {
            message: message(&other.to_string(), CompileError::MAX_MESSAGE_CHARS),
        },
    };
    CompileError {
        kind,
        at: position(error.position()),
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::script::ScriptDraft;

    use super::*;

    fn script(source: &str) -> Script {
        Script::try_from(ScriptDraft {
            api: 1,
            source: source.to_owned(),
            leash: 8,
            tool_guard: 2,
            block_guard: Vec::new(),
            messages: Vec::new(),
            patterns: Vec::new(),
        })
        .unwrap()
    }

    /// How many statements Rhai says stand outside every function.
    fn statements(source: &str) -> usize {
        let ast = Engine::new_raw().compile(source).unwrap();
        AsRef::<[_]>::as_ref(&ast).len()
    }

    // The user's reviewer's finding 5, pinned for the next bump: without
    // `internals`, the program's statements are still countable.
    #[test]
    fn rhai_shows_how_many_statements_stand_outside_every_function() {
        assert_eq!(statements(""), 0);
        assert_eq!(statements("// a comment\n/* another */"), 0);
        assert_eq!(
            statements("fn f() { 1 }\nfn g(a) { a }\n// and a comment"),
            0
        );
        assert_eq!(statements("let x = 1;\nfn f() { 1 }"), 1);
        assert_eq!(statements("fn f() { 1 }\nf();"), 1);
        assert_eq!(statements("fn f() { 1 }\n1 + 1;"), 1);
        assert_eq!(
            statements("const A = 1;\nlet b = 2;\nfn f() { 1 }\nf();"),
            3
        );
    }

    #[test]
    fn the_compiler_never_optimizes_so_no_statement_is_folded_away() {
        // An optimizer drops a statement without an effect; the count
        // above would then miss it.
        let source = "fn f() { 1 }\n42;";

        assert_eq!(
            compile(&script(source)).unwrap_err().kind,
            CompileErrorKind::TopLevelStatement
        );
    }

    #[test]
    fn a_place_at_a_lines_start_has_column_one() {
        assert_eq!(
            position(Position::new(3, 0)),
            Some(SourcePosition { line: 3, column: 1 })
        );
        assert_eq!(
            position(Position::new(3, 7)),
            Some(SourcePosition { line: 3, column: 7 })
        );
        assert_eq!(position(Position::NONE), None);
    }

    #[test]
    fn a_message_is_cut_and_put_on_one_line() {
        assert_eq!(message("one\ntwo \u{202e}three", 100), "one | two three");
        assert_eq!(message(&"x".repeat(500), 200).chars().count(), 200);
    }

    #[test]
    fn a_program_can_be_cloned_and_keeps_its_hooks() {
        let program = compile(&script("fn on_join(first) { }\nfn on_stop() { }")).unwrap();
        let copy = program.clone();

        assert!(copy.defines(Hook::Join) && copy.defines(Hook::Stop));
        assert!(!copy.defines(Hook::Timer));
        assert!(format!("{copy:?}").contains("Program"));
    }
}
