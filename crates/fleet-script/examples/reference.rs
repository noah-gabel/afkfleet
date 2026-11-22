//! The script reference's tables behind `just gen` and `just gen-check`
//! (Plan.md P15.28; ADR-0046): every table `docs/scripting.md` marks as
//! generated, written from fleet-script's description of the API.
//!
//! - `reference write <file>` writes the file's marked tables anew and
//!   leaves every other line as it is (`just gen`).
//! - `reference check <file>` exits with 1 when a marked table differs from
//!   what `write` would write (`just gen-check`).
//!
//! Exit codes: 0 done or up to date, 1 out of date or failed, 2 usage.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

/// Writes a line to stderr; one that can't be written is lost.
macro_rules! report {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, file] if mode == "write" => write(Path::new(file)),
        [mode, file] if mode == "check" => check(Path::new(file)),
        _ => {
            report!("usage: reference (write | check) <file>");
            ExitCode::from(2)
        }
    }
}

/// The file's text and the same with its tables written anew.
fn read(file: &Path) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(file)
        .map_err(|error| format!("{} can't be read: {error}", file.display()))?;
    let written = fleet_script::with_tables(&text).map_err(|error| error.to_string())?;
    Ok((text, written))
}

/// `just gen`.
fn write(file: &Path) -> ExitCode {
    let outcome = read(file).and_then(|(text, written)| {
        if text == written {
            return Ok(false);
        }
        std::fs::write(file, written)
            .map(|()| true)
            .map_err(|error| format!("{} can't be written: {error}", file.display()))
    });
    match outcome {
        Ok(changed) => {
            let what = if changed { "wrote" } else { "kept" };
            report!("{what} the API's tables in {}", file.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            report!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// `just gen-check`.
fn check(file: &Path) -> ExitCode {
    match read(file) {
        Ok((text, written)) if text == written => {
            report!(
                "the API's tables in {} match the description",
                file.display()
            );
            ExitCode::SUCCESS
        }
        Ok(_) => {
            report!(
                "the API's tables in {} are out of date: run `just gen` and commit the result",
                file.display()
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            report!("{error}");
            ExitCode::FAILURE
        }
    }
}
