//! The pure domain core of afkfleet.
//!
//! This crate holds the business rules: domain types and value objects, the bot
//! state machine, retry and circuit-breaker policies, mode scheduling,
//! `authorize()` and the Minecraft port traits. It does no IO and depends on
//! neither tokio nor azalea, so every rule here can be tested deterministically.
//! Time and randomness are always passed in (ADR-0010).
//!
//! # Modules
//! - [`id`]: typed IDs for users, accounts, bots, agents and modes.
//! - [`value`]: server addresses and the names of Minecraft accounts and users.
//! - [`chat`]: the messages bots send, and sanitized messages they receive.

pub mod chat;
pub mod id;
mod text;
pub mod value;
