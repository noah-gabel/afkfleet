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

mod ident;
mod placeholder;
mod template;
mod word;

pub use ident::{Ident, IdentError};
pub use template::{
    Given, MessageDraft, MessageTemplate, MessageValue, RenderError, TemplateError, ValueDraft,
    ValueKind,
};
pub use word::{Word, WordError, WordList, WordListError};
