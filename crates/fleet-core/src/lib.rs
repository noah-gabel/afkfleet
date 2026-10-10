//! The pure domain core of afkfleet.
//!
//! This crate holds the business rules: domain types and value objects, the bot
//! state machine, retry and circuit-breaker policies, mode scheduling,
//! `authorize()` and the Minecraft port traits. It does no IO and depends on
//! neither tokio nor azalea, so every rule here can be tested deterministically.
//!
//! # Modules
//! - [`id`]: typed IDs for users, accounts, bots, agents and modes.
//! - [`value`]: server addresses and the names of Minecraft accounts, users
//!   and agents.
//! - [`chat`]: the messages bots send, and sanitized messages they receive.
//! - [`disconnect`]: why a session ended, and whether the bot retries.
//! - [`resilience`]: retry backoff, failure windows and the circuit breaker.
//! - [`bot`]: the bot state machine, and a bot's spec and status.
//! - [`mode`]: what a bot does while it's online, and the presets.
//! - [`mc`]: the Minecraft ports the runtime drives a session through.
//! - [`authz`]: who may do what, decided by [`authz::authorize`].
//! - [`audit`]: the vocabulary of the server's audit log.
//! - [`time`]: saturating arithmetic on points in time and durations.
//! - [`system`]: the clock and secure-randomness ports the server reads time
//!   and random bytes through, and minting IDs from them.
//! - [`text`]: sanitizing untrusted text for a log line.
//!
//! # How it fits together
//! A bot's actor (in fleet-runtime) feeds every command and session event to
//! [`bot::transition`], which returns the next state and the [`bot::Effect`]s
//! to execute. The actor runs them through the [`mc`] ports:
//! [`mc::MinecraftConnector`] starts a session, and [`mc::SessionHandle`]
//! acts in it. [`disconnect::DisconnectReason::classify`] decides what an
//! ended session means, with a bot's [`disconnect::ConflictTexts`] on top,
//! and the [`resilience`] policies decide how long to
//! wait before the next attempt. While the bot is online, a
//! [`mode::ModePlan`] decides what it does and when, as
//! [`mode::GameAction`]s for the session and chat for the chat queue. On the
//! server, [`authz::authorize`] decides every request.
//!
//! # Conventions
//! These hold for the whole crate (ADR-0010):
//! - **Time is passed in.** Functions take `now: DateTime<Utc>`, and core never
//!   reads a clock. Only the session's liveness stamps are monotonic
//!   `Instant`s, written by the adapter.
//! - **Randomness is passed in** as `&mut impl Rng`, and core types never
//!   store a random number generator. IDs take 10 random bytes from the
//!   caller instead.
//! - **The server's ports.** The server reads the time and secure random
//!   bytes through [`system::Clock`] and [`system::SecureRandom`], whose
//!   implementations live outside core (ADR-0015). Core only names them.
//! - **Errors.** Each module that can fail has its own error enum. Its
//!   variants carry context, such as an index, a length or a limit, and never
//!   the untrusted input, so an error message can't carry it into a log.
//!   [`bot::transition`], [`disconnect::DisconnectReason::classify`],
//!   [`disconnect::ConflictTexts::classify`] and the chat sanitizer are total
//!   and return no errors.
//! - **Stored formats.** Value objects and modes deserialize through their
//!   validating constructors. Changes to stored JSON are additive only, and a
//!   new action or schedule type fails to load on an older server instead of
//!   running in part. Runtime types such as [`bot::BotState`], [`bot::BotSpec`],
//!   [`disconnect::DisconnectReason`] and [`mc::SessionCredentials`] have no
//!   serde: their wire formats come with the proto and DTO conversions.
//! - **Secrets** are `SecretString`s, whose `Debug` output is redacted.

pub mod audit;
pub mod authz;
pub mod bot;
pub mod chat;
pub mod disconnect;
pub mod id;
pub mod mc;
pub mod mode;
pub mod resilience;
pub mod system;
pub mod text;
pub mod time;
pub mod value;
