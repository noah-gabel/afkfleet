//! The afkfleet desktop app's Rust side (Plan.md Phase 8, ADR-0007).
//!
//! It builds the Tauri app from `tauri.conf.json` and runs it. The React UI lives in
//! `packages/ui`; `apps/desktop` is its thin web entry.

/// Builds the app from its configuration and runs it until its window closes.
///
/// # Errors
///
/// Returns Tauri's error if the app can't be built or its event loop fails.
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default().run(tauri::generate_context!())
}
