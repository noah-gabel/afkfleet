//! The embedded migrations, pinned by their checksums: editing a merged
//! migration changes this snapshot, which shows in review (ADR-0015).
//! `just migrations-check` refuses such an edit outright in CI.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::fmt::Write as _;

use fleet_server::infra::sqlite::MIGRATOR;

#[test]
fn the_migrations_are_pinned_by_their_checksums() {
    let mut lines = String::new();
    for migration in MIGRATOR.iter() {
        let mut checksum = String::new();
        for byte in migration.checksum.iter() {
            write!(checksum, "{byte:02x}").unwrap();
        }
        writeln!(
            lines,
            "{} {} {checksum}",
            migration.version, migration.description
        )
        .unwrap();
    }

    insta::assert_snapshot!(lines);
}
