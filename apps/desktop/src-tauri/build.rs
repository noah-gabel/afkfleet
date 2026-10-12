//! The desktop crate's build script: tauri-build reads `tauri.conf.json`, embeds the
//! Windows icon and resolves the capabilities and the app's command manifest at build time.

use std::error::Error;

use tauri_build::{AppManifest, Attributes};

/// The app's own commands. tauri-build generates an `allow-<command>` and a
/// `deny-<command>` permission for each, and the UI can call one only when a capability
/// grants its `allow-<command>`; `permissions/default.toml` keeps that true while the list
/// is empty (ADR-0017).
const COMMANDS: &[&str] = &[];

fn main() -> Result<(), Box<dyn Error>> {
    tauri_build::try_build(Attributes::new().app_manifest(AppManifest::new().commands(COMMANDS)))?;
    Ok(())
}
