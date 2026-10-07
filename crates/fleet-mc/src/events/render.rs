//! [`render`]: server text as plain text, within a fixed budget.
//!
//! azalea's own rendering (`FormattedText`'s `Display`) clones and re-renders
//! every argument of a translation, so a translation that repeats its
//! argument, nested a few levels deep, grows exponentially: a 507-byte system
//! message renders to 16.7 million characters. This renderer follows the same
//! template rules as azalea-chat 0.16's `TranslatableComponent::read`, but it
//! walks the text by reference, with an explicit stack instead of recursion,
//! and stops at [`MAX_CHARS`] characters or [`MAX_STEPS`] steps, whichever
//! comes first (ADR-0011). Every azalea bump re-checks it against azalea-chat
//! (ADR-0003).
//!
//! A step is a component visited, an argument substituted (whatever it
//! renders to) or a character written. Every character of a template is
//! either written or part of a placeholder, so the budget bounds all the work.

use core::iter::Peekable;
use core::ops::ControlFlow;
use core::str::Chars;

use azalea::FormattedText;
use azalea_chat::translatable_component::{PrimitiveOrComponent, TranslatableComponent};

/// The most characters a rendering writes.
pub(crate) const MAX_CHARS: usize = 4_096;

/// The most steps a rendering takes.
pub(crate) const MAX_STEPS: usize = 16_384;

/// Plain text rendered from server text, and whether the budget cut it off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rendered {
    /// The text, without formatting.
    pub(crate) text: String,
    /// The rendering stopped at the budget, so `text` is cut off.
    pub(crate) stopped: bool,
}

/// Renders `text` as the vanilla client shows it, without formatting, within
/// the budget.
pub(crate) fn render(text: &FormattedText) -> Rendered {
    let mut renderer = Renderer::default();
    let mut stack = vec![Frame::Component(text)];
    while let Some(frame) = stack.pop() {
        let flow = match frame {
            Frame::Component(component) => renderer.component(component, &mut stack),
            Frame::Siblings(mut siblings) => {
                if let Some(next) = siblings.next() {
                    stack.push(Frame::Siblings(siblings));
                    stack.push(Frame::Component(next));
                }
                ControlFlow::Continue(())
            }
            Frame::Template(template) => renderer.template(template, &mut stack),
        };
        if flow.is_break() {
            break;
        }
    }
    renderer.finish()
}

/// Renders a plain string within the same budget.
pub(crate) fn render_plain(text: &str) -> Rendered {
    let mut renderer = Renderer::default();
    // Stopping ends the rendering either way.
    let _ = renderer.write_str(text);
    renderer.finish()
}

