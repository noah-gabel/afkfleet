//! The afkfleet desktop app's Rust side (Plan.md Phase 8, ADR-0007).
//!
//! It builds the Tauri app from `tauri.conf.json` and runs it. The React UI lives in
//! `packages/ui`; `apps/desktop` is its thin web entry. Its hardening (P8.3, ADR-0017):
//! - one instance only: a second launch brings the running window forward (`focus`)
//! - the webview never leaves the app (`navigation`)
//! - the CSP, the capabilities and the absent global API live in `tauri.conf.json` and
//!   `capabilities/`; `scripts/tauri-hardening.mjs` pins them

mod focus;
mod navigation;

use tauri::Manager as _;

/// The main window's label, as `tauri.conf.json` names it.
const MAIN_WINDOW: &str = "main";

/// Builds the app from its configuration and runs it until its window closes.
///
/// # Errors
///
/// Returns Tauri's error if the app can't be built or its event loop fails.
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default()
        // First, as the plugin requires: a second launch hands over to this instance and
        // exits before anything else of it starts.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                focus::focus_main_window(&window);
            }
        }))
        .plugin(navigation::guard()?)
        .run(tauri::generate_context!())
}
