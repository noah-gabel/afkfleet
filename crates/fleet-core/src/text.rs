//! Character rules shared by the modules that handle text (ADR-0010).
//!
//! Minecraft text is untrusted and ends up in logs, the database and the app,
//! so the same rules apply everywhere:
//! - control characters (Unicode category Cc) are never allowed
//! - `§` starts a legacy formatting code
//! - bidirectional controls can reorder what a reader sees ("Trojan Source")
//! - invisible characters can hide text or make two names look identical

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

/// Invisible characters, as inclusive ranges: the format characters (Unicode
/// category Cf) that show no glyph, the line and paragraph separators, blank
/// fillers and variation selectors. They can hide text, break a name across
/// lines or make two names look identical (ADR-0010). The bidi controls have
/// their own list. U+FE0F stays, because it selects the emoji form of a symbol.
const INVISIBLE: [(char, char); 19] = [
    ('\u{00AD}', '\u{00AD}'),   // soft hyphen
    ('\u{034F}', '\u{034F}'),   // combining grapheme joiner
    ('\u{115F}', '\u{1160}'),   // Hangul choseong and jungseong fillers
    ('\u{17B4}', '\u{17B5}'),   // Khmer inherent vowels
    ('\u{180B}', '\u{180F}'),   // Mongolian variation selectors and vowel separator
    ('\u{200B}', '\u{200D}'),   // zero-width space, non-joiner and joiner
    ('\u{2028}', '\u{2029}'),   // line and paragraph separators
    ('\u{2060}', '\u{2064}'),   // word joiner and invisible math operators
    ('\u{206A}', '\u{206F}'),   // deprecated format characters
    ('\u{3164}', '\u{3164}'),   // Hangul filler
    ('\u{FE00}', '\u{FE0E}'),   // variation selectors 1–15
    ('\u{FEFF}', '\u{FEFF}'),   // byte-order mark
    ('\u{FFA0}', '\u{FFA0}'),   // halfwidth Hangul filler
    ('\u{FFF9}', '\u{FFFB}'),   // interlinear annotation controls
    ('\u{13430}', '\u{1343F}'), // Egyptian hieroglyph format controls
    ('\u{1BCA0}', '\u{1BCA3}'), // shorthand format controls
    ('\u{1D173}', '\u{1D17A}'), // musical symbol format controls
    ('\u{E0000}', '\u{E007F}'), // tags, which can spell out hidden text
    ('\u{E0100}', '\u{E01EF}'), // variation selectors 17–256
];

/// Whether `c` is an invisible character.
pub(crate) fn is_invisible(c: char) -> bool {
    INVISIBLE
        .iter()
        .any(|&(first, last)| (first..=last).contains(&c))
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
    #[case::combining_grapheme_joiner('\u{034F}')]
    #[case::hangul_choseong_filler('\u{115F}')]
    #[case::hangul_jungseong_filler('\u{1160}')]
    #[case::khmer_inherent_vowel('\u{17B4}')]
    #[case::mongolian_variation_selector('\u{180B}')]
    #[case::mongolian_vowel_separator('\u{180E}')]
    #[case::line_separator('\u{2028}')]
    #[case::paragraph_separator('\u{2029}')]
    #[case::function_application('\u{2061}')]
    #[case::invisible_plus('\u{2064}')]
    #[case::inhibit_symmetric_swapping('\u{206A}')]
    #[case::nominal_digit_shapes('\u{206F}')]
    #[case::hangul_filler('\u{3164}')]
    #[case::variation_selector_1('\u{FE00}')]
    #[case::text_variation_selector('\u{FE0E}')]
    #[case::halfwidth_hangul_filler('\u{FFA0}')]
    #[case::interlinear_annotation_anchor('\u{FFF9}')]
    #[case::interlinear_annotation_terminator('\u{FFFB}')]
    #[case::egyptian_hieroglyph_format_control('\u{13430}')]
    #[case::shorthand_format_control('\u{1BCA0}')]
    #[case::musical_symbol_format_control('\u{1D173}')]
    #[case::language_tag('\u{E0001}')]
    #[case::tag_latin_small_a('\u{E0061}')]
    #[case::cancel_tag('\u{E007F}')]
    #[case::variation_selector_17('\u{E0100}')]
    #[case::variation_selector_256('\u{E01EF}')]
    fn sanitize_strips_bidi_and_invisible_characters(#[case] c: char) {
        assert_eq!(keep(&format!("a{c}b")), "ab");
    }

    #[test]
    fn sanitize_keeps_the_emoji_variation_selector() {
        assert_eq!(keep("I \u{2764}\u{FE0F} it"), "I \u{2764}\u{FE0F} it");
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
