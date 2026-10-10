//! Why a config can't be loaded, with the key path of every problem.
//!
//! figment's own errors are converted at once and never kept: their text
//! shows a profile prefix (`default.runtime…`) and, for environment
//! variables, a key that isn't the variable's name. A TOML syntax error keeps
//! only its first line (the position), because the rest quotes the file.

use core::fmt;
use std::path::PathBuf;

use figment::error::Kind;
use figment::{Metadata, Source};

/// Why a binary's config can't be loaded: [`extract`](super::extract) can't
/// find or read it, or the binary's validation found problems of kind `K`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError<K> {
    /// There's no file at the given path.
    #[error("no config file at {}", path.display())]
    NotFound {
        /// The path that was given.
        path: PathBuf,
    },
    /// Something in the file or the environment can't be read. Only the
    /// first such error is reported.
    #[error("the config can't be read: {0}")]
    Parse(Box<ParseError>),
    /// The config was read, but validation found problems: all of them.
    #[error("the config is invalid:\n{0}")]
    Invalid(Problems<K>),
}

/// One part of a [`KeyPath`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeySegment {
    /// A key in a table, e.g. `runtime`.
    Key(String),
    /// A position in a list, e.g. the `1` in `bots[1]`.
    Index(usize),
}

/// Where a key sits in the config, written `standalone.bots[1].username`.
/// Paths sort part by part, positions by number.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyPath(Vec<KeySegment>);

impl KeyPath {
    /// The empty path: the config's top level.
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// This path with a key appended.
    #[must_use]
    pub fn key(&self, key: &str) -> Self {
        self.with(KeySegment::Key(key.to_owned()))
    }

    /// This path with a list position appended.
    #[must_use]
    pub fn index(&self, index: usize) -> Self {
        self.with(KeySegment::Index(index))
    }

    /// The path's parts.
    #[must_use]
    pub fn segments(&self) -> &[KeySegment] {
        &self.0
    }

    /// Whether this path is `prefix` or lies under it.
    #[must_use]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.0.starts_with(&prefix.0)
    }

    fn with(&self, segment: KeySegment) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment);
        Self(segments)
    }
}

impl fmt::Display for KeyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, segment) in self.0.iter().enumerate() {
            match segment {
                KeySegment::Key(key) if position == 0 => f.write_str(key)?,
                KeySegment::Key(key) => write!(f, ".{key}")?,
                KeySegment::Index(index) => write!(f, "[{index}]")?,
            }
        }
        Ok(())
    }
}

/// The first thing in the file or the environment that couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    key: KeyPath,
    source: ParseSource,
    problem: ParseProblem,
}

impl ParseError {
    /// Converts figment's error. `env_name` is the name figment gives the
    /// environment provider, which tells its errors apart from the file's;
    /// `env_prefix` is the prefix of the binary's variables.
    pub(crate) fn from_figment(error: &figment::Error, env_name: &str, env_prefix: &str) -> Self {
        let mut key =
            error
                .path
                .iter()
                .fold(KeyPath::root(), |path, segment| match segment.parse() {
                    Ok(index) => path.index(index),
                    Err(_) => path.key(segment),
                });
        let problem = match &error.kind {
            Kind::Message(message) => ParseProblem::Other {
                first_line: message.lines().next().unwrap_or_default().to_owned(),
            },
            Kind::InvalidType(found, expected) => ParseProblem::WrongType {
                expected: expected.clone(),
                found: found.to_string(),
            },
            Kind::InvalidValue(found, expected) => ParseProblem::InvalidValue {
                expected: expected.clone(),
                found: found.to_string(),
            },
            Kind::InvalidLength(len, expected) => ParseProblem::WrongLength {
                expected: expected.clone(),
                len: *len,
            },
            Kind::UnknownVariant(_, expected) => ParseProblem::UnknownVariant { expected },
            Kind::UnknownField(_, expected) => ParseProblem::UnknownKey { expected },
            Kind::MissingField(name) => {
                key = key.key(name);
                ParseProblem::MissingKey
            }
            Kind::DuplicateField(_) => ParseProblem::DuplicateKey,
            Kind::ISizeOutOfRange(_) | Kind::USizeOutOfRange(_) => ParseProblem::OutOfRange,
            Kind::Unsupported(_) | Kind::UnsupportedKey(..) => ParseProblem::Unsupported,
        };
        let source = match &error.metadata {
            Some(metadata) if metadata.name == env_name => ParseSource::Env {
                variable: env_variable(env_prefix, &key),
            },
            Some(Metadata {
                source: Some(Source::File(path)),
                ..
            }) => ParseSource::File { path: path.clone() },
            _ => ParseSource::Unknown,
        };
        Self {
            key,
            source,
            problem,
        }
    }

