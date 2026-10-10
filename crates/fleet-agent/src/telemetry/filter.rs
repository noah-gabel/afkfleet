//! The log layer's filter: the operator's `[log] filter` with azalea's rules
//! on top (ADR-0011, ADR-0014).
//!
//! It's `(EnvFilter ∧ azalea cap ∧ azalea_auth cap) ∨ panic reports`:
//! - **`EnvFilter`** runs the operator's directives, with a default for azalea
//!   in front: `warn`, or the operator's global level if that's lower (`off`
//!   when there's none), so the default only ever lowers azalea. It's left
//!   out when the operator wrote a plain `azalea` directive (no span, no
//!   fields), the only one that would be equally specific, where the order
//!   would decide.
//! - **The azalea cap** holds every `azalea…` target at `warn`, unless a
//!   directive names that target: then it takes the most verbose level the
//!   directives naming it set. A span directive without a target (such as
//!   `[mc_session]=debug`) can enable everything inside a span, but never
//!   azalea's kick rendering.
//! - **The `azalea_auth` cap** keeps `azalea_auth` at `info` whatever the
//!   filter says: its `trace` lines hold the chat-signing private key.
//!   fleet-startup applies it to every binary's filter.
//! - **Panic reports** always get through, since the default panic hook is
//!   gone (fleet-startup too).
//!
//! [`AzaleaRules`] hands the first two to fleet-startup's layer
//! (ADR-0015).

use std::collections::BTreeMap;

use fleet_startup::telemetry::FilterRules;
use tracing_subscriber::filter::{LevelFilter, Targets};

use crate::config::LogFilter;

/// The prefix of every azalea crate's target.
const AZALEA: &str = "azalea";

/// The agent's additions to fleet-startup's filter: azalea's default in
/// front of the operator's directives, the azalea cap, and the startup
/// warning.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AzaleaRules;

impl FilterRules for AzaleaRules {
    fn env_defaults(&self, filter: &LogFilter) -> Option<String> {
        azalea_default(filter)
    }

    fn cap(&self, filter: &LogFilter) -> Targets {
        azalea_cap(filter)
    }

    fn warn_at_startup(&self, filter: &LogFilter) {
        warn_if_azalea_is_lifted(filter);
    }
}

/// azalea's default, to go in front of the operator's directives, unless
/// one of them is a plain `azalea` directive.
fn azalea_default(filter: &LogFilter) -> Option<String> {
    let directives = filter.directives();
    let plain_azalea = directives
        .iter()
        .any(|directive| !directive.scoped && directive.target.as_deref() == Some(AZALEA));
    if plain_azalea {
        return None;
    }
    // The operator's global level: the last directive without a target,
    // span or fields, as in EnvFilter. Without one, nothing else logs.
    let global = directives
        .iter()
        .rev()
        .find(|directive| directive.target.is_none() && !directive.scoped)
        .map_or(LevelFilter::OFF, |directive| directive.level);
    let default = global.min(LevelFilter::WARN);
    Some(format!("{AZALEA}={default}"))
}

/// `warn` for every azalea target, except the ones a directive names: they
/// get the most verbose level the directives naming them set.
fn azalea_cap(filter: &LogFilter) -> Targets {
    let mut levels: BTreeMap<&str, LevelFilter> = BTreeMap::new();
    for directive in filter.directives() {
        if let Some(target) = directive
            .target
            .as_deref()
            .filter(|target| target.starts_with(AZALEA))
        {
            let level = levels.entry(target).or_insert(LevelFilter::OFF);
            *level = (*level).max(directive.level);
        }
    }
    levels.entry(AZALEA).or_insert(LevelFilter::WARN);
    Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_targets(levels)
}

/// Whether the operator's filter names an azalea target above `warn`.
pub(crate) fn lifts_azalea(filter: &LogFilter) -> bool {
    filter.directives().iter().any(|directive| {
        directive
            .target
            .as_deref()
            .is_some_and(|target| target.starts_with(AZALEA))
            && directive.level > LevelFilter::WARN
    })
}

