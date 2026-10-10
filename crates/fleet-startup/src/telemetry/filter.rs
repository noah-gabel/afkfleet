//! The log layer's filter: the operator's `[log] filter`, a binary's own
//! rules, and two rules no binary can change (ADR-0011, ADR-0015).
//!
//! It's `(EnvFilter ∧ the binary's cap ∧ azalea_auth cap) ∨ panic reports`:
//! - **`EnvFilter`** runs the operator's directives, after any defaults the
//!   binary puts in front.
//! - **The binary's cap** is whatever its [`FilterRules::cap`] returns.
//! - **The `azalea_auth` cap** keeps `azalea_auth` at `info` whatever the
//!   filter says: its `trace` lines hold secrets.
//! - **Panic reports** always get through, since the default panic hook is
//!   gone.
//!
//! The last two sit outside [`FilterRules`], so no binary's rules can weaken
//! them: a rule can only add a default or narrow the filter.

use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::combinator::{And, Or};
use tracing_subscriber::filter::{FilterExt, LevelFilter, Targets};
use tracing_subscriber::registry::LookupSpan;

use super::panic::PANIC_TARGET;
use crate::config::LogFilter;

/// azalea-auth's target, whose `trace` lines hold secrets.
const AZALEA_AUTH: &str = "azalea_auth";

/// A binary's own additions to the log filter. They can't weaken the
/// `azalea_auth` cap or the panic passthrough, which the filter applies
/// outside them.
pub trait FilterRules {
    /// Directives to put in front of the operator's, or `None`. They must be
    /// valid `EnvFilter` syntax: the filter is built with `parse_lossy`,
    /// which would print a dropped directive to stderr.
    fn env_defaults(&self, filter: &LogFilter) -> Option<String>;

    /// A cap applied together with the operator's filter: an event gets
    /// through only if both let it.
    fn cap(&self, filter: &LogFilter) -> Targets;

    /// Runs once, right after the global subscriber is installed and before
    /// the panic hook, so a warning about the filter reaches the log.
    fn warn_at_startup(&self, filter: &LogFilter);
}

/// The composed filter (see the module docs).
pub(crate) type LayerFilter<S> = Or<And<And<EnvFilter, Targets, S>, Targets, S>, Targets, S>;

/// Builds the layer's filter from the operator's filter and the binary's
/// rules.
pub(crate) fn layer_filter<S, R>(filter: &LogFilter, rules: &R) -> LayerFilter<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    R: FilterRules + ?Sized,
{
    let text = match rules.env_defaults(filter) {
        Some(defaults) => format!("{defaults},{}", filter.as_str()),
        None => filter.as_str().to_owned(),
    };
    // The text is already validated, so nothing is dropped (`parse_lossy`
    // would print a dropped directive to stderr).
    EnvFilter::builder()
        .parse_lossy(text)
        .and(rules.cap(filter))
        .and(
            Targets::new()
                .with_default(LevelFilter::TRACE)
                .with_target(AZALEA_AUTH, LevelFilter::INFO),
        )
        .or(Targets::new().with_target(PANIC_TARGET, LevelFilter::ERROR))
}

/// Rules that add nothing: the operator's filter, with only the fixed
/// `azalea_auth` cap and panic passthrough on top. fleet-server uses them.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRules;

impl FilterRules for NoRules {
    fn env_defaults(&self, _filter: &LogFilter) -> Option<String> {
        None
    }

    /// A cap that lets everything through.
    fn cap(&self, _filter: &LogFilter) -> Targets {
        Targets::new().with_default(LevelFilter::TRACE)
    }

    fn warn_at_startup(&self, _filter: &LogFilter) {}
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fleet_testkit::log_buffer::LogBuffer;
    use rstest::rstest;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::config::{LogConfig, LogFormat};
    use crate::telemetry::layer_with;

    /// Logs one event at every level for `$target`.
    macro_rules! probe {
        ($target:literal) => {
            tracing::trace!(target: $target, "probe");
            tracing::debug!(target: $target, "probe");
            tracing::info!(target: $target, "probe");
            tracing::warn!(target: $target, "probe");
            tracing::error!(target: $target, "probe");
        };
    }

