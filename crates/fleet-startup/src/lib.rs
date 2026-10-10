//! Start-up code shared by afkfleet's binaries, `afkfleet-agent` and
//! `afkfleet-server` (ADR-0015).
//!
//! It holds only what a process needs to start, and nothing else: no
//! accounts, no authentication, no domain logic. Those belong to the crates
//! that own them.
//!
//! # Modules
//! - [`config`]: loading a TOML file plus prefixed environment variables, with
//!   errors that name the key path of every problem and never echo a value,
//!   and the `[log]` section's types.
//! - [`telemetry`]: the log layer (JSON or pretty lines on stdout), its filter
//!   and a panic hook that logs through it.
//!
//! The filter always caps `azalea_auth` at `info` and always lets panic
//! reports through. A binary can only add rules on top, through
//! [`telemetry::FilterRules`]: the agent adds azalea's.

pub mod config;
pub mod telemetry;
