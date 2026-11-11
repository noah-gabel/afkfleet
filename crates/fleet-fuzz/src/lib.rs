//! The fuzz targets' bodies (Plan.md P13.2, ADR-0030).
//!
//! Each target is a plain function over bytes that panics when a property
//! breaks. cargo-fuzz runs them from `fuzz/`, whose targets are one line
//! each, and this crate's own tests run them over their [`seeds`] and over
//! proptest's inputs, so `just check` holds every property on every
//! platform without a fuzzer.
//!
//! What each target reads is untrusted somewhere: a request's fields, a
//! Minecraft server's text, an agent's or a server's messages, a stored
//! envelope, a scrape's bytes. None may panic, and each says what else must
//! hold:
//! - [`text`]: the value objects, the sanitizer, the chat formats and the
//!   private targets;
//! - [`mode`]: mode definitions, as stored and as the API takes them;
//! - [`wire`]: the control plane's messages, both ways;
//! - [`vault`]: the vault's envelopes;
//! - [`metrics`]: the agent's metrics responder.
//!
//! [`TARGETS`] names them all; `fuzz/`'s targets and the CI job are checked
//! against it.

pub mod metrics;
pub mod mode;
pub mod seeds;
pub mod text;
pub mod vault;
pub mod wire;

use std::borrow::Cow;

/// A fuzz target: its name, as `cargo fuzz run` takes it, and its body.
pub type Target = (&'static str, fn(&[u8]));

/// Every target.
pub const TARGETS: [Target; 10] = [
    ("value_objects", text::value_objects),
    ("chat_sanitizer", text::chat_sanitizer),
    ("chat_formats", text::chat_formats),
    ("chat_sender", text::chat_sender),
    ("private_targets", text::private_targets),
    ("mode_definition", mode::mode_definition),
    ("agent_message", wire::agent_message),
    ("server_message", wire::server_message),
    ("vault_envelope", vault::vault_envelope),
    ("agent_metrics", metrics::agent_metrics),
];

/// `data` as text: its UTF-8, each invalid sequence replaced, so no input
/// is wasted on the decoding.
pub(crate) fn lossy(data: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(data)
}

/// `data` cut at its first zero byte: what's before it and what's after.
/// Without one, everything is before it.
pub(crate) fn halves(data: &[u8]) -> (&[u8], &[u8]) {
    match data.iter().position(|byte| *byte == 0) {
        Some(at) => {
            let (before, after) = data.split_at(at);
            (before, after.get(1..).unwrap_or_default())
        }
        None => (data, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_cut_at_the_first_zero_byte() {
        assert_eq!(halves(b"ab\0cd\0e"), (&b"ab"[..], &b"cd\0e"[..]));
        assert_eq!(halves(b"\0"), (&b""[..], &b""[..]));
        assert_eq!(halves(b"abc"), (&b"abc"[..], &b""[..]));
        assert_eq!(halves(b""), (&b""[..], &b""[..]));
    }

    #[test]
    fn invalid_utf8_still_reads_as_text() {
        assert_eq!(lossy(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn every_target_has_its_own_name() {
        let mut names: Vec<&str> = TARGETS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), TARGETS.len());
    }
}
