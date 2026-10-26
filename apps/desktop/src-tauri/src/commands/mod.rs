//! The app's Tauri commands, grouped per feature (Plan.md P8.4, ADR-0017).
//!
//! Each one is a thin wrapper: it takes the managed [`AppState`] and its
//! arguments, which Tauri deserializes from the UI's call, and calls the
//! state's method of the same name ([`crate::endpoints`], [`crate::state`]).
//! Each answers `Result<T, CommandError>`; none takes or returns a token.
//!
//! A command runs only when it's listed in build.rs's `COMMANDS` and the
//! `main` capability grants its `allow-<command>`; `scripts/tauri-hardening.mjs`
//! pins both, and `tests/ipc-contract.json` lists the exact arguments the UI
//! sends.

use crate::keychain::KeychainFactory;
use crate::state::AppState;

pub(crate) mod accounts;
pub(crate) mod auth;
pub(crate) mod grants;
pub(crate) mod invites;
pub(crate) mod links;
pub(crate) mod live;
pub(crate) mod me;
pub(crate) mod mfa;
pub(crate) mod server;
pub(crate) mod users;

/// The managed state, as a command takes it.
pub(crate) type Managed<'a> = tauri::State<'a, AppState<KeychainFactory>>;
