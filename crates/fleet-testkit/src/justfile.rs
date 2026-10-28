//! A strict reader of the repository's justfile, for the tests that load a
//! committed config with exactly what a recipe passes it (ADR-0020, S72,
//! R124, R128): the configs of the definition of done's checks are only
//! ever run by the user, so their tests read each recipe's exports instead
//! of repeating them.
//!
//! It reads one recipe: its header's parameters, its dependencies and its
//! body's lines. A parameter is a name, `$` when just exports it, with an
//! optional default, which is either a quoted literal (`"info"`) or a path
//! in parentheses, built from `justfile_directory()`, an earlier parameter's
//! name and quoted segments joined by `/`. Anything else is refused, never
//! guessed: a variable, any other function, a backtick, an unquoted word, a
//! variadic parameter, or a dependency with arguments.
//! [`Recipe::exports_under`] then gives every exported value, a path under a
//! prefix moved to another root, so a test can run the recipe's exports
//! with temporary paths in place of the real folder's.
//!
//! What reaches every recipe's environment beside its own parameters,
//! [`environment`] lists: the justfile's `set` settings (`set export` and
//! `set dotenv-load` would export more) and its top-level `export` lines,
//! so a test can check that nothing else sets a config key (PR #78's
//! review).
//!
//! The scripts' Node tests read the same recipes through
//! `scripts/justfile.mjs`.

use core::fmt;
use std::path::Path;

/// One recipe of a justfile, as [`recipe`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    name: String,
    parameters: Vec<Parameter>,
    dependencies: Vec<String>,
    body: Vec<String>,
}

/// One parameter of a recipe's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    name: String,
    exported: bool,
    default: Option<Value>,
}

/// A parameter's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A quoted literal, as written between its quotes.
    Text(String),
    /// A path: its segments from the justfile's directory.
    Path(Vec<String>),
}

/// Why a recipe can't be read, or its exports can't be moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JustfileError {
    /// The justfile has no recipe of that name.
    NoRecipe {
        /// The recipe asked for.
        recipe: String,
    },
    /// The recipe's header holds something this reader doesn't read, at
    /// that byte of its line.
    Unreadable {
        /// The recipe.
        recipe: String,
        /// The byte of the header line where reading stopped.
        column: usize,
    },
    /// An exported path isn't under the prefix it was to be moved from.
    OutsidePrefix {
        /// The recipe.
        recipe: String,
        /// The exported parameter, without its `$`.
        parameter: String,
    },
    /// An exported parameter has no default, so the recipe alone doesn't
    /// say what it exports.
    NoDefault {
        /// The recipe.
        recipe: String,
        /// The exported parameter, without its `$`.
        parameter: String,
    },
    /// A top-level `set` or `export` line holds something this reader
    /// doesn't read.
    UnreadableLine {
        /// The line's number, from 1.
        line: usize,
    },
}

/// What the justfile gives every recipe's environment beside the recipe's
/// own parameters, as [`environment`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    settings: Vec<String>,
    exports: Vec<String>,
}

impl Environment {
    /// The names of the justfile's `set` settings, in order.
    #[must_use]
    pub fn settings(&self) -> &[String] {
        &self.settings
    }

    /// The names of its top-level `export NAME := …` variables, in order.
    #[must_use]
    pub fn exports(&self) -> &[String] {
        &self.exports
    }
}

/// The settings and top-level exports of the justfile `text`.
///
/// # Errors
/// [`JustfileError::UnreadableLine`] for a top-level `set` or `export` line
/// without a name, or an `export` without its `:=`.
pub fn environment(text: &str) -> Result<Environment, JustfileError> {
    let mut read = Environment {
        settings: Vec::new(),
        exports: Vec::new(),
    };
    for (index, line) in text.lines().enumerate() {
        let unreadable = || JustfileError::UnreadableLine { line: index + 1 };
        let (words, export) = match (keyword(line, "set"), keyword(line, "export")) {
            (Some(words), _) => (words, false),
            (None, Some(words)) => (words, true),
            (None, None) => continue,
        };
        let mut header = Header {
            line: words,
            recipe: "",
            at: 0,
        };
        let name = header.identifier().ok_or_else(unreadable)?;
        if export {
            if !header.rest().trim_start().starts_with(":=") {
                return Err(unreadable());
            }
            read.exports.push(name);
        } else {
            read.settings.push(name);
        }
    }
    Ok(read)
}

