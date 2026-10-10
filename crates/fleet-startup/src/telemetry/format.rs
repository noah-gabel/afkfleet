//! The log layer: `[log] format` plus the filter.

use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::{self, MakeWriter};
use tracing_subscriber::registry::LookupSpan;

use super::filter::{FilterRules, layer_filter};
use crate::config::{LogConfig, LogFormat};

/// A binary's log layer, writing to `writer`: JSON or pretty lines, with the
/// filter that `[log] filter`, the binary's `rules` and the fixed rules make
/// (see [`telemetry`](crate::telemetry)). `ansi` turns on colors for the
/// pretty format; [`init_with`](super::init_with) passes whether stdout is a
/// terminal.
#[must_use]
pub fn layer_with<S, W, R>(
    config: &LogConfig,
    rules: &R,
    ansi: bool,
    writer: W,
) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
    R: FilterRules + ?Sized,
{
    let filter = layer_filter(&config.filter, rules);
    match config.format {
        LogFormat::Json => fmt::layer()
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_writer(writer)
            .with_filter(filter)
            .boxed(),
        LogFormat::Pretty => fmt::layer()
            .with_ansi(ansi)
            .with_writer(writer)
            .with_filter(filter)
            .boxed(),
    }
}

#[cfg(test)]
mod tests {
    use fleet_testkit::log_buffer::LogBuffer;
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::config::LogFilter;

    /// Rules that add nothing.
    struct Plain;

    impl FilterRules for Plain {
        fn env_defaults(&self, _filter: &LogFilter) -> Option<String> {
            None
        }

        fn cap(&self, _filter: &LogFilter) -> Targets {
            Targets::new().with_default(LevelFilter::TRACE)
        }

        fn warn_at_startup(&self, _filter: &LogFilter) {}
    }

    fn log_in_a_span(format: LogFormat, ansi: bool) -> LogBuffer {
        let buffer = LogBuffer::default();
        let config = LogConfig {
            format,
            filter: LogFilter::default(),
        };
        let subscriber =
            tracing_subscriber::registry().with(layer_with(&config, &Plain, ansi, buffer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("bot", bot_id = "b1");
            let _entered = span.enter();
            tracing::info!(state = "online", "state changed");
        });
        buffer
    }

    #[test]
    fn json_lines_put_the_events_fields_at_the_top_level() {
        let lines = log_in_a_span(LogFormat::Json, false).json_lines().unwrap();

        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["level"], "INFO");
        assert_eq!(line["message"], "state changed");
        assert_eq!(line["state"], "online");
        assert_eq!(line["target"], module_path!());
        assert!(line.get("fields").is_none());
        assert_eq!(line["span"]["name"], "bot");
        assert_eq!(line["span"]["bot_id"], "b1");
        assert_eq!(line["spans"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn json_timestamps_are_utc_rfc_3339() {
        let lines = log_in_a_span(LogFormat::Json, false).json_lines().unwrap();

        let timestamp = lines[0]["timestamp"].as_str().unwrap();
        assert_eq!(timestamp.get(4..5), Some("-"));
        assert_eq!(timestamp.get(10..11), Some("T"));
        assert!(timestamp.ends_with('Z'), "{timestamp}");
    }

    #[test]
    fn pretty_lines_are_plain_without_ansi() {
        let text = log_in_a_span(LogFormat::Pretty, false).text();

        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("state changed"));
        assert!(text.contains("state=\"online\"") || text.contains("state=online"));
        assert!(text.contains("bot_id"));
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn pretty_lines_are_colored_with_ansi() {
        let text = log_in_a_span(LogFormat::Pretty, true).text();

        assert!(text.contains('\u{1b}'));
    }
}
