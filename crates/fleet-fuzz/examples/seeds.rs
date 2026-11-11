//! Writes every target's seeds where cargo-fuzz reads a corpus:
//! `<folder>/<target>/seed-<n>`, the folder given as the one argument.
//!
//!   cargo run -p fleet-fuzz --example seeds -- fuzz/corpus
//!
//! `just fuzz` runs it before a target. It only adds files named `seed-<n>`
//! and never deletes anything: what a run found stays.

use std::path::PathBuf;
use std::{env, fs, io};

use fleet_fuzz::TARGETS;
use fleet_fuzz::seeds::seeds;

fn main() -> io::Result<()> {
    let folder = env::args_os().nth(1).map(PathBuf::from).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "usage: seeds <corpus folder>")
    })?;
    for (name, _) in TARGETS {
        let corpus = folder.join(name);
        fs::create_dir_all(&corpus)?;
        for (n, seed) in seeds(name).iter().enumerate() {
            fs::write(corpus.join(format!("seed-{n}")), seed)?;
        }
    }
    Ok(())
}
