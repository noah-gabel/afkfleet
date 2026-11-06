//! The app's updates (Plan.md P12.5; ADR-0022, U27, U30, S27): the two commands the UI calls.
//! Neither takes an argument, so nothing the webview sends can name a release, an address or
//! bytes; tauri-plugin-updater's own commands are never granted. Tauri hands each the app's
//! handle, which ties the managed updater to the app's runtime.

use fleet_api_types::app::{CommandError, UpdateCheck};
use tauri::{AppHandle, Runtime, State};

use super::Managed;
use crate::update::{PluginUpdates, Updater};

/// `check_update`: this app's version, and whether a newer signed release waits; see
/// [`Updater::check`].
#[tauri::command]
pub(crate) async fn check_update<R: Runtime>(
    _app: AppHandle<R>,
    updater: State<'_, Updater<PluginUpdates<R>>>,
) -> Result<UpdateCheck, CommandError> {
    updater.check().await
}

/// `install_update`: installs the release the last check found; see [`Updater::install`].
#[tauri::command]
pub(crate) async fn install_update<R: Runtime>(
    _app: AppHandle<R>,
    updater: State<'_, Updater<PluginUpdates<R>>>,
    state: Managed<'_>,
) -> Result<(), CommandError> {
    updater.install(&state).await
}