/// What follows `word` and a space at the start of a top-level `line`.
fn keyword<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    line.strip_prefix(word)?
        .strip_prefix(' ')
        .map(str::trim_start)
        .or_else(|| (line == word).then_some(""))
}

impl fmt::Display for JustfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRecipe { recipe } => write!(f, "the justfile has no recipe {recipe}"),
            Self::Unreadable { recipe, column } => write!(
                f,
                "recipe {recipe}'s header can't be read at byte {column}: only quoted literals and justfile_directory() paths are"
            ),
            Self::OutsidePrefix { recipe, parameter } => write!(
                f,
                "recipe {recipe} exports {parameter} outside the prefix to be moved"
            ),
            Self::NoDefault { recipe, parameter } => {
                write!(f, "recipe {recipe} exports {parameter} without a default")
            }
            Self::UnreadableLine { line } => write!(
                f,
                "the justfile's line {line} can't be read: a setting or an export needs a name, an export its :="
            ),
        }
    }
}

impl std::error::Error for JustfileError {}

impl Recipe {
    /// The recipe's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its parameters, in the header's order.
    #[must_use]
    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }

    /// The parameter `name` (without a `$`), if the header has it.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&Parameter> {
        self.parameters
            .iter()
            .find(|parameter| parameter.name == name)
    }

    /// The recipes it depends on, in order.
    #[must_use]
    pub fn dependencies(&self) -> &[String] {
        &self.dependencies
    }

    /// Its body's lines, without their indentation.
    #[must_use]
    pub fn body(&self) -> &[String] {
        &self.body
    }

    /// Every exported parameter's name and value: a literal as it is, a path
    /// starting with `prefix`'s segments as `root` joined with the rest.
    ///
    /// # Errors
    /// [`JustfileError::OutsidePrefix`] for an exported path anywhere else,
    /// and [`JustfileError::NoDefault`] for an exported parameter with no
    /// default.
    pub fn exports_under(
        &self,
        prefix: &[&str],
        root: &Path,
    ) -> Result<Vec<(String, String)>, JustfileError> {
        self.parameters
            .iter()
            .filter(|parameter| parameter.exported)
            .map(|parameter| {
                let value = match &parameter.default {
                    None => return Err(self.error_for(parameter, false)),
                    Some(Value::Text(text)) => text.clone(),
                    Some(Value::Path(segments)) => segments
                        .get(..prefix.len())
                        .filter(|head| head.iter().zip(prefix).all(|(a, b)| a == b))
                        .and_then(|_| segments.get(prefix.len()..))
                        .ok_or_else(|| self.error_for(parameter, true))?
                        .iter()
                        .fold(root.to_path_buf(), |path, segment| path.join(segment))
                        .display()
                        .to_string(),
                };
                Ok((parameter.name.clone(), value))
            })
            .collect()
    }

    /// The error for `parameter`: a path outside the prefix, or no default.
    fn error_for(&self, parameter: &Parameter, has_default: bool) -> JustfileError {
        let (recipe, parameter) = (self.name.clone(), parameter.name.clone());
        if has_default {
            JustfileError::OutsidePrefix { recipe, parameter }
        } else {
            JustfileError::NoDefault { recipe, parameter }
        }
    }
}

impl Parameter {
    /// Its name, without the `$`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether just exports it to the recipe's commands (`$`).
    #[must_use]
    pub const fn exported(&self) -> bool {
        self.exported
    }

    /// Its default, if it has one.
    #[must_use]
    pub const fn default(&self) -> Option<&Value> {
        self.default.as_ref()
    }
}

