//! The afkfleet desktop app's Rust side (Plan.md Phase 8, ADR-0007).
//!
//! It builds the Tauri app from `tauri.conf.json` and runs it. The React UI lives in
//! `packages/ui`; `apps/desktop` is its thin web entry. Its hardening (P8.3, ADR-0017):
//! - one instance only: a second launch brings the running window forward (`focus`)
//! - the webview never leaves the app (`navigation`)
//! - the CSP, the capabilities and the absent global API live in `tauri.conf.json` and
//!   `capabilities/`; `scripts/tauri-hardening.mjs` pins them
//!
//! The bridge to the server (P8.4, ADR-0017): the UI calls the app's commands, which talk
//! to the server only through fleet-client. [`state::AppState`] holds the server profile
//! ([`profile`]) and the client; the refresh token lives in the OS keychain
//! ([`keychain`]); no token ever reaches the UI or the state.
//!
//! Microsoft's sign-in page (P9.8, ADR-0018) opens through `ShellOpener`:
//! tauri-plugin-opener's free `open_url`, on the one link the state holds
//! after fleet-client's `VerifiedLink` passed it. The plugin isn't
//! registered, so the webview gets none of its commands.

mod commands;
mod endpoints;
mod error;
mod focus;
pub mod keychain;
mod navigation;
pub mod profile;
pub mod startup;
pub mod state;

use std::sync::Arc;

use fleet_client::VerifiedLink;
use tauri::{Manager as _, Runtime};

use crate::keychain::KeychainFactory;
use crate::profile::PROFILE_FILE;
use crate::startup::StartError;
use crate::state::{AppState, LinkOpener, OpenError};

/// The main window's label, as `tauri.conf.json` names it.
const MAIN_WINDOW: &str = "main";

/// Registers the app's commands on `builder`: exactly the ones build.rs lists, each
/// still refused until the `main` capability grants it. The caller manages an
/// [`AppState`] on the built app before any command runs.
#[must_use]
pub fn configure<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.invoke_handler(tauri::generate_handler![
        commands::server::server_profile,
        commands::server::set_server_profile,
        commands::server::check_server,
        commands::auth::login,
        commands::auth::login_mfa,
        commands::auth::logout,
        commands::auth::register,
        commands::auth::step_up,
        commands::mfa::mfa_enroll,
        commands::mfa::mfa_confirm,
        commands::mfa::mfa_reset,
        commands::me::me,
        commands::me::change_password,
        commands::me::sessions,
        commands::me::revoke_session,
        commands::users::users,
        commands::users::patch_user,
        commands::invites::invites,
        commands::invites::create_invite,
        commands::invites::revoke_invite,
        commands::links::start_link,
        commands::links::link_status,
        commands::accounts::create_offline_account,
        commands::accounts::accounts,
        commands::accounts::accounts_awaiting_approval,
        commands::accounts::account,
        commands::accounts::delete_account,
        commands::accounts::approve_account,
        commands::accounts::refuse_account,
        commands::links::relink_account,
        commands::grants::account_grants,
        commands::grants::create_grant,
        commands::grants::change_grant,
        commands::grants::revoke_grant,
        commands::links::open_verification_link,
    ])
}

/// Builds the app from its configuration and runs it until its window closes. The
/// caller installs the TLS provider first ([`startup::install_crypto_provider`]).
///
/// # Errors
///
/// [`StartError`] if the app can't be built, its data folder can't be found, the
/// Windows Credential Manager can't be opened, or its event loop fails.
pub fn run() -> Result<(), StartError> {
    let app = configure(
        tauri::Builder::default()
            // First, as the plugin requires: a second launch hands over to this instance
            // and exits before anything else of it starts.
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                    focus::focus_main_window(&window);
                }
            }))
            .plugin(navigation::guard()?),
    )
    .build(tauri::generate_context!())?;
    let store = windows_native_keyring_store::Store::new().map_err(|_| StartError::Keychain)?;
    let factory = KeychainFactory::windows(store, &app.config().identifier);
    let profile_path = app.path().app_local_data_dir()?.join(PROFILE_FILE);
    // Managed before the event loop starts, so no command can run without it.
    app.manage(AppState::load(profile_path, factory, Arc::new(ShellOpener)));
    app.run(|_, _| {});
    Ok(())
}

/// Opens a checked link in the default browser with tauri-plugin-opener's
/// free `open_url`, which needs no registered plugin. Only real-OS wiring,
/// like the rest of this file: no test opens a browser.
struct ShellOpener;

impl LinkOpener for ShellOpener {
    fn open(&self, link: &VerifiedLink) -> Result<(), OpenError> {
        tauri_plugin_opener::open_url(link.as_str(), None::<&str>).map_err(|_| OpenError)
    }
}
