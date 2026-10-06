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

/// Zero-width and invisible format characters: zero-width space, non-joiner
/// and joiner, word joiner, the byte-order mark and the soft hyphen. They can
/// hide text or make two names look identical (ADR-0010).
const INVISIBLE: [char; 6] = [
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}', '\u{00AD}',
];

/// Whether `c` is a zero-width or invisible format character.
pub(crate) fn is_invisible(c: char) -> bool {
    INVISIBLE.contains(&c)
}

/// Whether [`sanitize`] keeps line breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineBreaks {
    /// Keep `\n`, e.g. for multi-line command output.
    Keep,
    /// Strip `\n` like every other control character, e.g. for names.
    Strip,
}

/// Untrusted text after [`sanitize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sanitized {
    /// The text that's safe to store and display.
    pub(crate) text: String,
    /// Whether characters were cut off at the length cap.
    pub(crate) truncated: bool,
}

/// Makes untrusted text safe to store and display as plain text.
///
/// Strips `§` together with the character after it (a legacy formatting
/// code), control characters (except `\n` with [`LineBreaks::Keep`]),
/// bidirectional controls and invisible characters. Then it keeps at most
/// `max_chars` characters, cutting on a character boundary. Sanitizing twice
/// gives the same text.
pub(crate) fn sanitize(raw: &str, max_chars: usize, line_breaks: LineBreaks) -> Sanitized {
    let mut text = String::new();
    let mut kept = 0_usize;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == SECTION_SIGN {
            // A formatting code is `§` plus one character; drop both.
            chars.next();
            continue;
        }
        if is_stripped(c, line_breaks) {
            continue;
        }
        if kept == max_chars {
            return Sanitized {
                text,
                truncated: true,
            };
        }
        text.push(c);
        kept += 1;
    }
    Sanitized {
        text,
        truncated: false,
    }
}

/// Whether [`sanitize`] drops `c` (apart from formatting codes).
fn is_stripped(c: char, line_breaks: LineBreaks) -> bool {
    let kept_line_break = c == '\n' && line_breaks == LineBreaks::Keep;
    (c.is_control() && !kept_line_break) || is_bidi_control(c) || is_invisible(c)
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

    fn keep(raw: &str) -> String {
        sanitize(raw, 1024, LineBreaks::Keep).text
    }

    #[rstest]
    #[case::color_codes("§cRed §lbold", "Red bold")]
    #[case::trailing_section_sign("abc§", "abc")]
    #[case::section_sign_eats_the_next_one("§§a", "a")]
    #[case::section_sign_eats_a_control("x§\u{7}y", "xy")]
    #[case::keeps_newlines("line 1\nline 2", "line 1\nline 2")]
    #[case::crlf_becomes_lf("a\r\nb", "a\nb")]
    #[case::bell_and_escape("\u{7}bell\u{1B}[31m", "bell[31m")]
    #[case::tab("a\tb", "ab")]
    #[case::c1_control("a\u{85}b", "ab")]
    #[case::ordinary_text("grüß dich 😀 漢字", "grüß dich 😀 漢字")]
    fn sanitize_strips_formatting_and_controls(#[case] raw: &str, #[case] expected: &str) {
        assert_eq!(keep(raw), expected);
    }

    #[rstest]
    #[case::arabic_letter_mark('\u{061C}')]
    #[case::left_to_right_mark('\u{200E}')]
    #[case::right_to_left_mark('\u{200F}')]
    #[case::left_to_right_embedding('\u{202A}')]
    #[case::right_to_left_embedding('\u{202B}')]
    #[case::pop_directional_formatting('\u{202C}')]
    #[case::left_to_right_override('\u{202D}')]
    #[case::right_to_left_override('\u{202E}')]
    #[case::left_to_right_isolate('\u{2066}')]
    #[case::right_to_left_isolate('\u{2067}')]
    #[case::first_strong_isolate('\u{2068}')]
    #[case::pop_directional_isolate('\u{2069}')]
    #[case::zero_width_space('\u{200B}')]
    #[case::zero_width_non_joiner('\u{200C}')]
    #[case::zero_width_joiner('\u{200D}')]
    #[case::word_joiner('\u{2060}')]
    #[case::byte_order_mark('\u{FEFF}')]
    #[case::soft_hyphen('\u{00AD}')]
    fn sanitize_strips_bidi_and_invisible_characters(#[case] c: char) {
        assert_eq!(keep(&format!("a{c}b")), "ab");
    }

    #[test]
    fn sanitize_can_strip_newlines() {
        assert_eq!(sanitize("a\nb", 64, LineBreaks::Strip).text, "ab");
    }

    #[test]
    fn sanitize_keeps_text_at_the_cap() {
        let raw = "x".repeat(1024);
        assert_eq!(
            sanitize(&raw, 1024, LineBreaks::Keep),
            Sanitized {
                text: raw.clone(),
                truncated: false
            }
        );
    }

    #[rstest]
    #[case::ascii("x")]
    #[case::emoji("😀")]
    #[case::cjk("漢")]
    fn sanitize_truncates_on_a_char_boundary(#[case] unit: &str) {
        let result = sanitize(&unit.repeat(1025), 1024, LineBreaks::Keep);
        assert_eq!(result.text, unit.repeat(1024));
        assert!(result.truncated);
    }

    #[test]
    fn sanitize_caps_after_stripping() {
        let raw = format!("§c{}\u{202E}", "x".repeat(1024));
        assert_eq!(
            sanitize(&raw, 1024, LineBreaks::Keep),
            Sanitized {
                text: "x".repeat(1024),
                truncated: false
            }
        );
    }

    /// Whether `c` may be left in text sanitized with [`LineBreaks::Keep`].
    fn is_kept(c: char) -> bool {
        (c == '\n' || !c.is_control())
            && c != SECTION_SIGN
            && !is_bidi_control(c)
            && !is_invisible(c)
    }

    proptest::proptest! {
        #[test]
        fn sanitized_text_is_clean_capped_and_stable(
            raw in proptest::prelude::any::<String>(),
            max_chars in 0..64_usize,
        ) {
            let result = sanitize(&raw, max_chars, LineBreaks::Keep);
            proptest::prop_assert!(result.text.chars().count() <= max_chars);
            proptest::prop_assert!(result.text.chars().all(is_kept));
            let again = sanitize(&result.text, max_chars, LineBreaks::Keep);
            proptest::prop_assert_eq!(again, Sanitized { text: result.text, truncated: false });
        }
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
