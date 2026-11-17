//! The one way a declaration's text holds a named part: `{name}`, with `{{`
//! and `}}` for a literal brace, as a chat format writes them (ADR-0020,
//! U59). A declared message's values and a chat pattern's captures are both
//! read by [`parse`].

use super::Ident;

/// A part of a declaration's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segment {
    /// This text, exactly: never empty, its doubled braces already single.
    Literal(String),
    /// A named part.
    Named(Ident),
}

/// A brace that's neither part of a `{name}` nor doubled, or a `{name}`
/// whose name isn't an [`Ident`]: where it starts, counted in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BadBrace {
    /// The brace's position in the text.
    pub(crate) position: usize,
}

/// A named part with where its `{` is in the text, counted in characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// The parts, in order; two literals are never side by side.
    pub(crate) segments: Vec<Segment>,
    /// For each [`Segment::Named`], in order, its `{`'s position.
    pub(crate) positions: Vec<usize>,
}

/// Reads `text` into its parts.
///
/// Reserving every other brace means a typo such as `{Amount}` or `{amount`
/// is an error instead of literal text, and a later kind of placeholder
/// can't change what a stored declaration means.
pub(crate) fn parse(text: &str) -> Result<Parsed, BadBrace> {
    let mut parsed = Parsed {
        segments: Vec::new(),
        positions: Vec::new(),
    };
    let mut literal = String::new();
    let mut chars = text.chars().enumerate().peekable();
    while let Some((position, c)) = chars.next() {
        match c {
            '{' | '}' if chars.peek().is_some_and(|&(_, next)| next == c) => {
                chars.next();
                literal.push(c);
            }
            '{' => {
                // The name runs to the next brace, which must close it.
                let mut name = String::new();
                let closed = loop {
                    match chars.next() {
                        Some((_, '}')) => break true,
                        Some((_, '{')) | None => break false,
                        Some((_, other)) => name.push(other),
                    }
                };
                let ident = closed
                    .then(|| Ident::try_from(name.as_str()).ok())
                    .flatten()
                    .ok_or(BadBrace { position })?;
                if !literal.is_empty() {
                    parsed
                        .segments
                        .push(Segment::Literal(core::mem::take(&mut literal)));
                }
                parsed.segments.push(Segment::Named(ident));
                parsed.positions.push(position);
            }
            '}' => return Err(BadBrace { position }),
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        parsed.segments.push(Segment::Literal(literal));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn literal(text: &str) -> Segment {
        Segment::Literal(text.to_owned())
    }

    fn named(name: &str) -> Segment {
        Segment::Named(Ident::try_from(name).unwrap())
    }

    #[rstest]
    #[case::nothing("", vec![], vec![])]
    #[case::only_text("/home farm", vec![literal("/home farm")], vec![])]
    #[case::one_name("/ah sell {price}", vec![literal("/ah sell "), named("price")], vec![9])]
    #[case::a_name_first("{amount} coins", vec![named("amount"), literal(" coins")], vec![0])]
    #[case::two_names(
        "/pay {to} {amount}",
        vec![literal("/pay "), named("to"), literal(" "), named("amount")],
        vec![5, 10]
    )]
    #[case::names_side_by_side("{a}{b}", vec![named("a"), named("b")], vec![0, 3])]
    #[case::doubled_braces("a {{b}} c", vec![literal("a {b} c")], vec![])]
    #[case::a_doubled_brace_beside_a_name(
        "{{{a}}}",
        vec![literal("{"), named("a"), literal("}")],
        vec![2]
    )]
    #[case::positions_count_characters("\u{fc}\u{fc} {a}", vec![literal("\u{fc}\u{fc} "), named("a")], vec![3])]
    fn reads_text_into_its_parts(
        #[case] text: &str,
        #[case] segments: Vec<Segment>,
        #[case] positions: Vec<usize>,
    ) {
        assert_eq!(
            parse(text),
            Ok(Parsed {
                segments,
                positions
            })
        );
    }

    #[rstest]
    #[case::an_opening_brace_alone("a { b", 2)]
    #[case::a_closing_brace_alone("a } b", 2)]
    #[case::unclosed("a {name", 2)]
    #[case::unclosed_at_the_end("a {", 2)]
    #[case::an_empty_name("a {} b", 2)]
    #[case::an_upper_case_name("{Amount}", 0)]
    #[case::a_name_with_a_space("{a b}", 0)]
    #[case::a_name_too_long("{abcdefghijklmnopqrstuvwxyz0123456}", 0)]
    #[case::a_brace_inside_a_name("{a{b}}", 0)]
    #[case::a_closing_brace_after_a_doubled_one("}}}", 2)]
    #[case::the_first_one_wins("} {", 0)]
    fn refuses_any_other_brace(#[case] text: &str, #[case] position: usize) {
        assert_eq!(parse(text), Err(BadBrace { position }));
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = parse(&text);
        }

        /// Text without a brace is one literal, itself.
        #[test]
        fn text_without_braces_is_itself(text in "[^{}]{1,40}") {
            let parsed = parse(&text).unwrap();

            prop_assert_eq!(parsed.segments, vec![Segment::Literal(text)]);
        }

        /// Whatever is accepted: no empty literal, no two literals side by
        /// side, and a position for each name.
        #[test]
        fn what_is_accepted_is_well_formed(text in "[a-c {}]{0,24}") {
            if let Ok(parsed) = parse(&text) {
                let names = parsed
                    .segments
                    .iter()
                    .filter(|segment| matches!(segment, Segment::Named(_)))
                    .count();
                prop_assert_eq!(names, parsed.positions.len());
                for pair in parsed.segments.windows(2) {
                    let both_literal = matches!(pair, [Segment::Literal(_), Segment::Literal(_)]);
                    prop_assert!(!both_literal);
                }
                for segment in &parsed.segments {
                    prop_assert_ne!(segment, &Segment::Literal(String::new()));
                }
            }
        }
    }
}
