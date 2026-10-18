//! The one shape of PEM the agent reads for its identity (ADR-0019, R10,
//! S68): exactly one section and nothing else. The text starts with the
//! section's `BEGIN` line and ends with its `END` line, one line break (LF
//! or CRLF) allowed after it, and holds no other `-----BEGIN` or `-----END`
//! marker, so nothing can sit before, after or beside the section: the PEM
//! reader skips text outside a section, and a stray `END` line too. The
//! section's body is the reader's to check.

/// Whether `text` is exactly one `label` section; see the
/// [module docs](self).
#[must_use]
pub(crate) fn framed(text: &[u8], label: &str) -> bool {
    let Ok(text) = core::str::from_utf8(text) else {
        return false;
    };
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    text.starts_with(&format!("-----BEGIN {label}-----"))
        && text.ends_with(&format!("-----END {label}-----"))
        && text.matches("-----BEGIN").count() == 1
        && text.matches("-----END").count() == 1
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::framed;

    const ONE: &str = "-----BEGIN THING-----\nAAAA\n-----END THING-----";

    #[rstest]
    #[case::bare(ONE.to_owned())]
    #[case::one_line_break(format!("{ONE}\n"))]
    #[case::crlf(format!("{}\r\n", ONE.replace('\n', "\r\n")))]
    fn one_section_with_at_most_one_line_break_is_framed(#[case] text: String) {
        assert!(framed(text.as_bytes(), "THING"));
    }

    #[rstest]
    #[case::text_before(format!("hi\n{ONE}"))]
    #[case::text_after(format!("{ONE}\nhi"))]
    #[case::two_line_breaks(format!("{ONE}\n\n"))]
    #[case::two_sections(format!("{ONE}\n{ONE}"))]
    #[case::a_stray_end(format!("{ONE}\nhi\n-----END THING-----"))]
    #[case::another_label(ONE.replace("THING", "OTHER"))]
    #[case::empty(String::new())]
    fn anything_else_is_not(#[case] text: String) {
        assert!(!framed(text.as_bytes(), "THING"), "{text:?}");
    }

    #[test]
    fn text_that_isnt_utf8_is_not_framed() {
        let mut text = ONE.as_bytes().to_vec();
        text.insert(22, 0xFF);

        assert!(!framed(&text, "THING"));
    }
}
