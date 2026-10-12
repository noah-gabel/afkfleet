//! The desktop crate's build script: tauri-build reads `tauri.conf.json`, embeds the
//! Windows icon and resolves the capabilities at build time.

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    tauri_build::try_build(tauri_build::Attributes::new())?;
    Ok(())
}
