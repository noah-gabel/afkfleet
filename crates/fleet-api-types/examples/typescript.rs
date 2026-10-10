//! The TypeScript export behind `just gen` and `just gen-check` (Plan.md
//! P6.9, ADR-0015).
//!
//! - `typescript write <dir>` makes `<dir>` hold exactly a fresh export
//!   (`just gen`). It deletes only files ts-rs generated, and stops without
//!   changing anything when `<dir>` holds anything else.
//! - `typescript check <dir>` exits with 1 and names every difference when
//!   `<dir>` isn't exactly a fresh export (`just gen-check`, CI's
//!   `ts-types-fresh`).
//!
//! Every run exports into a new, empty folder inside the workspace's
//! `target/gen/`, which is removed afterwards. Exit codes: 0 done or up to
//! date, 1 out of date or failed, 2 usage.

use std::error::Error;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;

use fleet_api_types::typescript;
use tempfile::TempDir;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [mode, dir] if mode == "write" => write(Path::new(dir)),
        [mode, dir] if mode == "check" => check(Path::new(dir)),
        _ => {
            report("usage: typescript (write | check) <dir>");
            return ExitCode::from(2);
        }
    };
    result.unwrap_or_else(|error| {
        report(&chain(error.as_ref()));
        ExitCode::FAILURE
    })
}

/// `just gen`.
fn write(dir: &Path) -> Result<ExitCode, Box<dyn Error>> {
    let scratch = scratch()?;
    typescript::write(scratch.path(), dir)?;
    report(&format!(
        "exported the API's TypeScript types to {}",
        dir.display()
    ));
    Ok(ExitCode::SUCCESS)
}

/// `just gen-check`.
fn check(dir: &Path) -> Result<ExitCode, Box<dyn Error>> {
    let scratch = scratch()?;
    let differences = typescript::check(scratch.path(), dir)?;
    if differences.is_empty() {
        report(&format!("{} matches a fresh export", dir.display()));
        return Ok(ExitCode::SUCCESS);
    }
    for difference in &differences {
        report(&format!("  {difference}"));
    }
    report(&format!(
        "{} is out of date: run `just gen` and commit the result",
        dir.display()
    ));
    Ok(ExitCode::FAILURE)
}

/// A new, empty folder inside the workspace's `target/gen/`, removed when
/// it's dropped.
fn scratch() -> io::Result<TempDir> {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/gen");
    std::fs::create_dir_all(&parent)?;
    tempfile::Builder::new()
        .prefix("export-")
        .tempdir_in(parent)
}

/// `error` and its sources, joined with `: `.
fn chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// One line on stderr. A failed write has nowhere to be reported.
fn report(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}
