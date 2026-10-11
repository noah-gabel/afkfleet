//! Validation: from [`RawConfig`] to [`ServerConfig`], reporting every
//! problem at once.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::raw::{RawAuth, RawConfig, RawLog, RawVault};
use super::{
    Argon2Params, AuthConfig, Bounds, ConfigError, DatabaseConfig, HttpConfig, KeyPath, LogConfig,
    LogFilter, LogFormat, ProblemKind, Problems, ServerConfig, VaultConfig,
};
use fleet_core::auth::SessionPolicy;

/// Validates `raw`: garde's ranges first, then each text key and `[auth]`'s
/// ranges.
///
/// # Errors
/// [`ConfigError::Invalid`](fleet_startup::config::ConfigError::Invalid)
/// with every problem, sorted by key.
pub(crate) fn validate(raw: &RawConfig) -> Result<ServerConfig, ConfigError> {
    let mut problems = Problems::default();
    if let Err(report) = garde::Validate::validate(raw) {
        for (path, error) in report.iter() {
            let key = path
                .to_string()
                .split('.')
                .fold(KeyPath::root(), |key, part| key.key(part));
            problems.push(
                key,
                ProblemKind::OutOfRange {
                    message: error.message().to_owned(),
                },
            );
        }
    }

    let bind = bind(raw.http.bind.as_deref(), &mut problems);
    let path = database_path(raw.database.path.as_deref(), &mut problems);
    let log = log_config(&raw.log, &mut problems);
    let argon2 = argon2(&raw.auth, &mut problems);
    let sessions = sessions(&raw.auth, &mut problems);
    let vault = vault(&raw.vault, &mut problems);

    let (true, Some(bind), Some(path), Some(log), Some(argon2), Some(sessions), Some(vault)) = (
        problems.is_empty(),
        bind,
        path,
        log,
        argon2,
        sessions,
        vault,
    ) else {
        return Err(ConfigError::Invalid(problems.sorted()));
    };
    Ok(ServerConfig {
        dev_mode: raw.dev_mode,
        http: HttpConfig {
            bind,
            request_timeout: Duration::from_secs(raw.http.request_timeout_secs),
            // garde has checked it's at most 1 MiB, which fits any usize
            // afkfleet builds for.
            max_body_bytes: usize::try_from(raw.http.max_body_bytes).unwrap_or(usize::MAX),
        },
        database: DatabaseConfig { path },
        log,
        auth: AuthConfig { argon2, sessions },
        vault,
    })
}

/// `[vault]`: both keys required; 1 to 16 key files, each under a key ID
/// from 1 to 4294967295 written without leading zeros, with an absolute
/// path; the active key ID among them. Whether the active key is listed is
/// checked only when the list itself is valid, so one mistake isn't
/// reported twice.
fn vault(raw: &RawVault, problems: &mut Problems) -> Option<VaultConfig> {
    let section = KeyPath::root().key("vault");
    let files_key = section.key("master_key_files");
    let active_key = section.key("active_key_id");
    let key_files = match &raw.master_key_files {
        None => {
            problems.push(files_key, ProblemKind::Missing);
            None
        }
        Some(entries) => key_files(entries, &files_key, problems),
    };
    let active = match raw.active_key_id {
        None => {
            problems.push(active_key.clone(), ProblemKind::Missing);
            None
        }
        Some(id) => {
            let id = u32::try_from(id).ok().filter(|id| *id >= 1);
            if id.is_none() {
                problems.push(active_key.clone(), ProblemKind::KeyId);
            }
            id
        }
    };
    let (Some(key_files), Some(active_key_id)) = (key_files, active) else {
        return None;
    };
    if !key_files.contains_key(&active_key_id) {
        problems.push(active_key, ProblemKind::UnknownKeyId);
        return None;
    }
    Some(VaultConfig {
        key_files,
        active_key_id,
    })
}

/// `[vault] master_key_files`: every entry is checked, so every problem is
/// reported at once.
fn key_files(
    entries: &BTreeMap<String, String>,
    key: &KeyPath,
    problems: &mut Problems,
) -> Option<BTreeMap<u32, PathBuf>> {
    let mut valid = true;
    if entries.is_empty() || entries.len() > VaultConfig::MAX_KEY_FILES {
        problems.push(
            key.clone(),
            ProblemKind::KeyCount {
                max: VaultConfig::MAX_KEY_FILES,
            },
        );
        valid = false;
    }
    let mut files = BTreeMap::new();
    for (id, path) in entries {
        let entry = key.key(id);
        let Some(id) = key_id(id) else {
            problems.push(entry, ProblemKind::KeyId);
            valid = false;
            continue;
        };
        match absolute_path(path) {
            Ok(path) => {
                files.insert(id, path);
            }
            Err(kind) => {
                problems.push(entry, kind);
                valid = false;
            }
        }
    }
    valid.then_some(files)
}

/// A key ID as written in the config: a whole number from 1 to 4294967295,
/// in its one canonical form (no sign, no leading zeros), so two entries
/// can't name the same key.
fn key_id(text: &str) -> Option<u32> {
    let id: u32 = text.parse().ok()?;
    (id >= 1 && id.to_string() == text).then_some(id)
}

