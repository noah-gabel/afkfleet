//! `[log]`: the log format and the operator's filter (ADR-0014).

use core::fmt;
use core::str::FromStr;

use tracing_subscriber::filter::{Directive, LevelFilter};

/// `[log]`: how a binary logs. [`crate::telemetry::init_with`] installs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogConfig {
    /// One JSON object per line, or human-readable lines.
    pub format: LogFormat,
    /// The operator's filter. The `azalea_auth` cap and the binary's own
    /// rules apply on top of it.
    pub filter: LogFilter,
}

/// `[log] format`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LogFormat {
    /// `json`: one JSON object per line, with the event's fields at the top
    /// level. The default, for production.
    #[default]
    Json,
    /// `pretty`: one human-readable line per event, colored on a terminal.
    Pretty,
}

impl LogFormat {
    /// Every format, in a fixed order.
    pub const ALL: [Self; 2] = [Self::Json, Self::Pretty];

    /// The format's name, as `[log] format` writes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Pretty => "pretty",
        }
    }
}

impl FromStr for LogFormat {
    type Err = UnknownLogFormatError;

    /// Reads a format's exact name.
    fn from_str(name: &str) -> Result<Self, UnknownLogFormatError> {
        Self::ALL
            .into_iter()
            .find(|format| format.name() == name)
            .ok_or(UnknownLogFormatError)
    }
}

/// Why a name isn't a [`LogFormat`]. The message lists the valid names and
/// never the text that was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown log format; expected one of: {names}", names = FormatNames)]
pub struct UnknownLogFormatError;

/// The names of [`LogFormat::ALL`], separated by `, `.
struct FormatNames;

impl fmt::Display for FormatNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, format) in LogFormat::ALL.into_iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            f.write_str(format.name())?;
        }
        Ok(())
    }
}

/// Why text isn't a valid [`LogFilter`]. No message echoes the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LogFilterError {
    /// The filter has no directive.
    #[error("the log filter has no directive")]
    Empty,
    /// A directive isn't valid filter syntax.
    #[error("directive {index} of the log filter isn't valid (counted from 0, between commas)")]
    InvalidDirective {
        /// The directive's position among the comma-separated pieces.
        index: usize,
    },
}

/// `[log] filter`: the operator's filter, in `EnvFilter` syntax
/// (`info,fleet_runtime=debug`). Spaces around directives are allowed, and
/// empty pieces between commas are skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    text: String,
    directives: Vec<FilterDirective>,
}

/// What a binary's [`FilterRules`](crate::telemetry::FilterRules) need to
/// know about one directive of a [`LogFilter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterDirective {
    /// The target it names, if any (`azalea_client` in
    /// `azalea_client=debug`).
    pub target: Option<String>,
    /// Whether it also names a span or fields, which makes it apply only
    /// there.
    pub scoped: bool,
    /// The most verbose level it enables.
    pub level: LevelFilter,
}

impl FilterDirective {
    /// Reads a parsed directive through its `Display` form,
    /// `target[span{fields}]=level` (any part but the level may be missing),
    /// since `Directive` doesn't expose its parts.
    fn read(directive: &Directive) -> Self {
        let text = directive.to_string();
        let (selector, level) = text.rsplit_once('=').unwrap_or(("", text.as_str()));
        let target = selector.split('[').next().unwrap_or_default();
        Self {
            target: (!target.is_empty()).then(|| target.to_owned()),
            scoped: selector.contains('['),
            // `Directive` displays a valid level, so this never falls back.
            level: level.parse().unwrap_or(LevelFilter::TRACE),
        }
    }
}

impl LogFilter {
    /// The default filter: `info`.
    pub const DEFAULT: &str = "info";

    /// The cleaned filter text: the directives, trimmed, joined by `,`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The directives, in order.
    #[must_use]
    pub fn directives(&self) -> &[FilterDirective] {
        &self.directives
    }
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            text: Self::DEFAULT.to_owned(),
            directives: vec![FilterDirective {
                target: None,
                scoped: false,
                level: LevelFilter::INFO,
            }],
        }
    }
}

impl TryFrom<&str> for LogFilter {
    type Error = LogFilterError;

