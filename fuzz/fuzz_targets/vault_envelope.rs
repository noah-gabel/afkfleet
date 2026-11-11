//! The `vault_envelope` target: its body is in crates/fleet-fuzz.
#![no_main]

use fleet_fuzz::kept::{keep_failing_input, run};

libfuzzer_sys::fuzz_target!(init: keep_failing_input(), |data: &[u8]| run(
    fleet_fuzz::vault::vault_envelope,
    data
));
