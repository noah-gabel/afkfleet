//! Loading a binary's config, and why it can fail (ADR-0014, ADR-0015).
//!
//! [`extract`] reads the TOML file at the exact path it's given, lets
//! environment variables with the binary's prefix override any key (the
//! prefix, then the key path in capitals with `__` between the parts), and
//! deserializes the result into the binary's raw config. The binary then
//! validates that into its own config, with its own kind of problem in
//! [`Problems`].
//!
//! - **Parse errors stop at the first** (bad TOML, a wrong type, an unknown
//!   key): nothing can be checked after them.
//! - **Validation reports every problem at once,** each with its key path
//!   (`standalone.bots[1].username`), and never echoes a value.
//!
//! The `[log]` section's types live here too, since both binaries read it.

mod error;
mod log;

use std::path::Path;

use figment::providers::{Env, Format, Toml};
use figment::{Figment, Provider};
use serde::de::DeserializeOwned;

pub use error::{
    ConfigError, KeyPath, KeySegment, ParseError, ParseProblem, ParseSource, Problem, Problems,
};
pub use log::{
    FilterDirective, LogConfig, LogFilter, LogFilterError, LogFormat, UnknownLogFormatError,
};

/// Reads the file at `path`, then the environment variables that start with
/// `env_prefix`, into `R`. Validation is the caller's: this never returns
/// [`ConfigError::Invalid`].
///
/// The file must exist at exactly this path; parent directories aren't
/// searched.
///
/// # Errors
/// - [`ConfigError::NotFound`] when there's no file at `path`.
/// - [`ConfigError::Parse`] for the first thing that can't be read: bad
///   TOML, a value of the wrong type, or an unknown key.
pub fn extract<R, K>(path: &Path, env_prefix: &str) -> Result<R, ConfigError<K>>
where
    R: DeserializeOwned,
{
    // figment treats a missing file as an empty one, so check first.
    if !path.is_file() {
        return Err(ConfigError::NotFound {
            path: path.to_owned(),
        });
    }
    let env = Env::prefixed(env_prefix).split("__");
    let env_name = env.metadata().name.into_owned();
    Figment::new()
        .merge(Toml::file_exact(path))
        .merge(env)
        .extract()
        .map_err(|error| {
            ConfigError::Parse(Box::new(ParseError::from_figment(
                &error, &env_name, env_prefix,
            )))
        })
}
