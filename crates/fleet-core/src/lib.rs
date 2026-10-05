//! The pure domain core of afkfleet.
//!
//! This crate holds the business rules: domain types and value objects, the bot
//! state machine, retry and circuit-breaker policies, mode scheduling,
//! `authorize()` and the Minecraft port traits. It does no IO and depends on
//! neither tokio nor azalea, so every rule here can be tested deterministically.
//!
//! Phase 0 only proves that the toolchain and quality gates work on this crate;
//! the domain arrives in Phase 2.

/// Returns the name of this crate.
///
/// A placeholder that proves the quality gates run; it's replaced in Phase 2.
#[must_use]
pub const fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_fleet_core() {
        assert_eq!(crate_name(), "fleet-core");
    }
}