    /// The key that couldn't be read; empty for the whole file (bad TOML).
    #[must_use]
    pub fn key(&self) -> &KeyPath {
        &self.key
    }

    /// Where the key came from.
    #[must_use]
    pub fn source(&self) -> &ParseSource {
        &self.source
    }

    /// What's wrong with it.
    #[must_use]
    pub fn problem(&self) -> &ParseProblem {
        &self.problem
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.key.segments().is_empty() {
            write!(f, "{}", self.problem)?;
        } else {
            write!(f, "{}: {}", self.key, self.problem)?;
        }
        match &self.source {
            ParseSource::File { path } => write!(f, " (in the file {})", path.display()),
            ParseSource::Env { variable } => {
                write!(f, " (in the environment variable {variable})")
            }
            ParseSource::Unknown => Ok(()),
        }
    }
}

/// The environment variable that sets `key`: the binary's prefix plus the
/// key's parts in capitals, joined by `__`.
fn env_variable(prefix: &str, key: &KeyPath) -> String {
    let parts: Vec<String> = key
        .segments()
        .iter()
        .map(|segment| match segment {
            KeySegment::Key(key) => key.to_ascii_uppercase(),
            KeySegment::Index(index) => index.to_string(),
        })
        .collect();
    format!("{prefix}{}", parts.join("__"))
}

/// Where a key that couldn't be read came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseSource {
    /// The config file.
    File {
        /// The file's path.
        path: PathBuf,
    },
    /// An environment variable with the binary's prefix.
    Env {
        /// The variable's name, rebuilt from the key path.
        variable: String,
    },
    /// Neither is known, e.g. for a key that's missing everywhere.
    Unknown,
}

/// What's wrong with a key that couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseProblem {
    /// The key isn't one the binary knows.
    #[error("unknown key{}", Expected(expected))]
    UnknownKey {
        /// The keys allowed in its table.
        expected: &'static [&'static str],
    },
    /// A name that isn't one of the allowed ones.
    #[error("unknown name{}", Expected(expected))]
    UnknownVariant {
        /// The allowed names.
        expected: &'static [&'static str],
    },
    /// A required key is missing.
    #[error("missing")]
    MissingKey,
    /// The value has the wrong type, e.g. text where a number belongs.
    #[error("expected {expected}, found {found}")]
    WrongType {
        /// What the key takes.
        expected: String,
        /// What it got.
        found: String,
    },
    /// The value has the right type but can't be used.
    #[error("invalid value {found}, expected {expected}")]
    InvalidValue {
        /// What the key takes.
        expected: String,
        /// What it got.
        found: String,
    },
    /// A list or table has the wrong number of entries.
    #[error("{len} entries, expected {expected}")]
    WrongLength {
        /// How many it takes.
        expected: String,
        /// How many it has.
        len: usize,
    },
    /// The key is set twice.
    #[error("set more than once")]
    DuplicateKey,
    /// A number too large or too small for any integer type.
    #[error("number out of range")]
    OutOfRange,
    /// A value of a kind the config never takes.
    #[error("unsupported value")]
    Unsupported,
    /// Any other error: the first line of its message, e.g. the position of
    /// a TOML syntax error.
    #[error("{first_line}")]
    Other {
        /// The first line of the message.
        first_line: String,
    },
}

/// `; expected one of: a, b`, or nothing for an empty list.
struct Expected(&'static [&'static str]);

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, name) in self.0.iter().enumerate() {
            let separator = if position == 0 {
                "; expected one of: "
            } else {
                ", "
            };
            write!(f, "{separator}{name}")?;
        }
        Ok(())
    }
}

/// Every problem validation found, sorted by key. `K` is the binary's kind
/// of problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problems<K>(Vec<Problem<K>>);

