//! A scan of a script's text for the places Rhai's public API doesn't
//! give: where each function is defined, where something stands outside
//! a function, where a `for` or a `catch` names its variable, and where
//! the first raw string is (`#"…"#`, which a script may not hold:
//! ADR-0040's amendment, A6).
//!
//! It also notes what of API level 2's the text names (ADR-0052, G1): a
//! level-2 object named as one, never as a member, a map's key, a call of
//! a function or its definition; and `bot.name`, which no compile finds,
//! since Rhai looks a member up when the line runs. Inside a text's
//! `${ … }` too.
//!
//! It judges nothing but a raw string, so it may run before Rhai has
//! parsed the text; it skips comments and texts as Rhai's tokenizer does
//! and reads names, braces and parentheses. A place it can't find is `None`,
//! never a wrong one, with one consequence that matters: a `for` or a
//! `catch` it didn't see isn't checked for a game object's name. Rhai
//! would then hand the loop the game object instead of the loop's own
//! value, which fails where it runs.

use core::cell::Cell;

use crate::api::OBJECTS_2;
use crate::error::SourcePosition;

/// A function as its definition is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Definition {
    /// Its name.
    pub(crate) name: String,
    /// Its parameters' names.
    pub(crate) parameters: Vec<String>,
    /// Where its `fn` is.
    pub(crate) at: SourcePosition,
}

/// A variable a `for` or a `catch` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    /// Its name.
    pub(crate) name: String,
    /// Where the name is.
    pub(crate) at: SourcePosition,
}

/// What of API level 2's a text names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Newer {
    /// The level-2 objects it names as objects, each once, in the order
    /// first named.
    pub(crate) objects: Vec<&'static str>,
    /// Where `bot.name` is first read.
    pub(crate) bot_name: Option<SourcePosition>,
}

/// What a scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Scanned {
    /// The functions, in the order they're written.
    pub(crate) definitions: Vec<Definition>,
    /// The variables `for` and `catch` name, in order.
    pub(crate) bindings: Vec<Binding>,
    /// The first thing outside every function.
    pub(crate) outside: Option<SourcePosition>,
    /// The first raw string, wherever it is.
    pub(crate) raw_string: Option<SourcePosition>,
    /// What of API level 2's it names.
    pub(crate) newer: Newer,
}

impl Scanned {
    /// Where the function `name` with `parameters` parameters is defined:
    /// the first such definition, or the first of that name.
    pub(crate) fn definition(&self, name: &str, parameters: usize) -> Option<SourcePosition> {
        let named = || self.definitions.iter().filter(|found| found.name == name);
        named()
            .find(|found| found.parameters.len() == parameters)
            .or_else(|| named().next())
            .map(|found| found.at)
    }
}

/// A piece of a script's text that the scan reads.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A name or a keyword.
    Word(String),
    /// Any other character that stands for itself.
    Symbol(char),
}

/// Reads the text's tokens with their places, skipping comments and texts.
struct Lexer<'a> {
    chars: core::iter::Peekable<core::str::Chars<'a>>,
    line: u32,
    column: u32,
    /// How many texts in backticks the scan is inside of.
    nesting: u32,
    /// Where the first raw string is, even one inside a text's `${ … }`.
    raw_string: &'a Cell<Option<SourcePosition>>,
    /// The three tokens before the next one, the last nearest.
    before: [Option<Token>; 3],
    /// A level-2 object's name waiting for the token after it, which says
    /// whether it's named as the object.
    pending: Option<&'static str>,
    /// What of API level 2's the tokens so far name.
    newer: Newer,
}

/// How deep texts in backticks may hold code that holds such a text. Rhai
/// refuses a script long before; past it the scan ends instead of going
/// deeper.
const MAX_NESTING: u32 = 32;