/// Checks each `(key, value, bounds)` of `[auth]` against its range and
/// reports every problem under its key, in garde's words ("lower than",
/// "greater than"), like the other numeric keys. Whether all were in range.
fn check_ranges<const N: usize>(
    checks: [(&'static str, u32, Bounds); N],
    problems: &mut Problems,
) -> bool {
    let section = KeyPath::root().key("auth");
    let mut valid = true;
    for (key, value, bounds) in checks {
        let message = if value < bounds.min {
            format!("lower than {}", bounds.min)
        } else if value > bounds.max {
            format!("greater than {}", bounds.max)
        } else {
            continue;
        };
        problems.push(section.key(key), ProblemKind::OutOfRange { message });
        valid = false;
    }
    valid
}

/// `[auth]`'s token lifetimes: each against its range, then the idle
/// lifetime against the absolute one, which is checked only when both are
/// in range, so one mistake isn't reported twice.
fn sessions(raw: &RawAuth, problems: &mut Problems) -> Option<SessionPolicy> {
    let in_range = check_ranges(
        [
            (
                "access_token_ttl_secs",
                raw.access_token_ttl_secs,
                AuthConfig::ACCESS_TOKEN_TTL_SECS,
            ),
            (
                "refresh_token_idle_days",
                raw.refresh_token_idle_days,
                AuthConfig::REFRESH_TOKEN_IDLE_DAYS,
            ),
            (
                "refresh_token_absolute_days",
                raw.refresh_token_absolute_days,
                AuthConfig::REFRESH_TOKEN_ABSOLUTE_DAYS,
            ),
        ],
        problems,
    );
    if !in_range {
        return None;
    }
    if raw.refresh_token_idle_days > raw.refresh_token_absolute_days {
        problems.push(
            KeyPath::root().key("auth").key("refresh_token_idle_days"),
            ProblemKind::GreaterThanKey {
                key: "refresh_token_absolute_days",
            },
        );
        return None;
    }
    // In range, an hour is always shorter than a day, so only an error that
    // can't happen remains.
    SessionPolicy::try_new(
        Duration::from_secs(u64::from(raw.access_token_ttl_secs)),
        days(raw.refresh_token_idle_days),
        days(raw.refresh_token_absolute_days),
    )
    .ok()
}

/// `n` days.
fn days(n: u32) -> Duration {
    Duration::from_hours(u64::from(n) * 24)
}

/// `[auth] argon2_*`: each value against its range in `Argon2Params`, with
/// every problem reported under its key, in garde's words ("lower than",
/// "greater than"), like the other numeric keys.
fn argon2(raw: &RawAuth, problems: &mut Problems) -> Option<Argon2Params> {
    check_ranges(
        [
            (
                "argon2_memory_kib",
                raw.memory_kib,
                Argon2Params::MEMORY_KIB,
            ),
            (
                "argon2_iterations",
                raw.iterations,
                Argon2Params::ITERATIONS,
            ),
            (
                "argon2_parallelism",
                raw.parallelism,
                Argon2Params::PARALLELISM,
            ),
            (
                "argon2_max_concurrent",
                raw.max_concurrent,
                Argon2Params::MAX_CONCURRENT,
            ),
        ],
        problems,
    );
    Argon2Params::try_new(
        raw.memory_kib,
        raw.iterations,
        raw.parallelism,
        raw.max_concurrent,
    )
    .ok()
}

/// `[http] bind`: an IP address and a port other than 0; missing means
/// [`HttpConfig::DEFAULT_BIND`].
fn bind(raw: Option<&str>, problems: &mut Problems) -> Option<SocketAddr> {
    let key = KeyPath::root().key("http").key("bind");
    let Some(text) = raw else {
        return Some(HttpConfig::DEFAULT_BIND);
    };
    let Ok(address) = text.parse::<SocketAddr>() else {
        problems.push(key, ProblemKind::SocketAddress);
        return None;
    };
    if address.port() == 0 {
        problems.push(key, ProblemKind::PortZero);
        return None;
    }
    Some(address)
}

/// `[database] path`: required, not empty, and absolute.
fn database_path(raw: Option<&str>, problems: &mut Problems) -> Option<PathBuf> {
    let key = KeyPath::root().key("database").key("path");
    let Some(text) = raw else {
        problems.push(key, ProblemKind::Missing);
        return None;
    };
    absolute_path(text)
        .map_err(|kind| problems.push(key, kind))
        .ok()
}

/// A path that must be absolute: not empty, not relative.
fn absolute_path(text: &str) -> Result<PathBuf, ProblemKind> {
    if text.is_empty() {
        return Err(ProblemKind::Empty);
    }
    let path = Path::new(text);
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Err(ProblemKind::NotAbsolute)
    }
}

/// `[log]`: a missing key takes its default.
fn log_config(raw: &RawLog, problems: &mut Problems) -> Option<LogConfig> {
    let section = KeyPath::root().key("log");
    let format = match raw.format.as_deref() {
        None => Some(LogFormat::default()),
        Some(name) => name
            .parse()
            .map_err(|error| problems.push(section.key("format"), ProblemKind::LogFormat(error)))
            .ok(),
    };
    let filter = match raw.filter.as_deref() {
        None => Some(LogFilter::default()),
        Some(text) => LogFilter::try_from(text)
            .map_err(|error| problems.push(section.key("filter"), ProblemKind::LogFilter(error)))
            .ok(),
    };
    Some(LogConfig {
        format: format?,
        filter: filter?,
    })
}
