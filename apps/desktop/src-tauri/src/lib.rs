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
//! The session's live events reach the UI over a Tauri `Channel`, through one
//! long-running command (P11.8, ADR-0020): fleet-client's socket in Rust, never its
//! ticket or token in an update.
//!
//! Microsoft's sign-in page (P9.8, ADR-0018) opens through `ShellOpener`:
//! tauri-plugin-opener's free `open_url`, on the one link the state holds
//! after fleet-client's `VerifiedLink` passed it. The plugin isn't
//! registered, so the webview gets none of its commands.
//!
//! Updates (P12.5, ADR-0022) come from tauri-plugin-updater, signed with the
//! maintainer's offline key: two commands of the app's own check and install
//! ([`update`]). The plugin is registered, but no capability grants its
//! commands.
//!
//! The window starts hidden and shows once it has its colour, and its colours follow the
//! user's theme (`window_theme`; P11.10, ADR-0020).

mod commands;
mod endpoints;
mod error;
mod focus;
pub mod keychain;
mod live;
mod navigation;
pub mod profile;
pub mod startup;
pub mod state;
pub mod update;
mod window_theme;

use std::sync::Arc;

use fleet_client::VerifiedLink;
use tauri::{Manager as _, Runtime};

use crate::keychain::KeychainFactory;
use crate::profile::PROFILE_FILE;
use crate::startup::StartError;
use crate::state::{AppState, LinkOpener, OpenError};
use crate::update::{PluginUpdates, Updater};

/// The main window's label, as `tauri.conf.json` names it.
const MAIN_WINDOW: &str = "main";

/// Registers the app's commands on `builder`: exactly the ones build.rs lists, each
/// still refused until the `main` capability grants it, and the updater plugin, whose
/// own commands no capability grants. Also reveals the hidden main window at setup, and
/// repaints it when its theme changes. The caller manages an [`AppState`] and the
/// updates ([`manage_updates`]) on the built app before any command runs.
#[must_use]
pub fn configure<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        // Rust's alone: no capability grants any of its commands (ADR-0022, S27).
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                app.manage(window_theme::reveal_main_window(&window));
            }
            Ok(())
        })
        // Windows' own change, under the `windows` choice, where the page follows it itself; and
        // every `set_window_theme` that changes the window's theme, which paints it as well.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::ThemeChanged(theme) = event
                && let Some(webview) = window.get_webview_window(window.label())
            {
                window_theme::paint(&webview, *theme);
            }
        })
        .invoke_handler(tauri::generate_handler![
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
            commands::grants::grant_candidates,
            commands::grants::create_grant,
            commands::grants::change_grant,
            commands::grants::revoke_grant,
            commands::links::open_verification_link,
            commands::live::watch_events,
            commands::bots::bots,
            commands::bots::bot,
            commands::bots::modes,
            commands::modes::create_mode,
            commands::modes::create_mode_with_commands,
            commands::modes::update_mode,
            commands::modes::update_mode_with_commands,
            commands::modes::delete_mode,
            commands::bots::patch_bot,
            commands::bots::set_bot_mode,
            commands::bots::start_bot,
            commands::bots::stop_bot,
            commands::bots::restart_bot,
            commands::bots::reset_bot,
            commands::bots::resume_bot,
            commands::bots::bot_chat,
            commands::bots::send_chat,
            commands::bots::send_command,
            commands::server_settings::server_settings,
            commands::server_settings::create_server_settings,
            commands::server_settings::update_server_settings,
            commands::server_settings::delete_server_settings,
            commands::server_settings::preview_chat_formats,
            commands::audit::audit,
            commands::agents::agents,
            commands::agents::disable_agent,
            commands::agents::release_agent,
            commands::users::user_grants,
            commands::users::shared_accounts,
            commands::grants::leave_account,
            commands::window::set_window_theme,
            commands::update::check_update,
            commands::update::install_update,
            commands::update::app_version,
        ])
}

/// Manages the [`Updater`] the update commands use on `app`, as [`configure`] built it:
/// on only for a release build whose updater points at the release's manifest, or a
/// debug build whose updater points only at loopback, the rehearsal's
/// ([`update::updates_on`]), at the app's own version. A release build downloads
/// over https alone (ADR-0027).
pub fn manage_updates<R: Runtime>(app: &tauri::App<R>) {
    let release_build = !cfg!(debug_assertions);
    let on = update::updates_on(app.config().plugins.0.get("updater"), release_build);
    app.manage(Updater::new(
        PluginUpdates::new(app.handle().clone()).requiring_https(release_build),
        on,
        &app.package_info().version.to_string(),
    ));
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
    manage_updates(&app);
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
/// like `run`: no test opens a browser.
struct ShellOpener;

impl LinkOpener for ShellOpener {
    fn open(&self, link: &VerifiedLink) -> Result<(), OpenError> {
        tauri_plugin_opener::open_url(link.as_str(), None::<&str>).map_err(|_| OpenError)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use tauri::test::mock_builder;
    use tauri::{Manager as _, RunEvent};

    use super::{MAIN_WINDOW, configure};
    use crate::window_theme::Revealed;

    #[test]
    fn configure_reveals_the_main_window_when_the_app_starts() {
        let app = configure(mock_builder())
            .build(tauri::generate_context!(test = true))
            .unwrap();
        let revealed = Rc::new(Cell::new(false));
        let seen = Rc::clone(&revealed);

        // Destroying the last window ends the mock's loop, so this returns.
        app.run(move |handle, event| {
            if matches!(event, RunEvent::Ready) {
                seen.set(handle.try_state::<Revealed>().is_some());
                handle
                    .get_webview_window(MAIN_WINDOW)
                    .unwrap()
                    .destroy()
                    .unwrap();
            }
        });

        assert!(revealed.get());
    }
}
