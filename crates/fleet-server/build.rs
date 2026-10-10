//! Rebuilds fleet-server whenever a file under `migrations/` is added, changed
//! or removed. `sqlx::migrate!` embeds the migrations, and an edit to an
//! embedded file triggers a rebuild on its own, but a new file doesn't, so
//! without this a new migration could be missing from the binary until
//! something else changed (ADR-0015).

fn main() {
    // Cargo reads build-script directives from stdout, so clippy doesn't apply
    // `print_stdout` to build scripts and no lint exception is needed.
    println!("cargo::rerun-if-changed=migrations");
}
