//! Validation: from [`RawConfig`] to [`ServerConfig`], reporting every
//! problem at once.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::raw::{RawConfig, RawLog};
use super::{
    ConfigError, DatabaseConfig, HttpConfig, KeyPath, LogConfig, LogFilter, LogFormat, ProblemKind,
    Problems, ServerConfig,
};

/// Validates `raw`: garde's ranges first, then each text key.
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

    let (true, Some(bind), Some(path), Some(log)) = (problems.is_empty(), bind, path, log) else {
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
    })
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
    if text.is_empty() {
        problems.push(key, ProblemKind::Empty);
        return None;
    }
    let path = Path::new(text);
    if !path.is_absolute() {
        problems.push(key, ProblemKind::NotAbsolute);
        return None;
    }
    Some(path.to_owned())
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
