//! The failing input is kept (ADR-0030): a panic in a target writes the
//! input it ran over to the file it was told, before anything else. One
//! test, in a binary of its own: the panic hook is the process's.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::panic::catch_unwind;

use fleet_fuzz::kept::{keep_failing_input_at, run};

/// A target that fails on any input that starts with `!`.
fn fails_on_a_bang(data: &[u8]) {
    assert!(!data.starts_with(b"!"), "a bang");
}

#[test]
fn a_panic_writes_the_input_it_ran_over_and_no_other() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("failing-input");
    keep_failing_input_at(None);
    keep_failing_input_at(Some(file.clone()));

    run(fails_on_a_bang, b"fine");
    let before_any_panic = file.exists();
    let failed = catch_unwind(|| run(fails_on_a_bang, b"!this one")).is_err();

    assert!(!before_any_panic, "an input that passed was written");
    assert!(failed);
    assert_eq!(std::fs::read(&file).unwrap(), b"!this one");
}
