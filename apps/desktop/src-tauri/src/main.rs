//! `afkfleet-desktop`: the afkfleet desktop app (Plan.md P8.2).
//!
//! It only starts the app; [`afkfleet_desktop::run`] holds the logic. Release builds
//! use the Windows GUI subsystem, so no console window opens next to the app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), tauri::Error> {
    afkfleet_desktop::run()
}
