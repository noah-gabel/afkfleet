//! Turns the raw config into an [`AgentConfig`], collecting every problem.
//!
//! garde checks the numeric ranges first. Then each section goes through
//! fleet-core's constructors, and the rules that span sections run: exactly
//! one mode, the bot count against `max_bots`, and accounts that clash. A
//! check that depends on a key with a problem of its own is skipped, so each
//! mistake is reported once.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use fleet_core::bot::BotAccount;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::mode::ModePreset;
use fleet_core::resilience::{CircuitPolicy, CircuitPolicyError, RetryPolicy, RetryPolicyError};
use fleet_core::value::{AgentName, McUsername, ServerAddress};
use fleet_mc::McConfig;
use fleet_runtime::RuntimeConfig;

use super::raw::{RawBot, RawConfig, RawControlPlane, RawLog, RawRetry, RawRuntime, RawStandalone};
use super::{
    AgentConfig, AgentMode, ConfigError, ControlPlaneConfig, DEFAULT_HEARTBEAT_FILE_NAME, KeyPath,
    LogConfig, LogFilter, LogFormat, ProblemKind, Problems, StandaloneBot,
};

/// Validates `raw`: garde's ranges first, then fleet-core's constructors and
/// the rules that span sections.
///
/// # Errors
/// [`ConfigError::Invalid`] with every problem, sorted by key.
pub(crate) fn validate(raw: &RawConfig) -> Result<AgentConfig, ConfigError> {
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

    let name = agent_name(raw.name.as_deref(), &mut problems);
    let heartbeat_file = heartbeat_file(&raw.runtime, &mut problems);
    let policies = policies(&raw.retry, &mut problems);
    let log = log_config(&raw.log, &mut problems);
    let mode = mode(raw, &mut problems);

    let (true, Some(name), Some(heartbeat_file), Some((retry, circuit)), Some(log), Some(mode)) = (
        problems.is_empty(),
        name,
        heartbeat_file,
        policies,
        log,
        mode,
    ) else {
        return Err(ConfigError::Invalid(problems.sorted()));
    };
    Ok(AgentConfig {
        name,
        runtime: runtime_config(&raw.runtime),
        mc: mc_config(&raw.runtime),
        retry,
        circuit,
        heartbeat_file,
        log,
        mode,
    })
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

fn agent_name(raw: Option<&str>, problems: &mut Problems) -> Option<AgentName> {
    let key = KeyPath::root().key("name");
    let text = required(raw, &key, problems)?;
    AgentName::try_from(text)
        .map_err(|error| problems.push(key, ProblemKind::AgentName(error)))
        .ok()
}

fn heartbeat_file(raw: &RawRuntime, problems: &mut Problems) -> Option<PathBuf> {
    let path = raw
        .heartbeat_file
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(DEFAULT_HEARTBEAT_FILE_NAME));
    if path.is_absolute() {
        Some(path)
    } else {
        problems.push(
            KeyPath::root().key("runtime").key("heartbeat_file"),
            ProblemKind::NotAbsolute,
        );
        None
    }
}

/// The retry and circuit policies. Skipped when a `[retry]` key is already
/// out of range, so fleet-core's rules don't report it a second time.
fn policies(raw: &RawRetry, problems: &mut Problems) -> Option<(RetryPolicy, CircuitPolicy)> {
    let section = KeyPath::root().key("retry");
    if problems.any_under(&section) {
        return None;
    }
    let retry = RetryPolicy::try_new(
        secs(raw.base_delay_secs),
        secs(raw.max_delay_secs),
        secs(raw.stable_after_secs),
    )
    .map_err(|error| problems.push(section.key(retry_key(error)), ProblemKind::Retry(error)))
    .ok();
    let circuit = CircuitPolicy::try_new(
        nonzero(raw.circuit_failures),
        secs(raw.circuit_window_secs),
        secs(raw.circuit_cooldown_secs),
    )
    .map_err(|error| {
        problems.push(section.key(circuit_key(error)), ProblemKind::Circuit(error));
    })
    .ok();
    retry.zip(circuit)
}

