//! fleet-script's tests of what it promises: validation, every limit, the
//! state, the game objects, chat, timers, the examples, threads and the
//! properties of ADR-0036's D18.
//!
//! One binary, so Rhai is linked once. The memory test is a binary of its
//! own (`tests/memory.rs`): it replaces the allocator.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod chat;
mod clock;
mod convert;
mod examples;
mod levels;
mod limits;
mod menu_examples;
mod menus;
mod objects;
mod properties;
mod refused;
mod sandbox;
mod state;
mod support;
mod threads;
mod time;
mod timers;
mod validation;
