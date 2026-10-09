//! The panic hook: one `error` event per panic, through `tracing`.
//!
//! Only dependencies can panic (the lints keep our own code from it), and
//! their messages can carry server text, which security rule 7 treats as
//! untrusted. So the payload goes through fleet-core's
//! [`sanitize_untrusted`]: one line, at most 1024 characters.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::panic::PanicHookInfo;

use fleet_core::text::sanitize_untrusted;

/// The target of panic reports. The log filter always lets them through.
pub const PANIC_TARGET: &str = "afkfleet::panic";

/// The most characters of a payload that are logged.
const MAX_PAYLOAD_CHARS: usize = 1024;

/// Replaces the default panic hook with [`report`].
pub(crate) fn install_hook() {
    std::panic::set_hook(Box::new(report));
}

/// Logs a panic: where, on which thread, and the sanitized payload.
fn report(info: &PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let location = info.location().map(ToString::to_string);
    let payload = payload_text(info.payload_as_str());
    let backtrace = backtrace_field(&Backtrace::capture());
    tracing::error!(
        target: PANIC_TARGET,
        thread = thread.name().unwrap_or("<unnamed>"),
        location = location.as_deref(),
        payload = %payload,
        backtrace = backtrace.as_deref(),
        "a thread panicked"
    );
}

/// The payload as one sanitized line, with ` [truncated]` when it was cut.
pub(crate) fn payload_text(payload: Option<&str>) -> String {
    let Some(raw) = payload else {
        return "<non-string payload>".to_owned();
    };
    let clean = sanitize_untrusted(raw, MAX_PAYLOAD_CHARS);
    if clean.truncated {
        format!("{} [truncated]", clean.text)
    } else {
        clean.text
    }
}

/// The backtrace, if one was captured (`RUST_BACKTRACE` asks for it).
pub(crate) fn backtrace_field(backtrace: &Backtrace) -> Option<String> {
    (backtrace.status() == BacktraceStatus::Captured).then(|| backtrace.to_string())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain(Some("index out of bounds"), "index out of bounds")]
    #[case::multi_line(Some("first line\nsecond line"), "first line | second line")]
    #[case::escapes_and_codes(Some("\u{1b}[31mred§l\u{202E}!"), "[31mred!")]
    #[case::not_a_string(None, "<non-string payload>")]
    fn payloads_become_one_clean_line(#[case] payload: Option<&str>, #[case] expected: &str) {
        assert_eq!(payload_text(payload), expected);
    }

    #[test]
    fn long_payloads_are_cut_and_marked() {
        let text = payload_text(Some(&"x".repeat(2000)));

        assert_eq!(text, format!("{} [truncated]", "x".repeat(1024)));
    }

    #[test]
    fn a_payload_at_the_cap_is_not_marked() {
        let payload = "x".repeat(1024);

        assert_eq!(payload_text(Some(&payload)), payload);
    }

    #[test]
    fn the_backtrace_is_logged_only_when_captured() {
        assert_eq!(backtrace_field(&Backtrace::disabled()), None);
        assert!(backtrace_field(&Backtrace::force_capture()).is_some());
    }
}