// Not derived: the derive would require `K: Default`.
impl<K> Default for Problems<K> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<K> Problems<K> {
    /// The problems, sorted by key; problems at the same key keep the order
    /// they were found in.
    pub fn iter(&self) -> impl Iterator<Item = &Problem<K>> {
        self.0.iter()
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether any problem is at `prefix` or under it.
    #[must_use]
    pub fn any_under(&self, prefix: &KeyPath) -> bool {
        self.0.iter().any(|problem| problem.key.starts_with(prefix))
    }

    /// Adds a problem at `key`.
    pub fn push(&mut self, key: KeyPath, kind: K) {
        self.0.push(Problem { key, kind });
    }

    /// The problems sorted by key; problems at the same key keep the order
    /// they were found in.
    #[must_use]
    pub fn sorted(mut self) -> Self {
        self.0.sort_by(|a, b| a.key.cmp(&b.key));
        self
    }
}

impl<K: fmt::Display> fmt::Display for Problems<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, problem) in self.0.iter().enumerate() {
            if position > 0 {
                f.write_str("\n")?;
            }
            write!(f, "  - {problem}")?;
        }
        Ok(())
    }
}

/// One problem, at one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem<K> {
    key: KeyPath,
    kind: K,
}

impl<K> Problem<K> {
    /// The key it's about.
    #[must_use]
    pub fn key(&self) -> &KeyPath {
        &self.key
    }

    /// What's wrong.
    #[must_use]
    pub fn kind(&self) -> &K {
        &self.kind
    }
}

