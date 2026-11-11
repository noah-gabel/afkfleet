//! The `private_targets` target: its body is in crates/fleet-fuzz.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| fleet_fuzz::text::private_targets(data));