/// The recipe `name` of the justfile `text`.
///
/// # Errors
/// [`JustfileError::NoRecipe`] when there's none, and
/// [`JustfileError::Unreadable`] when its header holds anything but what the
/// [module docs](self) list.
pub fn recipe(text: &str, name: &str) -> Result<Recipe, JustfileError> {
    let mut lines = text.lines();
    let line = lines
        .by_ref()
        .find(|line| is_header_of(line, name))
        .ok_or_else(|| JustfileError::NoRecipe {
            recipe: name.to_owned(),
        })?;
    let mut header = Header {
        line,
        recipe: name,
        at: name.len(),
    };
    let parameters = header.parameters()?;
    let dependencies = header.dependencies()?;
    let body = lines
        .take_while(|line| line.starts_with("    "))
        .map(|line| line.trim().to_owned())
        .collect();
    Ok(Recipe {
        name: name.to_owned(),
        parameters,
        dependencies,
        body,
    })
}

/// Whether `line` is the header of the recipe `name`: its whole name, then
/// its parameters or its colon.
fn is_header_of(line: &str, name: &str) -> bool {
    line.strip_prefix(name)
        .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with(':'))
}

/// A recipe's header line, read from `at` on.
struct Header<'a> {
    line: &'a str,
    recipe: &'a str,
    at: usize,
}

impl<'a> Header<'a> {
    /// The header can't be read at `column`.
    fn refuse(&self, column: usize) -> JustfileError {
        JustfileError::Unreadable {
            recipe: self.recipe.to_owned(),
            column,
        }
    }

    /// What's left of the line.
    fn rest(&self) -> &'a str {
        self.line.get(self.at..).unwrap_or_default()
    }

    fn skip_spaces(&mut self) {
        let rest = self.rest();
        self.at += rest.len() - rest.trim_start_matches(' ').len();
    }

    /// Whether the next parameter or dependency may start here: a space,
    /// the line's end, or (after a parameter) the colon.
    fn at_separator(&self, colon: bool) -> bool {
        let rest = self.rest();
        rest.is_empty() || rest.starts_with(' ') || (colon && rest.starts_with(':'))
    }

    /// The parameters up to the header's colon, which it reads too.
    fn parameters(&mut self) -> Result<Vec<Parameter>, JustfileError> {
        let mut parameters: Vec<Parameter> = Vec::new();
        loop {
            self.skip_spaces();
            if self.rest().starts_with(':') {
                self.at += 1;
                return Ok(parameters);
            }
            if self.rest().is_empty() {
                return Err(self.refuse(self.at));
            }
            let exported = self.rest().starts_with('$');
            if exported {
                self.at += 1;
            }
            let name = self.identifier().ok_or_else(|| self.refuse(self.at))?;
            let default = if self.rest().starts_with('=') {
                self.at += 1;
                Some(self.value(&parameters)?)
            } else {
                None
            };
            if !self.at_separator(true) {
                return Err(self.refuse(self.at));
            }
            parameters.push(Parameter {
                name,
                exported,
                default,
            });
        }
    }

    /// The dependencies after the colon: plain names only.
    fn dependencies(&mut self) -> Result<Vec<String>, JustfileError> {
        let mut dependencies = Vec::new();
        loop {
            self.skip_spaces();
            if self.rest().is_empty() {
                return Ok(dependencies);
            }
            let name = self.identifier().ok_or_else(|| self.refuse(self.at))?;
            if !self.at_separator(false) {
                return Err(self.refuse(self.at));
            }
            dependencies.push(name);
        }
    }

    /// A name: a letter or `_`, then letters, digits, `_` and `-`.
    fn identifier(&mut self) -> Option<String> {
        let rest = self.rest();
        let first = rest.chars().next()?;
        if !(first.is_ascii_alphabetic() || first == '_') {
            return None;
        }
        let length = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(rest.len());
        let name = rest.get(..length)?.to_owned();
        self.at += length;
        Some(name)
    }

    /// A default: a quoted literal, or a path in parentheses.
    fn value(&mut self, earlier: &[Parameter]) -> Result<Value, JustfileError> {
        let start = self.at;
        let rest = self.rest();
        if rest.starts_with('"') {
            let text = quoted(rest).ok_or_else(|| self.refuse(start))?;
            self.at += text.len() + 2;
            return Ok(Value::Text(text.to_owned()));
        }
        if rest.starts_with('(') {
            let close = closing(rest).ok_or_else(|| self.refuse(start))?;
            let inner = rest.get(1..close).ok_or_else(|| self.refuse(start))?;
            let segments = self.path(inner, start + 1, earlier)?;
            self.at += close + 1;
            return Ok(Value::Path(segments));
        }
        Err(self.refuse(start))
    }

    /// A path's segments: `justfile_directory()` or an earlier path
    /// parameter first, then quoted segments, each between `/`s.
    fn path(
        &self,
        inner: &str,
        offset: usize,
        earlier: &[Parameter],
    ) -> Result<Vec<String>, JustfileError> {
        let mut terms = terms(inner, offset).into_iter();
        let (first_at, first) = terms.next().ok_or_else(|| self.refuse(offset))?;
        let mut segments = if first == "justfile_directory()" {
            Vec::new()
        } else if let Some(Value::Path(base)) = earlier
            .iter()
            .find(|parameter| parameter.name == first)
            .and_then(|parameter| parameter.default.as_ref())
        {
            base.clone()
        } else {
            return Err(self.refuse(first_at));
        };
        for (at, term) in terms {
            let segment = term
                .strip_prefix('"')
                .and_then(|term| term.strip_suffix('"'))
                .filter(|segment| is_segment(segment))
                .ok_or_else(|| self.refuse(at))?;
            segments.push(segment.to_owned());
        }
        Ok(segments)
    }
}

