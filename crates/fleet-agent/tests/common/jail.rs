//! `figment::Jail` for the tests that load configs: its own directory, and an
//! environment that's restored afterwards. Each test file includes only this
//! file (`#[path = "common/jail.rs"]`), so the one approved lint exception
//! stays one (ADR-0014).

use figment::Jail;

/// Runs `test` inside a `figment::Jail`.
#[expect(
    clippy::result_large_err,
    reason = "figment::Jail fixes the closure's error type to figment::Error (ADR-0014)"
)]
pub(crate) fn in_jail(test: impl FnOnce(&mut Jail)) {
    Jail::expect_with(|jail| {
        test(jail);
        Ok(())
    });
}