/// Logs one warning when the operator's filter lifts an azalea target above
/// `warn` (ADR-0011).
pub(crate) fn warn_if_azalea_is_lifted(filter: &LogFilter) {
    if lifts_azalea(filter) {
        tracing::warn!(
            "the log filter lifts an azalea target above warn; azalea's own kick \
             rendering at info or more verbose levels can be crashed or slowed down by a \
             hostile server (ADR-0011)"
        );
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::config::{LogConfig, LogFormat};
    use crate::telemetry::capture::Capture;
    use crate::telemetry::{PANIC_TARGET, layer};

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
    const TARGETS: [&str; 5] = [
        "fleet_agent",
        "azalea_client::disconnect",
        "azalea_protocol",
        "azalea_auth",
        "azalea_auth::certs",
    ];

    fn probe_all() {
        probe!("fleet_agent");
        probe!("azalea_client::disconnect");
        probe!("azalea_protocol");
        probe!("azalea_auth");
        probe!("azalea_auth::certs");
    }

    fn config(filter: &str) -> LogConfig {
        LogConfig {
            format: LogFormat::Json,
            filter: LogFilter::try_from(filter).unwrap(),
        }
    }

    /// Runs `emit` under the agent's layer with `filter`, and returns the
    /// most verbose level that got through for each probe target (`OFF` for
    /// none).
    fn most_verbose(filter: &str, emit: impl FnOnce()) -> [LevelFilter; 5] {
        let capture = Capture::default();
        let subscriber =
            tracing_subscriber::registry().with(layer(&config(filter), false, capture.clone()));
        tracing::subscriber::with_default(subscriber, emit);

        let mut seen: BTreeMap<String, LevelFilter> = BTreeMap::new();
        for line in capture.json_lines() {
            if line["message"] != "probe" {
                continue;
            }
            let target = line["target"].as_str().unwrap().to_owned();
            let level: LevelFilter = line["level"].as_str().unwrap().parse().unwrap();
            let entry = seen.entry(target).or_insert(LevelFilter::OFF);
            *entry = (*entry).max(level);
        }
        TARGETS.map(|target| seen.get(target).copied().unwrap_or(LevelFilter::OFF))
    }

    const OFF: LevelFilter = LevelFilter::OFF;
    const ERROR: LevelFilter = LevelFilter::ERROR;
    const WARN: LevelFilter = LevelFilter::WARN;
    const INFO: LevelFilter = LevelFilter::INFO;
    const DEBUG: LevelFilter = LevelFilter::DEBUG;

    #[rstest]
    #[case::default("info", [INFO, WARN, WARN, WARN, WARN], false)]
    #[case::broad_debug("debug", [DEBUG, WARN, WARN, WARN, WARN], false)]
    #[case::named_azalea("debug,azalea=info", [DEBUG, INFO, INFO, INFO, INFO], true)]
    #[case::below_warn("error", [ERROR, ERROR, ERROR, ERROR, ERROR], false)]
    #[case::one_azalea_crate("azalea_client=debug", [OFF, DEBUG, OFF, OFF, OFF], true)]
    #[case::auth_trace("azalea_auth=trace", [OFF, OFF, OFF, INFO, INFO], true)]
    #[case::certs_trace("info,azalea_auth::certs=trace", [INFO, WARN, WARN, WARN, INFO], true)]
    #[case::azalea_off("azalea=off", [OFF, OFF, OFF, OFF, OFF], false)]
    #[case::everything_off("off", [OFF, OFF, OFF, OFF, OFF], false)]
    #[case::span_only_outside("[mc_session]=debug", [OFF, OFF, OFF, OFF, OFF], false)]
    #[case::azalea_span_outside("info,azalea[x]=debug", [INFO, WARN, WARN, WARN, WARN], true)]
    fn azaleas_rules_hold_for_every_filter(
        #[case] filter: &str,
        #[case] expected: [LevelFilter; 5],
        #[case] lifts: bool,
    ) {
        assert_eq!(most_verbose(filter, probe_all), expected, "{filter}");
        assert_eq!(
            lifts_azalea(&LogFilter::try_from(filter).unwrap()),
            lifts,
            "{filter}"
        );
    }

    #[test]
    fn a_span_directive_without_a_target_never_lifts_azalea() {
        let inside = most_verbose("[mc_session]=debug", || {
            let span = tracing::info_span!("mc_session");
            let _entered = span.enter();
            probe_all();
        });

        assert_eq!(inside, [DEBUG, WARN, WARN, WARN, WARN]);
    }

    #[test]
    fn an_azalea_span_directive_lifts_azalea_only_in_its_span() {
        // EnvFilter matches a directive's target against the span too, so
        // `azalea[x]` means azalea's own spans named `x`; inside one, it
        // enables every target at that level, except where the caps hold.
        let inside = most_verbose("info,azalea[x]=debug", || {
            let span = tracing::info_span!(target: "azalea_client", "x");
            let _entered = span.enter();
            probe_all();
        });

        assert_eq!(inside, [DEBUG, DEBUG, DEBUG, INFO, INFO]);
    }

    #[rstest]
    #[case::everything_off("off")]
    #[case::panic_target_off("warn,afkfleet::panic=off")]
    #[case::no_global_level("azalea_client=debug")]
    fn panic_reports_always_get_through(#[case] filter: &str) {
        let capture = Capture::default();
        let subscriber =
            tracing_subscriber::registry().with(layer(&config(filter), false, capture.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: PANIC_TARGET, "probe");
        });

        let lines = capture.json_lines();
        assert_eq!(lines.len(), 1, "{filter}");
        assert_eq!(lines[0]["target"], PANIC_TARGET);
    }

    #[rstest]
    #[case::lifted("debug,azalea_client=info", true)]
    #[case::not_lifted("debug", false)]
    fn the_startup_warning_comes_only_for_a_lifted_azalea(
        #[case] filter: &str,
        #[case] warned: bool,
    ) {
        let capture = Capture::default();
        let subscriber =
            tracing_subscriber::registry().with(layer(&config("info"), false, capture.clone()));
        tracing::subscriber::with_default(subscriber, || {
            warn_if_azalea_is_lifted(&LogFilter::try_from(filter).unwrap());
        });

        let warnings: Vec<_> = capture
            .json_lines()
            .into_iter()
            .filter(|line| line["level"] == "WARN")
            .collect();
        assert_eq!(warnings.len(), usize::from(warned));
        if warned {
            assert!(
                warnings[0]["message"]
                    .as_str()
                    .unwrap()
                    .contains("hostile server")
            );
        }
    }
}
