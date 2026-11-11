//! Every fuzz target, run without a fuzzer (P13.2, ADR-0030): over its own
//! seeds, over arbitrary bytes and over arbitrary text. cargo-fuzz explores
//! much further; this holds each target's properties in `just check`, on
//! every platform, for every input proptest draws.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use fleet_fuzz::TARGETS;
use fleet_fuzz::seeds::seeds;
use proptest::prelude::*;

#[test]
fn every_target_holds_on_its_seeds() {
    for (name, target) in TARGETS {
        for seed in seeds(name) {
            target(&seed);
        }
    }
}

#[test]
fn every_target_holds_on_the_empty_input() {
    for (_, target) in TARGETS {
        target(&[]);
    }
}

proptest! {
    #[test]
    fn every_target_holds_on_any_bytes(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        for (_, target) in TARGETS {
            target(&data);
        }
    }

    #[test]
    fn every_target_holds_on_any_text(text in any::<String>()) {
        for (_, target) in TARGETS {
            target(text.as_bytes());
        }
    }

    /// Each seed with a stretch of its bytes replaced: inputs close to the
    /// valid ones, where a parser's later branches are.
    #[test]
    fn every_target_holds_near_its_seeds(
        at in any::<proptest::sample::Index>(),
        patch in proptest::collection::vec(any::<u8>(), 0..8),
    ) {
        for (name, target) in TARGETS {
            for mut seed in seeds(name) {
                let from = at.index(seed.len().max(1)).min(seed.len());
                let to = (from + patch.len()).min(seed.len());
                seed.splice(from..to, patch.iter().copied());
                target(&seed);
            }
        }
    }
}