/// The text between `rest`'s opening quote and the next one, if it holds no
/// backslash and no brace.
fn quoted(rest: &str) -> Option<&str> {
    let inside = rest.get(1..)?;
    let text = inside.get(..inside.find('"')?)?;
    (!text.contains(['\\', '{', '}'])).then_some(text)
}

/// The byte of the parenthesis that closes `rest`'s first one, outside
/// quotes.
fn closing(rest: &str) -> Option<usize> {
    let mut depth = 0_usize;
    let mut quoted = false;
    for (at, c) in rest.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// `inner`'s terms between the `/`s outside quotes, each trimmed, with the
/// byte of the header line it starts at (`inner` starts at `offset`).
fn terms(inner: &str, offset: usize) -> Vec<(usize, &str)> {
    let mut terms = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut push = |from: usize, to: usize| {
        let piece = inner.get(from..to).unwrap_or_default();
        let leading = piece.len() - piece.trim_start().len();
        terms.push((offset + from + leading, piece.trim()));
    };
    for (at, c) in inner.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '/' if !quoted => {
                push(start, at);
                start = at + 1;
            }
            _ => {}
        }
    }
    push(start, inner.len());
    terms
}

/// Whether a path segment is a plain name: not empty, not `.` or `..`, and
/// without separators, quotes or braces.
fn is_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains(['/', '\\', '"', '{', '}'])
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const JUSTFILE: &str = r#"
set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

# A recipe with no parameters.
plain:
    echo one
    echo two

# Not part of plain: the body ended at the blank line.
plain-two:
    echo three

[no-exit-message]
paths dir=(justfile_directory() / "target" / "dod-run" / "agent") $AFKFLEET_A__FILE=(dir / "agent.crt") $AFKFLEET_A__ROOT=(justfile_directory() / "target" / "dod-run" / "afkfleet.db") $AFKFLEET_A__LOG__FILTER="info" username:
    node scripts/x.mjs "{{ dir }}"

after: plain _helper-two
    echo four

outside $AFKFLEET_A__DB=(justfile_directory() / "target" / "dev-server" / "afkfleet.db"):
    echo five

