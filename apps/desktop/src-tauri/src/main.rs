//! `afkfleet-desktop`: the afkfleet desktop app (Plan.md P8.2).
//!
//! It installs the TLS provider and starts the app; [`afkfleet_desktop::run`] holds the
//! logic. Release builds use the Windows GUI subsystem, so no console window opens next
//! to the app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use afkfleet_desktop::startup::{self, StartError};

fn main() -> Result<(), StartError> {
    // Before any client is built: aws-lc-rs is the process's only provider (ADR-0017).
    startup::install_crypto_provider()?;
    afkfleet_desktop::run()
}
