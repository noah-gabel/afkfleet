//! Scripted modes: what a script is made of and bound by (Plan.md Phase 15;
//! ADR-0036, ADR-0037).
//!
//! A scripted mode's behaviour is a small program instead of a list of
//! steps. This module holds everything about it that doesn't depend on the
//! language it's written in: no Rhai type is named here. `fleet-script`
//! runs the program, `fleet-runtime` gives it events and performs what it
//! asks for, and `fleet-mc` only translates.
//!
//! - [`Script`] is a scripted mode's body: its [`ScriptSource`], the API
//!   level it was validated at, its leash and tool guard, and what it
//!   declares. [`limits`] holds a script's limits as constants.
//! - [`Ident`], [`Word`] and [`WordList`] are the names and the closed
//!   lists of text a mode's declaration holds.
//! - [`Input`] names the keys and mouse buttons of a vanilla client,
//!   [`HoldTracker`] knows which a script holds down, and
//!   [`ActionLimiter`] holds every kind of action to today's shortest
//!   interval, below the script (ADR-0036, D4, D7, D9).
//! - [`Snapshot`] is what a hook sees of the game: the bot, the entities
//!   and the blocks near it, everything a server controls as a registry
//!   [`Identifier`] or a number (ADR-0036, D12).
//! - [`Leash`] decides how far a bot walks from where it was put, and
//!   [`may_mine`] and [`ToolGuard::allows`] what it may mine (ADR-0036,
//!   D10, D11).
//! - [`Hook`] and [`HookEvent`] are what a script is called for, and
//!   [`Refusal`] why an action wasn't taken (ADR-0036, D4, D6).
//! - [`MessageTemplate`] is a message the mode declares, fixed or with
//!   typed values: the only text its script can send (ADR-0036, D8).
//! - [`ChatPattern`] is a line of the server's own chat the mode declares
//!   it reads, as typed values and never as text; [`candidate_line`] says
//!   which received lines may be read at all, and [`ChatWatch`] which
//!   patterns count right now (ADR-0037). [`amount`] reads the numbers.

pub mod amount;
mod event;
mod facts;
mod guard;
mod ident;
mod input;
mod leash;
mod limiter;
pub mod limits;
mod pattern;
mod placeholder;
mod settings;
mod source;
mod template;
mod watch;
mod word;

pub use amount::DecimalMark;
pub use event::{Hook, HookEvent, Refusal, TimerName, TimerNameError};
pub use facts::{
    BlockPosition, Blocks, BotFacts, EntityFact, HeldItem, Identifier, IdentifierError, LookingAt,
    Position, PositionError, Snapshot,
};
pub use guard::may_mine;
pub use ident::{Ident, IdentError};
pub use input::{HoldError, HoldTracker, Input, Key, MouseButton, Pressed};
pub use leash::{Leash, LeashTick};
pub use limiter::{ActionLimiter, Limited, Press, TooSoon};
pub use pattern::{
    Capture, CaptureDraft, CaptureKind, Captured, ChatMatch, ChatPattern, ChatPatterns,
    ChatPatternsError, PatternDraft, PatternError, When, candidate_line,
};
#[cfg(test)]
pub(crate) use settings::draft_of;
pub use settings::{
    LeashRadius, LeashRadiusError, Script, ScriptApiLevel, ScriptApiLevelError, ScriptDraft,
    ScriptError, ToolGuard, ToolGuardError,
};
pub use source::{ScriptSource, ScriptSourceError};
pub use template::{
    Given, MessageDraft, MessageTemplate, MessageValue, RenderError, TemplateError, ValueDraft,
    ValueKind,
};
pub use watch::{ChatWatch, Observed, SendId, WindowError};
pub use word::{Word, WordError, WordList, WordListError};
