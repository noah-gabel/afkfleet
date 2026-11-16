//! Scripted modes: what a script is made of and bound by (Plan.md Phase 15;
//! ADR-0036, ADR-0037).
//!
//! A scripted mode's behaviour is a small program instead of a list of
//! steps. This module holds everything about it that doesn't depend on the
//! language it's written in: no Rhai type is named here. `fleet-script`
//! runs the program, `fleet-runtime` gives it events and performs what it
//! asks for, and `fleet-mc` only translates.
//!
//! - [`Ident`], [`Word`] and [`WordList`] are the names and the closed
//!   lists of text a mode's declaration holds.
//! - [`MessageTemplate`] is a message the mode declares, fixed or with
//!   typed values: the only text its script can send (ADR-0036, D8).
//! - [`ChatPattern`] is a line of the server's own chat the mode declares
//!   it reads, as typed values and never as text; [`candidate_line`] says
//!   which received lines may be read at all, and [`ChatWatch`] which
//!   patterns count right now (ADR-0037). [`amount`] reads the numbers.

pub mod amount;
mod ident;
mod pattern;
mod placeholder;
mod template;
mod watch;
mod word;

pub use amount::DecimalMark;
pub use ident::{Ident, IdentError};
pub use pattern::{
    Capture, CaptureDraft, CaptureKind, Captured, ChatMatch, ChatPattern, ChatPatterns,
    ChatPatternsError, PatternDraft, PatternError, When, candidate_line,
};
pub use template::{
    Given, MessageDraft, MessageTemplate, MessageValue, RenderError, TemplateError, ValueDraft,
    ValueKind,
};
pub use watch::{ChatWatch, Observed, SendId, WindowError};
pub use word::{Word, WordError, WordList, WordListError};
