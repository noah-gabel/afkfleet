//! Character rules shared by the modules that handle text (ADR-0010).
//!
//! Minecraft text is untrusted and ends up in logs, the database and the app,
//! so the same rules apply everywhere:
//! - control characters (Unicode category Cc) are never allowed
//! - `§` starts a legacy formatting code
//! - bidirectional controls can reorder what a reader sees ("Trojan Source")

/// The section sign, which starts a legacy Minecraft formatting code.
pub(crate) const SECTION_SIGN: char = '§';

/// Unicode's bidirectional controls: the embeddings, overrides and isolates,
/// plus the left-to-right, right-to-left and Arabic letter marks.
const BIDI_CONTROLS: [char; 12] = [
    '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

/// Whether `c` is a bidirectional control.
pub(crate) fn is_bidi_control(c: char) -> bool {
    BIDI_CONTROLS.contains(&c)
}

/// Whether `c` may never appear in a message the bots send: a control
/// character, `§` or a bidirectional control.
pub(crate) fn is_forbidden_in_message(c: char) -> bool {
    c.is_control() || c == SECTION_SIGN || is_bidi_control(c)
}

/// The length of `s` in UTF-16 code units, which is how Minecraft (Java)
/// measures strings.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::newline('\n')]
    #[case::tab('\t')]
    #[case::bell('\u{7}')]
    #[case::delete('\u{7F}')]
    #[case::c1_next_line('\u{85}')]
    #[case::section_sign('§')]
    #[case::right_to_left_override('\u{202E}')]
    #[case::first_strong_isolate('\u{2068}')]
    #[case::arabic_letter_mark('\u{061C}')]
    fn forbids_control_section_and_bidi_characters(#[case] c: char) {
        assert!(is_forbidden_in_message(c));
    }

    #[rstest]
    #[case::letter('a')]
    #[case::space(' ')]
    #[case::slash('/')]
    #[case::umlaut('ü')]
    #[case::emoji('😀')]
    fn allows_ordinary_characters(#[case] c: char) {
        assert!(!is_forbidden_in_message(c));
    }

    #[rstest]
    #[case::ascii("hello", 5)]
    #[case::two_byte_utf8("ü", 1)]
    #[case::astral_plane_emoji("😀", 2)]
    #[case::empty("", 0)]
    fn counts_utf16_code_units(#[case] s: &str, #[case] expected: usize) {
        assert_eq!(utf16_len(s), expected);
    }
}