bare $AFKFLEET_A__NAME:
    echo six
"#;

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    fn path(segments: &[&str]) -> Value {
        Value::Path(
            segments
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect(),
        )
    }

    fn parameter(name: &str, exported: bool, default: Value) -> Parameter {
        Parameter {
            name: name.to_owned(),
            exported,
            default: Some(default),
        }
    }

    #[test]
    fn a_recipe_without_parameters_reads_with_its_body() {
        let read = recipe(JUSTFILE, "plain");

        assert_eq!(
            read,
            Ok(Recipe {
                name: "plain".to_owned(),
                parameters: Vec::new(),
                dependencies: Vec::new(),
                body: vec!["echo one".to_owned(), "echo two".to_owned()],
            })
        );
    }

    #[test]
    fn a_recipe_is_found_by_its_whole_name() {
        let read = recipe(JUSTFILE, "plain-two");

        assert_eq!(
            read.map(|read| read.body),
            Ok(vec!["echo three".to_owned()])
        );
    }

    #[test]
    fn exported_and_plain_parameters_read_with_literal_and_path_defaults() {
        let read = recipe(JUSTFILE, "paths").unwrap();

        assert_eq!(
            read.parameters(),
            [
                parameter("dir", false, path(&["target", "dod-run", "agent"])),
                parameter(
                    "AFKFLEET_A__FILE",
                    true,
                    path(&["target", "dod-run", "agent", "agent.crt"])
                ),
                parameter(
                    "AFKFLEET_A__ROOT",
                    true,
                    path(&["target", "dod-run", "afkfleet.db"])
                ),
                parameter("AFKFLEET_A__LOG__FILTER", true, text("info")),
                Parameter {
                    name: "username".to_owned(),
                    exported: false,
                    default: None,
                },
            ]
        );
        assert_eq!(read.body(), [r#"node scripts/x.mjs "{{ dir }}""#]);
        assert_eq!(
            read.parameter("AFKFLEET_A__LOG__FILTER")
                .and_then(Parameter::default),
            Some(&text("info"))
        );
    }

    #[test]
    fn dependencies_are_read_in_order() {
        let read = recipe(JUSTFILE, "after").unwrap();

        assert_eq!(read.dependencies(), ["plain", "_helper-two"]);
        assert_eq!(read.body(), ["echo four"]);
    }

    #[test]
    fn exports_under_moves_paths_under_the_prefix_and_keeps_literals() {
        let read = recipe(JUSTFILE, "paths").unwrap();
        let root = PathBuf::from("/tmp/jail");

        let exports = read.exports_under(&["target", "dod-run"], &root).unwrap();

        assert_eq!(
            exports,
            [
                (
                    "AFKFLEET_A__FILE".to_owned(),
                    root.join("agent").join("agent.crt").display().to_string()
                ),
                (
                    "AFKFLEET_A__ROOT".to_owned(),
                    root.join("afkfleet.db").display().to_string()
                ),
                ("AFKFLEET_A__LOG__FILTER".to_owned(), "info".to_owned()),
            ]
        );
    }

    #[test]
    fn an_exported_path_outside_the_prefix_is_an_error() {
        let read = recipe(JUSTFILE, "outside").unwrap();

        let exports = read.exports_under(&["target", "dod-run"], Path::new("/tmp/jail"));

        assert_eq!(
            exports,
            Err(JustfileError::OutsidePrefix {
                recipe: "outside".to_owned(),
                parameter: "AFKFLEET_A__DB".to_owned(),
            })
        );
    }

    #[test]
    fn an_export_without_a_default_is_an_error() {
        let read = recipe(JUSTFILE, "bare").unwrap();

        let exports = read.exports_under(&["target", "dod-run"], Path::new("/tmp/jail"));

        assert_eq!(
            exports,
            Err(JustfileError::NoDefault {
                recipe: "bare".to_owned(),
                parameter: "AFKFLEET_A__NAME".to_owned(),
            })
        );
    }

    #[test]
    fn a_missing_recipe_is_an_error() {
        let read = recipe(JUSTFILE, "plai");

        assert_eq!(
            read,
            Err(JustfileError::NoRecipe {
                recipe: "plai".to_owned()
            })
        );
    }

    /// Asserts that the recipe `r` with `header` is refused at `column`.
    fn refused_at(header: &str, column: usize) {
        let text = format!(
            "{header}
    echo
"
        );

        let read = recipe(&text, "r");

        assert_eq!(
            read,
            Err(JustfileError::Unreadable {
                recipe: "r".to_owned(),
                column,
            }),
            "{header}"
        );
    }

    #[test]
    fn a_variable_default_is_refused() {
        refused_at(r"r $X=sqlx_db:", 5);
    }

    #[test]
    fn another_function_is_refused() {
        refused_at(r#"r $X=(env("X") / "a"):"#, 6);
    }

    #[test]
    fn an_unknown_earlier_name_is_refused() {
        refused_at(r#"r $X=(nope / "a"):"#, 6);
    }

    #[test]
    fn a_path_starting_with_a_literal_is_refused() {
        refused_at(r#"r $X=("a" / "b"):"#, 6);
    }

    #[test]
    fn a_backtick_is_refused() {
        refused_at(r"r $X=`whoami`:", 5);
    }

    #[test]
    fn an_unclosed_quote_is_refused() {
        refused_at(r#"r $X="info:"#, 5);
    }

    #[test]
    fn an_unclosed_parenthesis_is_refused() {
        refused_at(r#"r $X=(justfile_directory() / "a":"#, 5);
    }

    #[test]
    fn a_backslash_is_refused() {
        refused_at(r#"r $X="a\b":"#, 5);
    }

    #[test]
    fn a_parent_segment_is_refused() {
        refused_at(r#"r $X=(justfile_directory() / ".."):"#, 29);
    }

    #[test]
    fn a_slash_in_a_segment_is_refused() {
        refused_at(r#"r $X=(justfile_directory() / "a/b"):"#, 29);
    }

    #[test]
    fn a_variadic_parameter_is_refused() {
        refused_at(r"r *args:", 2);
    }

    #[test]
    fn a_dependency_with_arguments_is_refused() {
        refused_at(r#"r: (other "x")"#, 3);
    }

    #[test]
    fn a_header_without_its_colon_is_refused() {
        refused_at(r#"r $X="info""#, 11);
    }

    #[test]
    fn an_error_names_the_recipe_and_the_parameter_never_a_value() {
        let shown = JustfileError::OutsidePrefix {
            recipe: "dod-server".to_owned(),
            parameter: "AFKFLEET_SERVER__DATABASE__PATH".to_owned(),
        }
        .to_string();

        assert_eq!(
            shown,
            "recipe dod-server exports AFKFLEET_SERVER__DATABASE__PATH outside the prefix to be moved"
        );
    }

    /// A justfile with settings, a top-level export, a plain variable and a
    /// recipe whose body sets and exports things of its own.
    const GLOBALS: &str = r#"
# set export would export every parameter
set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]
set dotenv-load

export RUSTDOCFLAGS := "-D warnings"
stable := "1.99.0"

r:
    export X=1
    set Y=2
"#;

    #[test]
    fn the_environment_lists_every_setting_and_top_level_export() {
        let read = environment(GLOBALS).unwrap();

        assert_eq!(read.settings(), ["windows-shell", "dotenv-load"]);
        assert_eq!(read.exports(), ["RUSTDOCFLAGS"]);
    }

    #[test]
    fn a_setting_without_a_name_is_refused_by_its_line() {
        let read = environment("plain:\n    echo\n\nset\n");

        assert_eq!(read, Err(JustfileError::UnreadableLine { line: 4 }));
    }

    #[test]
    fn an_export_without_its_assignment_is_refused_by_its_line() {
        let read = environment("export RUSTDOCFLAGS\n");

        assert_eq!(read, Err(JustfileError::UnreadableLine { line: 1 }));
    }
}