    fn try_from(text: &str) -> Result<Self, LogFilterError> {
        let mut pieces = Vec::new();
        let mut directives = Vec::new();
        for (index, piece) in text.split(',').enumerate() {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let directive = Directive::from_str(piece)
                .map_err(|_| LogFilterError::InvalidDirective { index })?;
            directives.push(FilterDirective::read(&directive));
            pieces.push(piece);
        }
        if directives.is_empty() {
            return Err(LogFilterError::Empty);
        }
        Ok(Self {
            text: pieces.join(","),
            directives,
        })
    }
}

impl FromStr for LogFilter {
    type Err = LogFilterError;

    fn from_str(text: &str) -> Result<Self, LogFilterError> {
        Self::try_from(text)
    }
}

impl fmt::Display for LogFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn directive(target: Option<&str>, scoped: bool, level: LevelFilter) -> FilterDirective {
        FilterDirective {
            target: target.map(ToOwned::to_owned),
            scoped,
            level,
        }
    }

    #[rstest]
    #[case::json("json", LogFormat::Json)]
    #[case::pretty("pretty", LogFormat::Pretty)]
    fn formats_parse_by_their_exact_names(#[case] name: &str, #[case] format: LogFormat) {
        assert_eq!(name.parse(), Ok(format));
        assert_eq!(format.name(), name);
    }

    #[rstest]
    #[case::upper_case("JSON")]
    #[case::unknown("compact")]
    #[case::empty("")]
    fn other_format_names_are_refused(#[case] name: &str) {
        assert_eq!(name.parse::<LogFormat>(), Err(UnknownLogFormatError));
        assert!(!UnknownLogFormatError.to_string().contains(name) || name.is_empty());
    }

    #[test]
    fn the_defaults_are_json_and_info() {
        let config = LogConfig::default();
        assert_eq!(config.format, LogFormat::Json);
        assert_eq!(config.filter, LogFilter::try_from("info").unwrap());
        assert_eq!(config.filter.as_str(), "info");
    }

    #[test]
    fn a_filter_keeps_its_directives_trimmed() {
        let filter = LogFilter::try_from(" debug , azalea_client=info,, ").unwrap();

        assert_eq!(filter.as_str(), "debug,azalea_client=info");
        assert_eq!(
            filter.directives(),
            [
                directive(None, false, LevelFilter::DEBUG),
                directive(Some("azalea_client"), false, LevelFilter::INFO),
            ]
        );
    }

    #[rstest]
    #[case::bare_target("azalea", directive(Some("azalea"), false, LevelFilter::TRACE))]
    #[case::level_only("warn", directive(None, false, LevelFilter::WARN))]
    #[case::numeric_level(
        "azalea_auth=5",
        directive(Some("azalea_auth"), false, LevelFilter::TRACE)
    )]
    #[case::module_target(
        "azalea_auth::certs=trace",
        directive(Some("azalea_auth::certs"), false, LevelFilter::TRACE)
    )]
    #[case::span(
        "azalea[mc_session]=debug",
        directive(Some("azalea"), true, LevelFilter::DEBUG)
    )]
    #[case::span_only("[mc_session]=debug", directive(None, true, LevelFilter::DEBUG))]
    #[case::fields("[{bot_id=x}]=info", directive(None, true, LevelFilter::INFO))]
    #[case::off("off", directive(None, false, LevelFilter::OFF))]
    fn directives_are_read_like_env_filter(#[case] text: &str, #[case] expected: FilterDirective) {
        assert_eq!(LogFilter::try_from(text).unwrap().directives(), [expected]);
    }

    #[rstest]
    #[case::empty("", LogFilterError::Empty)]
    #[case::only_commas(" , ,", LogFilterError::Empty)]
    #[case::bad_level("info,azalea=loud", LogFilterError::InvalidDirective { index: 1 })]
    #[case::unclosed_span("[mc_session=debug", LogFilterError::InvalidDirective { index: 0 })]
    fn bad_filters_are_refused_without_echoing_them(
        #[case] text: &str,
        #[case] error: LogFilterError,
    ) {
        assert_eq!(LogFilter::try_from(text), Err(error));
        assert!(!error.to_string().contains("loud"));
    }
}
