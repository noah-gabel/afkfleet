//! Keeping the input a target fails on.
//!
//! libFuzzer saves a failing input itself when the process dies by a signal
//! it handles. A Rust panic ends a fuzz target through `abort`, which on
//! Windows is a fast-fail no handler sees: the target exits with
//! `0xc0000409` and nothing is saved. So every target runs through [`run`],
//! which remembers its input, and installs [`keep_failing_input`]'s panic
//! hook, which writes that input out before the abort.

use std::path::PathBuf;
use std::sync::Mutex;

/// The environment variable that names the file a failing input is written
/// to; `scripts/fuzz.mjs` sets it for each target. Unset, nothing is kept.
pub const KEEP_AT: &str = "AFKFLEET_FUZZ_KEEP_AT";

/// The input of the target that runs now.
static CURRENT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Runs `target` over `data`, remembering `data` first.
pub fn run(target: fn(&[u8]), data: &[u8]) {
    // Poisoned only if a panic hit this very copy, which doesn't panic.
    if let Ok(mut current) = CURRENT.lock() {
        current.clear();
        current.extend_from_slice(data);
    }
    target(data);
}

/// From now on, a panic first writes the input [`run`] remembered to the
/// file [`KEEP_AT`] names, then goes on as before. Does nothing when the
/// variable isn't set.
pub fn keep_failing_input() {
    keep_failing_input_at(std::env::var_os(KEEP_AT).map(PathBuf::from));
}

/// [`keep_failing_input`], the file given.
pub fn keep_failing_input_at(file: Option<PathBuf>) {
    let Some(file) = file else { return };
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        if let Ok(current) = CURRENT.lock() {
            // Best effort: the panic itself is still reported below.
            let _ = std::fs::write(&file, &*current);
        }
        before(panic);
    }));
}