/// The `[retry]` key a [`RetryPolicyError`] is about.
fn retry_key(error: RetryPolicyError) -> &'static str {
    match error {
        RetryPolicyError::ZeroBase => "base_delay_secs",
        RetryPolicyError::MaxTooSmall => "max_delay_secs",
        RetryPolicyError::ZeroStableAfter => "stable_after_secs",
    }
}

/// The `[retry]` key a [`CircuitPolicyError`] is about.
fn circuit_key(error: CircuitPolicyError) -> &'static str {
    match error {
        CircuitPolicyError::ZeroWindow => "circuit_window_secs",
        CircuitPolicyError::ZeroCooldown => "circuit_cooldown_secs",
    }
}

fn mode(raw: &RawConfig, problems: &mut Problems) -> Option<AgentMode> {
    match (&raw.standalone, &raw.control_plane) {
        (Some(standalone), None) => {
            standalone_bots(standalone, raw.runtime.max_bots, problems).map(AgentMode::Standalone)
        }
        (None, Some(control_plane)) => {
            control_plane_config(control_plane, problems).map(AgentMode::ControlPlane)
        }
        (Some(_), Some(_)) => {
            problems.push(KeyPath::root(), ProblemKind::BothModes);
            None
        }
        (None, None) => {
            problems.push(KeyPath::root(), ProblemKind::NoMode);
            None
        }
    }
}

/// Every bot entry, plus the count and the accounts. The count is checked
/// against `max_bots` only when `max_bots` itself is valid, and only valid
/// names are checked for clashes.
fn standalone_bots(
    raw: &RawStandalone,
    max_bots: u64,
    problems: &mut Problems,
) -> Option<Vec<StandaloneBot>> {
    let key = KeyPath::root().key("standalone").key("bots");
    let count = raw.bots.len();
    let max = usize::try_from(max_bots).unwrap_or(usize::MAX);
    if count == 0 {
        problems.push(key.clone(), ProblemKind::NoBots);
    } else if count > max && !problems.any_under(&KeyPath::root().key("runtime").key("max_bots")) {
        problems.push(key.clone(), ProblemKind::TooManyBots { count, max });
    }

    let mut accounts: Vec<(usize, BotAccount)> = Vec::new();
    let bots: Vec<Option<StandaloneBot>> = raw
        .bots
        .iter()
        .enumerate()
        .map(|(index, bot)| standalone_bot(bot, &key.index(index), index, &mut accounts, problems))
        .collect();
    bots.into_iter().collect()
}

fn standalone_bot(
    raw: &RawBot,
    key: &KeyPath,
    index: usize,
    accounts: &mut Vec<(usize, BotAccount)>,
    problems: &mut Problems,
) -> Option<StandaloneBot> {
    let username = parsed(
        raw.username.as_deref(),
        &key.key("username"),
        problems,
        McUsername::try_from,
        ProblemKind::Username,
    );
    if let Some(username) = &username {
        let account = BotAccount::Offline(username.clone());
        match accounts
            .iter()
            .find(|(_, other)| other.clashes_with(&account))
        {
            Some(&(with, _)) => problems.push(key.key("username"), ProblemKind::Clash { with }),
            None => accounts.push((index, account)),
        }
    }
    let server = parsed(
        raw.server.as_deref(),
        &key.key("server"),
        problems,
        ServerAddress::try_from,
        ProblemKind::Server,
    );
    let mode = parsed(
        raw.mode.as_deref(),
        &key.key("mode"),
        problems,
        str::parse::<ModePreset>,
        ProblemKind::Mode,
    );
    let conflict_texts = ConflictTexts::try_new(&raw.conflict_texts)
        .map_err(|error| {
            problems.push(key.key("conflict_texts"), ProblemKind::ConflictTexts(error));
        })
        .ok();
    Some(StandaloneBot {
        username: username?,
        server: server?,
        mode: mode?,
        conflict_texts: conflict_texts?,
    })
}

