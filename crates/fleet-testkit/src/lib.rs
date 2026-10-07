//! Test doubles for afkfleet (Plan.md §4, §8): scriptable fakes for the ports
//! that `fleet-core` defines, so every layer above them can be tested without
//! IO. It's a dev-dependency only, and like any library code it never panics:
//! misuse shows up as an error or an outcome value (ADR-0011).
//!
//! - [`mc`]: a fake [`MinecraftConnector`](fleet_core::mc::MinecraftConnector)
//!   whose sessions a test drives through a [`SessionController`](mc::SessionController).
//! - [`log_capture`]: a process-wide capture of every log line, for tests
//!   that check a secret never reaches a log.
//!
//! Fakes are preferred over mocks; `mockall` is only for checking interactions
//! (CLAUDE.md, testing conventions).

pub mod log_capture;
pub mod mc;
