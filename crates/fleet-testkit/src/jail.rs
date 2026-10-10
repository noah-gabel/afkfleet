//! `figment::Jail` for the tests that load configs: its own directory, and an
//! environment that's restored afterwards. Every config test in the workspace
//! goes through [`in_jail`], so the one approved lint exception stays one
//! (ADR-0014, ADR-0015).

use figment::Jail;

/// Runs `test` inside a `figment::Jail`. The test's own assertions panic as
/// usual, and `in_jail` passes those panics through.
#[expect(
    clippy::result_large_err,
    reason = "figment::Jail fixes the closure's error type to figment::Error (ADR-0014)"
)]
pub fn in_jail(test: impl FnOnce(&mut Jail)) {
    Jail::expect_with(|jail| {
        test(jail);
        Ok(())
    });
}