    /// The targets every case is checked for, in this order.
    const TARGETS: [&str; 4] = [
        "fleet_server",
        "sqlx::query",
        "azalea_auth",
        "azalea_auth::certs",
    ];

    fn probe_all() {
        probe!("fleet_server");
        probe!("sqlx::query");
        probe!("azalea_auth");
        probe!("azalea_auth::certs");
    }

    /// Runs `emit` under a layer with no rules and `filter`, and returns the
    /// most verbose level that got through for each probe target (`OFF` for
    /// none).
    fn most_verbose(filter: &str, emit: impl FnOnce()) -> [LevelFilter; 4] {
        let buffer = LogBuffer::default();
        let config = LogConfig {
            format: LogFormat::Json,
            filter: LogFilter::try_from(filter).unwrap(),
        };
        let subscriber = tracing_subscriber::registry().with(layer_with(
            &config,
            &NoRules,
            false,
            buffer.clone(),
        ));
        tracing::subscriber::with_default(subscriber, emit);

        let mut seen: BTreeMap<String, LevelFilter> = BTreeMap::new();
        for line in buffer.lines_with("probe").unwrap() {
            let target = line["target"].as_str().unwrap().to_owned();
            let level: LevelFilter = line["level"].as_str().unwrap().parse().unwrap();
            let entry = seen.entry(target).or_insert(LevelFilter::OFF);
            *entry = (*entry).max(level);
        }
        TARGETS.map(|target| seen.get(target).copied().unwrap_or(LevelFilter::OFF))
    }

    const OFF: LevelFilter = LevelFilter::OFF;
    const WARN: LevelFilter = LevelFilter::WARN;
    const INFO: LevelFilter = LevelFilter::INFO;
    const DEBUG: LevelFilter = LevelFilter::DEBUG;
    const TRACE: LevelFilter = LevelFilter::TRACE;

    #[rstest]
    #[case::default("info", [INFO, INFO, INFO, INFO])]
    #[case::broad_debug("debug", [DEBUG, DEBUG, INFO, INFO])]
    #[case::everything_trace("trace", [TRACE, TRACE, INFO, INFO])]
    #[case::one_target("warn,sqlx=debug", [WARN, DEBUG, WARN, WARN])]
    #[case::auth_trace("azalea_auth=trace", [OFF, OFF, INFO, INFO])]
    #[case::certs_trace("info,azalea_auth::certs=trace", [INFO, INFO, INFO, INFO])]
    #[case::everything_off("off", [OFF, OFF, OFF, OFF])]
    fn no_rules_leave_the_filter_to_the_operator_but_cap_azalea_auth(
        #[case] filter: &str,
        #[case] expected: [LevelFilter; 4],
    ) {
        assert_eq!(most_verbose(filter, probe_all), expected, "{filter}");
    }

    #[rstest]
    #[case::everything_off("off")]
    #[case::panic_target_off("warn,afkfleet::panic=off")]
    #[case::no_global_level("sqlx=debug")]
    fn panic_reports_get_through_with_no_rules(#[case] filter: &str) {
        let buffer = LogBuffer::default();
        let config = LogConfig {
            format: LogFormat::Json,
            filter: LogFilter::try_from(filter).unwrap(),
        };
        let subscriber = tracing_subscriber::registry().with(layer_with(
            &config,
            &NoRules,
            false,
            buffer.clone(),
        ));
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: PANIC_TARGET, "probe");
        });

        let lines = buffer.json_lines().unwrap();
        assert_eq!(lines.len(), 1, "{filter}");
        assert_eq!(lines[0]["target"], PANIC_TARGET);
    }

    #[test]
    fn no_rules_add_no_defaults_and_no_warning() {
        let filter = LogFilter::default();
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::registry().with(layer_with(
            &LogConfig::default(),
            &NoRules,
            false,
            buffer.clone(),
        ));
        tracing::subscriber::with_default(subscriber, || NoRules.warn_at_startup(&filter));

        assert_eq!(NoRules.env_defaults(&filter), None);
        assert_eq!(buffer.text(), "");
    }
}