impl<K: fmt::Display> fmt::Display for Problem<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.key.segments().is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.key, self.kind)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::error::Actual;
    use rstest::rstest;
    use std::path::Path;

    const PREFIX: &str = "AFKFLEET_AGENT__";
    const ENV_NAME: &str = "`AFKFLEET_AGENT__` environment variable(s)";

    fn figment_error(kind: Kind, path: &[&str], metadata: Option<Metadata>) -> figment::Error {
        let mut error = figment::Error::from(kind);
        error.path = path.iter().map(|key| (*key).to_owned()).collect();
        error.metadata = metadata;
        error
    }

    fn file() -> Metadata {
        Metadata::from("TOML file", Path::new("agent.toml"))
    }

    fn env() -> Metadata {
        Metadata::named(ENV_NAME)
    }

    fn convert(kind: Kind, path: &[&str], metadata: Option<Metadata>) -> ParseError {
        ParseError::from_figment(&figment_error(kind, path, metadata), ENV_NAME, PREFIX)
    }

    #[test]
    fn an_unknown_key_in_the_file_names_the_key_and_the_file() {
        let error = convert(
            Kind::UnknownField("max_botz".into(), &["max_bots", "watchdog_timeout_secs"]),
            &["runtime", "max_botz"],
            Some(file()),
        );

        assert_eq!(error.key().to_string(), "runtime.max_botz");
        assert_eq!(
            error.source(),
            &ParseSource::File {
                path: PathBuf::from("agent.toml")
            }
        );
        assert_eq!(
            error.problem(),
            &ParseProblem::UnknownKey {
                expected: &["max_bots", "watchdog_timeout_secs"]
            }
        );
        assert_eq!(
            error.to_string(),
            "runtime.max_botz: unknown key; expected one of: max_bots, watchdog_timeout_secs \
             (in the file agent.toml)"
        );
    }

    #[test]
    fn an_unknown_key_from_the_environment_names_the_variable() {
        let error = convert(
            Kind::UnknownField("maxbots".into(), &["max_bots"]),
            &["runtime", "maxbots"],
            Some(env()),
        );

        assert_eq!(
            error.source(),
            &ParseSource::Env {
                variable: "AFKFLEET_AGENT__RUNTIME__MAXBOTS".to_owned()
            }
        );
        assert_eq!(
            error.to_string(),
            "runtime.maxbots: unknown key; expected one of: max_bots \
             (in the environment variable AFKFLEET_AGENT__RUNTIME__MAXBOTS)"
        );
    }

    #[test]
    fn list_positions_become_indexes() {
        let error = convert(
            Kind::UnknownField("usernme".into(), &[]),
            &["standalone", "bots", "1", "usernme"],
            Some(file()),
        );

        assert_eq!(error.key().to_string(), "standalone.bots[1].usernme");
        assert_eq!(error.problem(), &ParseProblem::UnknownKey { expected: &[] });
        assert_eq!(error.problem().to_string(), "unknown key");
    }

    #[test]
    fn a_missing_key_is_appended_to_its_table() {
        let error = convert(
            Kind::MissingField("username".into()),
            &["standalone", "bots", "0"],
            None,
        );

        assert_eq!(error.key().to_string(), "standalone.bots[0].username");
        assert_eq!(error.source(), &ParseSource::Unknown);
        assert_eq!(error.problem(), &ParseProblem::MissingKey);
        assert_eq!(error.to_string(), "standalone.bots[0].username: missing");
    }

    #[test]
    fn an_unknown_provider_has_no_source() {
        let error = convert(
            Kind::DuplicateField("name"),
            &[],
            Some(Metadata::named("something else")),
        );

        assert_eq!(error.source(), &ParseSource::Unknown);
        assert_eq!(error.to_string(), "set more than once");
    }

    #[rstest]
    #[case::wrong_type(
        Kind::InvalidType(Actual::Str("many".into()), "u64".into()),
        ParseProblem::WrongType { expected: "u64".into(), found: "string \"many\"".into() },
        "expected u64, found string \"many\""
    )]
    #[case::invalid_value(
        Kind::InvalidValue(Actual::Signed(-1), "u64".into()),
        ParseProblem::InvalidValue { expected: "u64".into(), found: "signed int `-1`".into() },
        "invalid value signed int `-1`, expected u64"
    )]
    #[case::wrong_length(
        Kind::InvalidLength(3, "2 elements".into()),
        ParseProblem::WrongLength { expected: "2 elements".into(), len: 3 },
        "3 entries, expected 2 elements"
    )]
    #[case::unknown_variant(
        Kind::UnknownVariant("sometimes".into(), &["json", "pretty"]),
        ParseProblem::UnknownVariant { expected: &["json", "pretty"] },
        "unknown name; expected one of: json, pretty"
    )]
    #[case::duplicate(
        Kind::DuplicateField("name"),
        ParseProblem::DuplicateKey,
        "set more than once"
    )]
    #[case::isize_range(Kind::ISizeOutOfRange(-1), ParseProblem::OutOfRange, "number out of range")]
    #[case::usize_range(
        Kind::USizeOutOfRange(1),
        ParseProblem::OutOfRange,
        "number out of range"
    )]
    #[case::unsupported(
        Kind::Unsupported(Actual::Unit),
        ParseProblem::Unsupported,
        "unsupported value"
    )]
    #[case::unsupported_key(
        Kind::UnsupportedKey(Actual::Unit, "a string".into()),
        ParseProblem::Unsupported,
        "unsupported value"
    )]
    #[case::message_keeps_its_first_line(
        Kind::Message("TOML parse error at line 3, column 7\n  |\n3 | name = \"marker\"\n".into()),
        ParseProblem::Other { first_line: "TOML parse error at line 3, column 7".into() },
        "TOML parse error at line 3, column 7"
    )]
    fn every_kind_converts(
        #[case] kind: Kind,
        #[case] problem: ParseProblem,
        #[case] message: &str,
    ) {
        let error = convert(kind, &[], Some(file()));

        assert_eq!(error.problem(), &problem);
        assert_eq!(error.problem().to_string(), message);
        assert!(!error.to_string().contains("marker"));
    }

    #[test]
    fn key_paths_sort_positions_by_number() {
        let bots = KeyPath::root().key("standalone").key("bots");
        let mut keys = [
            bots.index(10).key("username"),
            KeyPath::root().key("name"),
            bots.index(2).key("username"),
            bots.index(2).key("mode"),
        ];
        keys.sort();

        let shown: Vec<String> = keys.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "name",
                "standalone.bots[2].mode",
                "standalone.bots[2].username",
                "standalone.bots[10].username",
            ]
        );
        assert!(bots.index(2).key("mode").starts_with(&bots));
        assert!(!KeyPath::root().key("name").starts_with(&bots));
    }
}
