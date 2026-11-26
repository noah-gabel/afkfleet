//! Scripted modes: the sandboxed Rhai engine, validation, the instance
//! that runs a script's hooks, and the API's objects (Plan.md Phase 15,
//! group B; ADR-0036, ADR-0037, ADR-0040).
//!
//! A scripted mode's behaviour is a small program instead of a list of
//! steps: a virtual player at a vanilla client that presses keys and
//! mouse buttons, looks, rides and reacts. `fleet-core` holds everything
//! about it that doesn't depend on the language; this crate is the
//! language. **Only this crate depends on Rhai,** which `deny.toml`
//! enforces.
//!
//! - [`compile`] checks a [`Script`]'s source and turns it into a
//!   [`Program`], or says what's wrong with its line and column. The
//!   server calls it when a mode is saved, the agent again before a run.
//! - An [`Instance`] is one bot's running script: its state, its timers,
//!   what it holds down, its budgets. Its caller hands it events
//!   ([`Instance::handle`]) and wakes it when it asked
//!   ([`Instance::wake`]); each call runs hooks to their end and returns
//!   the [`Action`]s they asked for, and what they were [`Refused`].
//! - [`convert`] writes a step mode as the script that does what its
//!   steps do, with what it does differently ([`Conversion`]): "Convert
//!   to script" (Plan.md P15.27).
//! - [`API`] describes every object a script uses, for the reference and
//!   the editor ([`with_tables`] writes the reference's tables from it),
//!   and [`EXAMPLES`] are the complete scripts the reference shows.
//!
//! **Pure and synchronous:** no IO, no tokio, no clock. A hook is given
//! its time and its facts ([`Moment`]), a script's random numbers come
//! from a seed, and how long a hook has run is read from a stopwatch its
//! caller hands in. A hook is a bounded computation: a few microseconds
//! as a rule, and never more than 5 ms. Compiling takes about half a
//! millisecond and belongs off the async runtime.
//!
//! **What the sandbox is** (ADR-0036, D3; ADR-0040): Rhai without modules,
//! without a clock, without `eval`, without anonymous functions, with
//! packages picked by hand so that nothing a script can call blocks or
//! reaches outside; an operation budget and a time budget, each a hook
//! and a second; limits on calls, nesting, texts, arrays, maps and
//! variables; and a state that is checked while a hook runs. A script
//! that fails or hits a limit stops, and everything it held is released.
//!
//! [`Script`]: fleet_core::script::Script

mod action;
mod api;
mod budget;
mod clock;
mod compile;
mod convert;
mod engine;
mod error;
mod examples;
mod instance;
mod scan;
mod state;
mod timers;
mod turn;

pub use action::{Action, Asked, PressKind, Refused};
pub use api::describe::{API, Member, MemberKind, Object, reason_meaning, reasons};
pub use api::reference::{ReferenceError, table, with_tables};
pub use compile::{Program, compile};
pub use convert::{Conversion, ConvertError, Note, NoteKind, convert};
pub use error::{
    CompileError, CompileErrorKind, Limit, Misuse, ScriptError, ScriptErrorKind, SourcePosition,
};
pub use examples::{EXAMPLES, Example};
pub use instance::{Instance, Moment, Outcome, Presence, Ran};