/// What's left to render, innermost last.
enum Frame<'a> {
    /// A component, then its siblings.
    Component(&'a FormattedText),
    /// The siblings of a component that aren't rendered yet.
    Siblings(core::slice::Iter<'a, FormattedText>),
    /// The rest of a translation's template.
    Template(Template<'a>),
}

/// A translation's template, part of the way through.
struct Template<'a> {
    key: &'a str,
    chars: Peekable<Chars<'a>>,
    args: &'a [PrimitiveOrComponent],
    /// The argument the next `%s` takes; `%N$s` leaves it alone.
    next_arg: usize,
    /// Where the translation's output began, so a malformed template can be
    /// replaced by its key, as azalea does.
    start_len: usize,
    start_chars: usize,
}

impl<'a> Template<'a> {
    /// The translation's template: the language's string for its key, else
    /// the server's fallback, else the key itself.
    fn new(translatable: &'a TranslatableComponent, start_len: usize, start_chars: usize) -> Self {
        let template = azalea_language::get(&translatable.key)
            .or(translatable.fallback.as_deref())
            .unwrap_or(&translatable.key);
        Self {
            key: &translatable.key,
            chars: template.chars().peekable(),
            args: &translatable.args,
            next_arg: 0,
            start_len,
            start_chars,
        }
    }
}

/// The output and the budget spent so far.
#[derive(Default)]
struct Renderer {
    out: String,
    chars: usize,
    steps: usize,
    stopped: bool,
}

impl Renderer {
    fn finish(self) -> Rendered {
        Rendered {
            text: self.out,
            stopped: self.stopped,
        }
    }

    /// Spends a step, or stops if the budget is spent.
    fn step(&mut self) -> ControlFlow<()> {
        if self.steps >= MAX_STEPS {
            self.stopped = true;
            return ControlFlow::Break(());
        }
        self.steps = self.steps.saturating_add(1);
        ControlFlow::Continue(())
    }

    /// Writes one character, which costs a step.
    fn write_char(&mut self, c: char) -> ControlFlow<()> {
        self.step()?;
        if self.chars >= MAX_CHARS {
            self.stopped = true;
            return ControlFlow::Break(());
        }
        self.out.push(c);
        self.chars = self.chars.saturating_add(1);
        ControlFlow::Continue(())
    }

    fn write_str(&mut self, text: &str) -> ControlFlow<()> {
        text.chars().try_for_each(|c| self.write_char(c))
    }

    /// Visits a component: its own text, or its translation, then (from the
    /// stack) its siblings.
    fn component<'a>(
        &mut self,
        component: &'a FormattedText,
        stack: &mut Vec<Frame<'a>>,
    ) -> ControlFlow<()> {
        self.step()?;
        stack.push(Frame::Siblings(component.get_base().siblings.iter()));
        match component {
            FormattedText::Text(text) => self.write_str(&text.text),
            FormattedText::Translatable(translatable) => {
                stack.push(Frame::Template(Template::new(
                    translatable,
                    self.out.len(),
                    self.chars,
                )));
                ControlFlow::Continue(())
            }
        }
    }

    /// Renders a template until it ends, or until an argument that's a
    /// component, which goes on the stack above the rest of the template.
    ///
    /// The rules are azalea-chat's: `%%` is `%`; `%s` is the next argument;
    /// `%N$s` is argument N (one digit); a missing argument is empty; any
    /// other `%` is written as it is. A `%` and a digit without `$s` makes the
    /// template invalid, and the translation shows its key instead.
    fn template<'a>(
        &mut self,
        mut template: Template<'a>,
        stack: &mut Vec<Frame<'a>>,
    ) -> ControlFlow<()> {
        while let Some(c) = template.chars.next() {
            if c != '%' {
                self.write_char(c)?;
                continue;
            }
            let arg = match template.chars.peek().copied() {
                Some('%') => {
                    template.chars.next();
                    self.write_char('%')?;
                    continue;
                }
                Some('s') => {
                    template.chars.next();
                    let index = template.next_arg;
                    template.next_arg = index.saturating_add(1);
                    template.args.get(index)
                }
                Some(digit @ '0'..='9') => {
                    template.chars.next();
                    if template.chars.next() != Some('$') || template.chars.next() != Some('s') {
                        return self.write_key(&template);
                    }
                    // `%0$s` names no argument, so it's empty.
                    digit
                        .to_digit(10)
                        .and_then(|n| usize::try_from(n).ok())
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|index| template.args.get(index))
                }
                // A lone `%` at the end, or one before any other character.
                _ => {
                    self.write_char('%')?;
                    continue;
                }
            };
            self.step()?;
            match arg {
                None => {}
                Some(PrimitiveOrComponent::FormattedText(component)) => {
                    stack.push(Frame::Template(template));
                    stack.push(Frame::Component(component));
                    return ControlFlow::Continue(());
                }
                Some(PrimitiveOrComponent::String(text)) => self.write_str(text)?,
                Some(primitive) => self.write_str(&primitive.to_string())?,
            }
        }
        ControlFlow::Continue(())
    }

    /// Replaces what a malformed template wrote with its key, as azalea's
    /// `read` fails and the translation renders as its key.
    fn write_key(&mut self, template: &Template<'_>) -> ControlFlow<()> {
        self.out.truncate(template.start_len);
        self.chars = template.start_chars;
        self.write_str(template.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Server text from its JSON form.
    fn text(json: &str) -> FormattedText {
        serde_json::from_str(json).unwrap()
    }

    /// A translation that repeats its argument four times, nested `depth`
    /// deep around `innermost`: rendered naively, it grows as 4^depth.
    fn nested(depth: usize, innermost: &str) -> String {
        (0..depth).fold(serde_json::to_string(innermost).unwrap(), |inner, _| {
            format!(r#"{{"translate":"%1$s%1$s%1$s%1$s","with":[{inner}]}}"#)
        })
    }

    // --- The budget ---

    #[test]
    fn nested_arguments_stop_at_the_char_budget() {
        let rendered = render(&text(&nested(12, "x")));

        assert!(rendered.stopped);
        assert_eq!(rendered.text, "x".repeat(MAX_CHARS));
    }

    #[test]
    fn nested_empty_arguments_stop_at_the_step_budget() {
        let rendered = render(&text(&nested(12, "")));

        assert!(rendered.stopped);
        assert_eq!(rendered.text, "");
    }

    #[test]
    fn empty_substitutions_cost_steps_too() {
        let fallback = "%1$s".repeat(MAX_STEPS + 1);
        let json = format!(r#"{{"translate":"t","fallback":"{fallback}","with":[""]}}"#);

        let rendered = render(&text(&json));

        assert!(rendered.stopped);
        assert_eq!(rendered.text, "");
    }

    #[test]
    fn text_up_to_the_char_budget_is_complete() {
        let rendered = render(&FormattedText::from("y".repeat(MAX_CHARS)));

        assert!(!rendered.stopped);
        assert_eq!(rendered.text.chars().count(), MAX_CHARS);
    }

    #[test]
    fn text_over_the_char_budget_is_cut_off() {
        let rendered = render(&FormattedText::from("y".repeat(MAX_CHARS + 1)));

        assert!(rendered.stopped);
        assert_eq!(rendered.text, "y".repeat(MAX_CHARS));
    }

    #[test]
    fn plain_strings_have_the_same_budget() {
        assert_eq!(
            render_plain("hello"),
            Rendered {
                text: "hello".to_owned(),
                stopped: false
            }
        );
        let long = render_plain(&"z".repeat(MAX_CHARS + 1));
        assert!(long.stopped);
        assert_eq!(long.text, "z".repeat(MAX_CHARS));
    }

    #[test]
    fn argument_zero_is_empty() {
        // azalea-chat's own rendering panics here: it computes `d - 1` on the
        // unsigned digit, and overflow checks are on. So it's no reference.
        let rendered = render(&text(
            r#"{"translate":"t.x","fallback":"[%0$s]","with":["a"]}"#,
        ));

        assert_eq!(rendered.text, "[]");
        assert!(!rendered.stopped);
    }

    // --- Fidelity: the same text as azalea-chat's own rendering ---

    #[rstest]
    #[case::known_key_with_args(
        r#"{"translate":"multiplayer.player.joined","with":["AfkBot2"]}"#,
        "AfkBot2 joined the game"
    )]
    #[case::known_key_with_a_component_arg(
        r#"{"translate":"multiplayer.player.joined","with":[{"text":"Afk","extra":["Bot2"]}]}"#,
        "AfkBot2 joined the game"
    )]
    #[case::escaped_percent(r#"{"translate":"t.x","fallback":"100%% sure"}"#, "100% sure")]
    #[case::positional(
        r#"{"translate":"t.x","fallback":"%2$s then %1$s","with":["a","b"]}"#,
        "b then a"
    )]
    #[case::positional_leaves_the_sequence_alone(
        r#"{"translate":"t.x","fallback":"%2$s %s %s","with":["a","b"]}"#,
        "b a b"
    )]
    #[case::json_fallback(r#"{"translate":"no.such.key","fallback":"plan B"}"#, "plan B")]
    #[case::unknown_key_without_fallback(r#"{"translate":"no.such.key"}"#, "no.such.key")]
    #[case::missing_args(r#"{"translate":"t.x","fallback":"[%s][%3$s]"}"#, "[][]")]
    #[case::other_percent_signs(r#"{"translate":"t.x","fallback":"50% off %d %"}"#, "50% off %d %")]
    #[case::malformed_template_shows_the_key(
        r#"{"translate":"t.x","fallback":"before %s %1x","with":["arg"],"extra":["!"]}"#,
        "t.x!"
    )]
    #[case::text_siblings(r#"{"text":"a","extra":["b",{"text":"c","extra":["d"]}]}"#, "abcd")]
    #[case::translation_siblings(
        r#"{"translate":"t.x","fallback":"%s!","with":["x"],"extra":["y",{"translate":"t.y","fallback":"z"}]}"#,
        "x!yz"
    )]
    #[case::primitives(
        r#"{"translate":"t.x","fallback":"%s %s %s","with":[7,true,2.5]}"#,
        "7 true 2.5"
    )]
    fn renders_like_azalea_chat(#[case] json: &str, #[case] expected: &str) {
        let text = text(json);

        let rendered = render(&text);

        assert_eq!(rendered.text, expected);
        assert!(!rendered.stopped);
        assert_eq!(
            rendered.text,
            text.to_string(),
            "azalea-chat renders it the same"
        );
    }
}
