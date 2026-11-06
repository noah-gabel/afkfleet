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
//! - [`justfile`]: a strict reader of the justfile's recipes, for the tests
//!   that load a config with exactly a recipe's exports (ADR-0020, R124).
//! - [`dockerfile`]: a reader of the Dockerfiles' instructions and stages,
//!   for the tests that pin both images (ADR-0022, S13).
//! - [`compose`]: a line reader of the compose files, for the tests that pin
//!   them (ADR-0022, S14–S17).
//! - [`docker`]: `docker` and `docker compose` with deadlines, a project's
//!   cleanup and a log follower, for the slow tests that run containers
//!   (ADR-0014; ADR-0022, U24).
//! - [`system`]: a [`Clock`](fleet_core::system::Clock) that only moves when
//!   the test moves it, and a seeded
//!   [`SecureRandom`](fleet_core::system::SecureRandom), which is why this
//!   crate must never be more than a dev-dependency (`just testkit-check`).
//!
//! Fakes are preferred over mocks; `mockall` is only for checking interactions
//! (CLAUDE.md, testing conventions).

pub mod compose;
pub mod docker;
pub mod dockerfile;
pub mod jail;
pub mod justfile;
pub mod log_buffer;
pub mod log_capture;
pub mod mc;
pub mod system;
