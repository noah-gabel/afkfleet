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
