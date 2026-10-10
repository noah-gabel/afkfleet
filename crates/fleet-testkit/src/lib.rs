//! Test doubles for afkfleet (Plan.md §4, §8): scriptable fakes for the ports
//! that `fleet-core` defines, so every layer above them can be tested without
//! IO, and a few helpers every crate's tests share. It's a dev-dependency
//! only, and like any library code it never panics: misuse shows up as an
//! error or an outcome value (ADR-0011). The one exception is [`jail::in_jail`],
//! which passes the panics of the test it runs through.
//!
//! - [`mc`]: a fake [`MinecraftConnector`](fleet_core::mc::MinecraftConnector)
//!   whose sessions a test drives through a [`SessionController`](mc::SessionController),
//!   and a fake [`SessionCredentialProvider`](fleet_core::mc::SessionCredentialProvider).
//! - [`log_capture`]: a process-wide capture of every log line, for tests
//!   that check a secret never reaches a log.
//! - [`log_buffer`]: a writer for a log layer a test builds itself, for tests
//!   of what that layer prints.
//! - [`jail`]: figment's `Jail`, for the tests that load configs.
//! - [`system`]: a [`Clock`](fleet_core::system::Clock) that only moves when
//!   the test moves it, and a seeded
//!   [`SecureRandom`](fleet_core::system::SecureRandom), which is why this
//!   crate must never be more than a dev-dependency (`just testkit-check`).
//!
//! Fakes are preferred over mocks; `mockall` is only for checking interactions
//! (CLAUDE.md, testing conventions).

pub mod jail;
pub mod log_buffer;
pub mod log_capture;
pub mod mc;
pub mod system;