impl Lexer<'_> {
    /// The next character, counted.
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.column = 0;
        } else {
            self.column = self.column.saturating_add(1);
        }
        Some(c)
    }

    /// The character after the next, without taking either.
    fn second(&self) -> Option<char> {
        let mut ahead = self.chars.clone();
        ahead.next();
        ahead.next()
    }

    /// Skips to the end of the line.
    fn skip_line(&mut self) {
        while self.chars.peek().is_some_and(|c| *c != '\n') {
            self.bump();
        }
    }

    /// Skips a block comment whose `/*` is taken; they nest.
    fn skip_block(&mut self) {
        let mut depth = 1_u32;
        while depth > 0 {
            match self.bump() {
                Some('*') if self.chars.peek() == Some(&'/') => {
                    self.bump();
                    depth -= 1;
                }
                Some('/') if self.chars.peek() == Some(&'*') => {
                    self.bump();
                    depth = depth.saturating_add(1);
                }
                Some(_) => {}
                None => return,
            }
        }
    }

    /// Skips a text whose opening `quote` is taken, with its escapes.
    fn skip_text(&mut self, quote: char) {
        while let Some(c) = self.bump() {
            match c {
                '\\' => {
                    self.bump();
                }
                _ if c == quote => return,
                _ => {}
            }
        }
    }

    /// How many `#` come before a `"` right after the `#` just taken:
    /// `Some` for a raw string's opening, with its count of `#` in all.
    fn raw_opening(&self) -> Option<usize> {
        let mut ahead = self.chars.clone();
        let mut hashes = 1_usize;
        loop {
            match ahead.next()? {
                '#' => hashes = hashes.saturating_add(1),
                '"' => return Some(hashes),
                _ => return None,
            }
        }
    }

    /// Skips a raw string whose first `#` is taken, to the `"` and as
    /// many `#` that close it; nothing in it is an escape.
    fn skip_raw(&mut self, hashes: usize) {
        for _ in 0..hashes {
            self.bump();
        }
        while let Some(c) = self.bump() {
            if c != '"' {
                continue;
            }
            let mut closing = 0_usize;
            while closing < hashes && self.chars.peek() == Some(&'#') {
                self.bump();
                closing = closing.saturating_add(1);
            }
            if closing == hashes {
                return;
            }
        }
    }

    /// Skips a text in backticks whose opening one is taken. A doubled
    /// backtick is one of its characters, and `${ … }` holds code, which
    /// is skipped by its braces, texts inside it included.
    fn skip_template(&mut self) {
        self.nesting = self.nesting.saturating_add(1);
        if self.nesting > MAX_NESTING {
            // Nothing more is read.
            while self.bump().is_some() {}
        }
        self.skip_template_body();
        self.nesting = self.nesting.saturating_sub(1);
    }

    /// The inside of a text in backticks, to the backtick that closes it.
    fn skip_template_body(&mut self) {
        while let Some(c) = self.bump() {
            match c {
                '`' if self.chars.peek() == Some(&'`') => {
                    self.bump();
                }
                '`' => return,
                '$' if self.chars.peek() == Some(&'{') => {
                    self.bump();
                    let mut depth = 1_u32;
                    while depth > 0 {
                        match self.next() {
                            Some((Token::Symbol('{'), _)) => depth = depth.saturating_add(1),
                            Some((Token::Symbol('}'), _)) => depth -= 1,
                            Some(_) => {}
                            None => return,
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl Lexer<'_> {
    /// Notes what of API level 2's `next` and the tokens before it name;
    /// `None` is the text's end.
    fn note(&mut self, next: Option<&(Token, SourcePosition)>) {
        if let Some(object) = self.pending.take() {
            // A call of a function of the script's own, or a map's key.
            let own = matches!(next, Some((Token::Symbol('(' | ':'), _)));
            if !own && !self.newer.objects.contains(&object) {
                self.newer.objects.push(object);
            }
        }
        let Some((token, at)) = next else {
            return;
        };
        let is =
            |token: &Option<Token>, word: &str| matches!(token, Some(Token::Word(w)) if w == word);
        let symbol =
            |token: &Option<Token>, c: char| matches!(token, Some(Token::Symbol(s)) if *s == c);
        let [third, second, last] = &self.before;
        if let Token::Word(word) = token {
            let after_dot = symbol(last, '.');
            if word == "name" && after_dot && self.newer.bot_name.is_none() {
                // `bot.name` and `bot?.name`.
                let of_bot = is(second, "bot") || (symbol(second, '?') && is(third, "bot"));
                if of_bot {
                    self.newer.bot_name = Some(*at);
                }
            }
            let named = !after_dot && !is(last, "fn");
            if named {
                self.pending = OBJECTS_2.iter().copied().find(|object| object == word);
            }
        }
        self.before = [second.clone(), last.clone(), Some(token.clone())];
    }

    /// The next token, unnoted.
    fn read(&mut self) -> Option<(Token, SourcePosition)> {
        loop {
            let c = self.bump()?;
            let at = SourcePosition {
                line: self.line,
                column: self.column,
            };
            match c {
                _ if c.is_whitespace() => {}
                '/' if self.chars.peek() == Some(&'/') => self.skip_line(),
                '/' if self.chars.peek() == Some(&'*') => {
                    self.bump();
                    self.skip_block();
                }
                '"' => self.skip_text('"'),
                '#' if let Some(hashes) = self.raw_opening() => {
                    if self.raw_string.get().is_none() {
                        self.raw_string.set(Some(at));
                    }
                    self.skip_raw(hashes);
                    return Some((Token::Symbol('#'), at));
                }
                '`' => self.skip_template(),
                // A character in quotes: 'x' or an escape. A lone quote
                // isn't valid Rhai; it's taken as a symbol.
                '\'' if self.chars.peek() == Some(&'\\') || self.second() == Some('\'') => {
                    self.skip_text('\'');
                }
                _ if c.is_alphabetic() || c == '_' => {
                    let mut word = String::from(c);
                    while let Some(next) = self.chars.peek().copied() {
                        if next.is_alphanumeric() || next == '_' {
                            word.push(next);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    return Some((Token::Word(word), at));
                }
                _ => return Some((Token::Symbol(c), at)),
            }
        }
    }
}

impl Iterator for Lexer<'_> {
    type Item = (Token, SourcePosition);

    fn next(&mut self) -> Option<(Token, SourcePosition)> {
        let next = self.read();
        self.note(next.as_ref());
        next
    }
}

/// Scans a script's text.
pub(crate) fn scan(source: &str) -> Scanned {
    let raw_string = Cell::new(None);
    let mut lexer = Lexer {
        chars: source.chars().peekable(),
        line: 1,
        column: 0,
        nesting: 0,
        raw_string: &raw_string,
        before: [None, None, None],
        pending: None,
        newer: Newer::default(),
    };
    let mut tokens = (&mut lexer).peekable();
    let mut scanned = Scanned::default();
    // How many braces are open. Functions are defined at none.
    let mut depth = 0_u32;
    while let Some((token, at)) = tokens.next() {
        match token {
            Token::Symbol('{') => depth = depth.saturating_add(1),
            Token::Symbol('}') => depth = depth.saturating_sub(1),
            Token::Word(word) if depth == 0 && word == "private" => {}
            Token::Word(word) if depth == 0 && word == "fn" => {
                let Some((Token::Word(name), _)) = tokens.next() else {
                    continue;
                };
                // The parameters run to the parenthesis that closes them.
                let mut parameters = Vec::new();
                for (token, _) in tokens.by_ref() {
                    match token {
                        Token::Word(parameter) => parameters.push(parameter),
                        Token::Symbol(')') => break,
                        Token::Symbol(_) => {}
                    }
                }
                scanned.definitions.push(Definition {
                    name,
                    parameters,
                    at,
                });
            }
            Token::Word(word) if word == "for" || word == "catch" => {
                // `for x in`, `for (x, i) in` and `catch (x)`.
                let grouped =
                    word == "catch" || matches!(tokens.peek(), Some((Token::Symbol('('), _)));
                if grouped {
                    if !matches!(tokens.peek(), Some((Token::Symbol('('), _))) {
                        continue;
                    }
                    tokens.next();
                    while let Some((token, at)) =
                        tokens.next_if(|(token, _)| !matches!(token, Token::Symbol(')' | '{')))
                    {
                        if let Token::Word(name) = token {
                            scanned.bindings.push(Binding { name, at });
                        }
                    }
                } else if let Some((Token::Word(name), at)) =
                    tokens.next_if(|(token, _)| matches!(token, Token::Word(_)))
                {
                    scanned.bindings.push(Binding { name, at });
                }
            }
            Token::Word(_) | Token::Symbol(_) => {
                if depth == 0 && scanned.outside.is_none() {
                    scanned.outside = Some(at);
                }
            }
        }
    }
    drop(tokens);
    scanned.raw_string = raw_string.get();
    scanned.newer = lexer.newer;
    scanned
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const fn at(line: u32, column: u32) -> SourcePosition {
        SourcePosition { line, column }
    }

    #[test]
    fn it_finds_each_function_with_its_parameters_and_its_place() {
        let scanned = scan(
            "// a comment\nfn on_join(first) {\n  walk(1, 2);\n}\n\n  private fn walk(a, b) { a + b }\nfn empty() {}\n",
        );

        assert_eq!(
            scanned.definitions,
            [
                Definition {
                    name: "on_join".to_owned(),
                    parameters: vec!["first".to_owned()],
                    at: at(2, 1),
                },
                Definition {
                    name: "walk".to_owned(),
                    parameters: vec!["a".to_owned(), "b".to_owned()],
                    at: at(6, 11),
                },
                Definition {
                    name: "empty".to_owned(),
                    parameters: Vec::new(),
                    at: at(7, 1),
                },
            ]
        );
        assert_eq!(scanned.outside, None);
        assert_eq!(scanned.bindings.len(), 0);
    }

    #[test]
    fn a_definition_is_found_by_its_name_and_its_parameters() {
        let scanned = scan("fn f(a) {}\nfn f() {}\nfn g(a, b) {}\n");

        assert_eq!(scanned.definition("f", 0), Some(at(2, 1)));
        assert_eq!(scanned.definition("f", 1), Some(at(1, 1)));
        // No such count: the first of that name.
        assert_eq!(scanned.definition("f", 5), Some(at(1, 1)));
        assert_eq!(scanned.definition("g", 2), Some(at(3, 1)));
        assert_eq!(scanned.definition("h", 0), None);
    }

    #[rstest]
    #[case("let x = 1;\nfn f() {}", at(1, 1))]
    #[case("fn f() {}\n\n   f();", at(3, 4))]
    #[case("fn f() { \"}\" }\nloop { }", at(2, 1))]
    #[case("fn f() { '}' }\n1 + 1;", at(2, 1))]
    #[case("fn f() { `}${ \"}\" }` }\nx;", at(2, 1))]
    #[case("fn f() { /* } */ }\n// }\nx;", at(3, 1))]
    #[case("/* a /* nested */ comment */ x;", at(1, 30))]
    #[case("fn f() { '\\'' }\n;", at(2, 1))]
    #[case("fn f() { \"a \\\" } b\" }\n42", at(2, 1))]
    #[case("fn f() { `a `` } b` }\n42", at(2, 1))]
    #[case("fn f() { `${ #{ a: `}` } }` }\n42", at(2, 1))]
    fn it_finds_the_first_thing_outside_every_function(
        #[case] source: &str,
        #[case] expected: SourcePosition,
    ) {
        assert_eq!(scan(source).outside, Some(expected), "{source}");
    }

    #[rstest]
    #[case("")]
    #[case("// only a comment")]
    #[case("fn f() { if true { 1 } else { 2 } }\nfn g() { #{ a: 1 } }")]
    #[case("fn f() { \"fn g() {\" }")]
    #[case("/// docs\nfn f() { let s = `}}}`; s }")]
    fn a_script_of_functions_only_has_nothing_outside(#[case] source: &str) {
        assert_eq!(scan(source).outside, None, "{source}");
    }

    #[test]
    fn it_finds_the_variables_for_and_catch_name() {
        let scanned = scan(
            "fn f() {\n  for item in list { }\n  for (value, index) in list { }\n  try { } catch (problem) { }\n  try { } catch { }\n}",
        );

        assert_eq!(
            scanned
                .bindings
                .iter()
                .map(|binding| (binding.name.as_str(), binding.at))
                .collect::<Vec<_>>(),
            [
                ("item", at(2, 7)),
                ("value", at(3, 8)),
                ("index", at(3, 15)),
                ("problem", at(4, 18)),
            ]
        );
    }

    #[test]
    fn a_for_in_a_comment_or_a_text_names_nothing() {
        let scanned = scan(
            "fn f() {\n  // for keys in list\n  let s = \"for keys in\";\n  let t = `catch (state)`;\n}",
        );

        assert_eq!(scanned.bindings.len(), 0);
    }

    #[test]
    fn text_that_ends_early_ends_the_scan_without_a_wrong_place() {
        for source in [
            "fn f() { \"never closed",
            "fn f() { /* never closed",
            "fn f(",
            "fn",
            "fn f() { `${ never",
        ] {
            let scanned = scan(source);

            assert_eq!(scanned.outside, None, "{source}");
        }
    }

    #[test]
    fn texts_nested_deeper_than_rhai_allows_end_the_scan() {
        let deep = format!(
            "fn f() {{ {}1{} }}
x;",
            "`${".repeat(5_000),
            "}`".repeat(5_000)
        );

        // No overflow of the stack, and no place made up.
        assert_eq!(scan(&deep).outside, None);
    }

    /// The first raw string's place, wherever it is: in code or in a
    /// text's `${ … }`; never a `#{`, nor a `#` in a text or a comment
    /// (ADR-0040's amendment, A6).
    #[rstest]
    #[case(r##"fn f() { #"a"# }"##, Some(at(1, 10)))]
    #[case(r###"fn f() { `${ ##"}"## }` }"###, Some(at(1, 14)))]
    #[case(r##"fn f() { #{ a: "#\"" } } // #"x"#"##, None)]
    #[case(r##"fn f() { /* #"x"# */ }"##, None)]
    fn it_finds_the_first_raw_string(
        #[case] source: &str,
        #[case] expected: Option<SourcePosition>,
    ) {
        assert_eq!(scan(source).raw_string, expected);
    }

    /// A raw string is skipped whole, its quotes and braces with it, to the
    /// quote and as many `#` that close it.
    #[test]
    fn a_raw_string_is_skipped_whole() {
        let scanned = scan("fn f() { ##\"}\"# fn g() {\"## }\nx;");

        assert_eq!(scanned.outside, Some(at(2, 1)));
        assert_eq!(scanned.definitions.len(), 1);
    }

    #[test]
    fn a_column_counts_characters_not_bytes() {
        let scanned = scan("fn f() { \"\u{e4}\u{f6}\u{fc}\" } x;");

        assert_eq!(scanned.outside, Some(at(1, 18)));
    }
}