fn control_plane_config(
    raw: &RawControlPlane,
    problems: &mut Problems,
) -> Option<ControlPlaneConfig> {
    let section = KeyPath::root().key("control_plane");
    let url = non_empty(raw.url.as_deref(), &section.key("url"), problems);
    let ca_cert_file = non_empty(
        raw.ca_cert_file.as_deref(),
        &section.key("ca_cert_file"),
        problems,
    );
    let cert_file = non_empty(
        raw.cert_file.as_deref(),
        &section.key("cert_file"),
        problems,
    );
    let key_file = non_empty(raw.key_file.as_deref(), &section.key("key_file"), problems);
    Some(ControlPlaneConfig {
        url: url?.to_owned(),
        ca_cert_file: PathBuf::from(ca_cert_file?),
        cert_file: PathBuf::from(cert_file?),
        key_file: PathBuf::from(key_file?),
    })
}

/// The value of a required key, or a [`ProblemKind::Missing`].
fn required<'a>(value: Option<&'a str>, key: &KeyPath, problems: &mut Problems) -> Option<&'a str> {
    if value.is_none() {
        problems.push(key.clone(), ProblemKind::Missing);
    }
    value
}

/// The value of a required key that mustn't be empty.
fn non_empty<'a>(
    value: Option<&'a str>,
    key: &KeyPath,
    problems: &mut Problems,
) -> Option<&'a str> {
    let text = required(value, key, problems)?;
    if text.is_empty() {
        problems.push(key.clone(), ProblemKind::Empty);
        return None;
    }
    Some(text)
}

/// A required key, parsed by a fleet-core constructor.
fn parsed<'a, T, E>(
    value: Option<&'a str>,
    key: &KeyPath,
    problems: &mut Problems,
    parse: impl FnOnce(&'a str) -> Result<T, E>,
    problem: impl FnOnce(E) -> ProblemKind,
) -> Option<T> {
    let text = required(value, key, problems)?;
    parse(text)
        .map_err(|error| problems.push(key.clone(), problem(error)))
        .ok()
}

fn runtime_config(raw: &RawRuntime) -> RuntimeConfig {
    RuntimeConfig {
        max_bots: nonzero(raw.max_bots),
        watchdog_timeout: secs(raw.watchdog_timeout_secs),
        packet_liveness_timeout: secs(raw.packet_liveness_timeout_secs),
        connect_timeout: secs(raw.connect_timeout_secs),
        shutdown_timeout: secs(raw.shutdown_timeout_secs),
        ..RuntimeConfig::default()
    }
}

fn mc_config(raw: &RawRuntime) -> McConfig {
    McConfig {
        max_abandoned_threads: nonzero(raw.max_abandoned_threads),
        ..McConfig::default()
    }
}

const fn secs(value: u64) -> Duration {
    Duration::from_secs(value)
}

/// A count that garde has already checked to be at least 1.
fn nonzero(value: u64) -> NonZeroUsize {
    let above_one = usize::try_from(value.saturating_sub(1)).unwrap_or(usize::MAX);
    NonZeroUsize::MIN.saturating_add(above_one)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::zero_base(RetryPolicyError::ZeroBase, "base_delay_secs")]
    #[case::max_too_small(RetryPolicyError::MaxTooSmall, "max_delay_secs")]
    #[case::zero_stable_after(RetryPolicyError::ZeroStableAfter, "stable_after_secs")]
    fn retry_errors_name_their_key(#[case] error: RetryPolicyError, #[case] key: &str) {
        assert_eq!(retry_key(error), key);
    }

    #[rstest]
    #[case::zero_window(CircuitPolicyError::ZeroWindow, "circuit_window_secs")]
    #[case::zero_cooldown(CircuitPolicyError::ZeroCooldown, "circuit_cooldown_secs")]
    fn circuit_errors_name_their_key(#[case] error: CircuitPolicyError, #[case] key: &str) {
        assert_eq!(circuit_key(error), key);
    }

    #[rstest]
    #[case::one(1, 1)]
    #[case::many(50, 50)]
    #[case::zero_is_clamped(0, 1)]
    fn counts_become_nonzero(#[case] value: u64, #[case] expected: usize) {
        assert_eq!(nonzero(value).get(), expected);
    }
}
