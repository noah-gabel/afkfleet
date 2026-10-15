//! `afkfleet-server user create-owner` (Plan.md P7.5, §7.2, ADR-0016): the
//! bootstrap of the first, and only, Owner.
//!
//! - **The Owner check comes first.** Before any password is typed or read,
//!   the command asks the database whether an Owner exists and stops if one
//!   does; the unique index still decides at insert.
//! - **At a terminal,** the password is typed twice, hidden (rpassword). A
//!   mismatch, a password the length rule refuses, or one the strength check
//!   calls weak (with zxcvbn's hints) asks again, at most [`ROUNDS`] times.
//! - **`--password-stdin`** reads one password from stdin, at most
//!   [`STDIN_MAX_BYTES`], removes one trailing line break and runs the same
//!   checks once, with no retry. It's refused when stdin is a terminal, where
//!   the typed password would show. Scripts and P12.7's smoke test use it.
//! - **No other way in:** no password flag and no environment variable,
//!   which `ps`, shell history and `docker inspect` would show.
//! - **Reads never block the runtime.** The prompt and the stdin read run in
//!   `spawn_blocking`; each moves its source onto the blocking thread and
//!   gets it back.
//!
//! It opens the database like `migrate` (pending migrations run), never
//! loads the vault's keys, and builds the real clock and randomness itself,
//! like `serve`. It logs per `[log]`: "the Owner is created" at `info`, and a
//! failure of the database or the password service at `error`. The outcome
//! is one plain line: on stdout when the Owner is created, on stderr
//! otherwise.

use std::io::{self, Read, Write};
use std::sync::Arc;

use fleet_core::auth::NewPassword;
use fleet_core::system::SecureRandom;
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, SecretString};
use tracing::{error, info};
use zeroize::{Zeroize as _, Zeroizing};

use super::DATABASE_CLOSE;
use crate::app::users::{CreateOwnerError, UserService};
use crate::config::ServerConfig;
use crate::http::error::chain;
use crate::infra::crypto::passwords::Argon2Passwords;
use crate::infra::sqlite::{self, DatabaseOptions};
use crate::infra::system::{OsRandom, SystemClock};
use crate::ports::passwords::HashError;

/// The exit codes of `afkfleet-server user create-owner`, shown in its help.
pub const CREATE_OWNER_EXIT_CODES: &str = "\
Exit codes:
  0  the Owner is created
  1  an Owner already exists, the username is invalid or taken, no password
     could be read, the passwords were refused, or the config, logging,
     database or password service failed
  2  usage error";

/// How many times the prompt asks before giving up.
pub const ROUNDS: usize = 3;

/// The most bytes `--password-stdin` reads.
pub const STDIN_MAX_BYTES: usize = 1024;

/// What the prompt says when it can't reach a terminal.
pub const NO_TERMINAL: &str = "needs a terminal, or pass the password with --password-stdin";

/// How `create-owner` ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The Owner is created.
    Created,
    /// Something was refused or failed; stderr (and the log, for a failure)
    /// says what.
    Failed,
}

impl Exit {
    /// The process's exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Created => 0,
            Self::Failed => 1,
        }
    }
}

/// Where a typed password comes from: the terminal, or a stand-in in tests.
pub trait PasswordPrompt {
    /// Shows `prompt` and reads one password without echoing it.
    ///
    /// # Errors
    /// The I/O error when there's no terminal to ask.
    fn read(&mut self, prompt: &str) -> io::Result<SecretString>;
}

/// The terminal, through rpassword.
#[derive(Debug, Default, Clone, Copy)]
pub struct TerminalPrompt;

impl PasswordPrompt for TerminalPrompt {
    fn read(&mut self, prompt: &str) -> io::Result<SecretString> {
        let mut typed = rpassword::prompt_password(prompt)?;
        let secret = exact_secret(&typed);
        typed.zeroize();
        Ok(secret)
    }
}

/// A [`SecretString`] holding exactly `text`, allocated at its length, so
/// boxing it into the secret never reallocates and leaves an unwiped copy.
fn exact_secret(text: &str) -> SecretString {
    let mut exact = String::with_capacity(text.len());
    exact.push_str(text);
    SecretString::from(exact)
}

/// Why `--password-stdin` couldn't read a password. No message shows any
/// of it.
#[derive(Debug, thiserror::Error)]
enum StdinError {
    /// Stdin is a terminal, where a typed password would show.
    #[error(
        "--password-stdin reads a piped password, and stdin is a terminal: leave the flag out to type it hidden"
    )]
    Terminal,
    /// Reading stdin failed.
    #[error("the password on stdin can't be read")]
    Unreadable(#[source] io::Error),
    /// More than [`STDIN_MAX_BYTES`].
    #[error("the password on stdin is longer than {max} bytes")]
    TooLong {
        /// The limit.
        max: usize,
    },
    /// Not UTF-8 text.
    #[error("the password on stdin isn't UTF-8")]
    NotUtf8,
}

/// Reads one password from `reader` on a blocking thread, so the read never
/// blocks the async runtime; a terminal is refused unread. The read has no
/// timeout: it ends at the pipe's end or after [`STDIN_MAX_BYTES`], and the
/// Owner check has already run. A blocking read can't be aborted; the
/// awaited handle owns it.
async fn read_stdin_blocking(
    mut reader: Box<dyn Read + Send>,
    is_terminal: bool,
) -> Result<SecretString, StdinError> {
    if is_terminal {
        return Err(StdinError::Terminal);
    }
    tokio::task::spawn_blocking(move || read_stdin(&mut *reader))
        .await
        .map_err(|_| StdinError::Unreadable(io::Error::other("the reading thread failed")))?
}

/// Reads one password from `reader`: at most [`STDIN_MAX_BYTES`], one
/// trailing `\n` or `\r\n` removed. The buffer is allocated once at its full
/// size and wiped when it's dropped.
fn read_stdin(reader: &mut dyn Read) -> Result<SecretString, StdinError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(STDIN_MAX_BYTES + 1));
    reader
        .take(u64::try_from(STDIN_MAX_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(StdinError::Unreadable)?;
    if bytes.len() > STDIN_MAX_BYTES {
        return Err(StdinError::TooLong {
            max: STDIN_MAX_BYTES,
        });
    }
    let line_break = if bytes.ends_with(b"\r\n") {
        2
    } else {
        usize::from(bytes.ends_with(b"\n"))
    };
    let len = bytes.len().saturating_sub(line_break);
    bytes.truncate(len);
    let text = core::str::from_utf8(&bytes).map_err(|_| StdinError::NotUtf8)?;
    Ok(exact_secret(text))
}

/// A prompt that can move onto a blocking thread.
type OwnedPrompt = Box<dyn PasswordPrompt + Send>;

/// Reads one password with `prompt` on a blocking thread, so waiting for the
/// terminal never blocks the async runtime, and hands the prompt back.
/// `None` when the blocking task failed (it panicked); the prompt is lost
/// then. No timeout: a person is typing. A blocking read can't be aborted;
/// the awaited handle owns it.
async fn prompt_blocking(
    mut prompt: OwnedPrompt,
    text: String,
) -> Option<(OwnedPrompt, io::Result<SecretString>)> {
    tokio::task::spawn_blocking(move || {
        let read = prompt.read(&text);
        (prompt, read)
    })
    .await
    .ok()
}

/// Where the password comes from. Both sources are owned, so each blocking
/// read can move its source onto a blocking thread and get it back.
pub enum PasswordInput {
    /// Typed twice at the terminal.
    Prompt(OwnedPrompt),
    /// One password from stdin (`--password-stdin`).
    Stdin {
        /// The process's stdin.
        reader: Box<dyn Read + Send>,
        /// Whether stdin is a terminal, which is refused.
        is_terminal: bool,
    },
}

impl core::fmt::Debug for PasswordInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Prompt(_) => f.write_str("Prompt"),
            Self::Stdin { is_terminal, .. } => f
                .debug_struct("Stdin")
                .field("is_terminal", is_terminal)
                .finish_non_exhaustive(),
        }
    }
}

/// Creates the Owner `username` in the database `config` names, with the
/// password from `input`. The outcome line goes to `out` or `err`.
pub async fn create_owner(
    config: &ServerConfig,
    username: &str,
    input: PasswordInput,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let username = match Username::try_from(username) {
        Ok(username) => username,
        Err(error) => return refuse(err, &format!("the username is invalid: {error}")),
    };
    let database = match sqlite::open(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return fail(err, "the database can't be opened", &error),
    };
    let random: Arc<dyn SecureRandom> = Arc::new(OsRandom);
    let exit = match Argon2Passwords::new(config.auth.argon2, Arc::clone(&random)).await {
        Ok(passwords) => {
            let service = UserService::new(
                Arc::new(SystemClock),
                random,
                Arc::new(database.clone()),
                Arc::new(passwords),
            );
            create(&service, &username, input, out, err).await
        }
        Err(error) => fail(err, "the password service can't start", &error),
    };
    match database.close_within(DATABASE_CLOSE).await {
        Ok(()) => exit,
        Err(error) => fail(err, "the database can't be closed", &error),
    }
}

/// What one try with a password came to.
enum Attempt {
    /// The Owner is created.
    Created,
    /// The password was refused; the prompt may ask again. The text says why.
    Refused(String),
    /// Nothing more to try.
    Stop(Exit),
}

/// The Owner check, then the password from `input`.
async fn create(
    service: &UserService,
    username: &Username,
    input: PasswordInput,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    match service.owner_exists().await {
        Ok(false) => {}
        Ok(true) => return refuse(err, "an Owner already exists"),
        Err(error) => return fail(err, "the database failed", &error),
    }
    match input {
        PasswordInput::Stdin {
            reader,
            is_terminal,
        } => {
            let password = match read_stdin_blocking(reader, is_terminal).await {
                Ok(password) => password,
                Err(error) => return refuse(err, &error.to_string()),
            };
            match attempt(service, username, password, out, err).await {
                Attempt::Created => Exit::Created,
                Attempt::Refused(why) => refuse(err, &why),
                Attempt::Stop(exit) => exit,
            }
        }
        PasswordInput::Prompt(mut prompt) => {
            let first = format!("Password for the Owner \"{}\": ", username.as_str());
            for _ in 0..ROUNDS {
                let Some((returned, Ok(password))) = prompt_blocking(prompt, first.clone()).await
                else {
                    return refuse(err, NO_TERMINAL);
                };
                let Some((returned, Ok(again))) =
                    prompt_blocking(returned, "Repeat the password: ".to_owned()).await
                else {
                    return refuse(err, NO_TERMINAL);
                };
                prompt = returned;
                if password.expose_secret() != again.expose_secret() {
                    let _ = writeln!(err, "the passwords don't match");
                    continue;
                }
                match attempt(service, username, password, out, err).await {
                    Attempt::Created => return Exit::Created,
                    Attempt::Refused(why) => {
                        let _ = writeln!(err, "{why}");
                    }
                    Attempt::Stop(exit) => return exit,
                }
            }
            refuse(err, &format!("{ROUNDS} passwords were refused"))
        }
    }
}

/// One try: the length rule, then the service (the strength check, the
/// hash, the insert and its audit entry).
async fn attempt(
    service: &UserService,
    username: &Username,
    password: SecretString,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Attempt {
    let password = match NewPassword::try_from(password) {
        Ok(password) => password,
        Err(error) => return Attempt::Refused(error.to_string()),
    };
    match service.create_owner(username, &password).await {
        Ok(id) => {
            info!(user_id = %id, "the Owner is created");
            let _ = writeln!(out, "the Owner \"{}\" is created", username.as_str());
            Attempt::Created
        }
        Err(CreateOwnerError::Password(HashError::Weak {
            warning,
            suggestions,
        })) => {
            let hints = warning
                .iter()
                .map(|warning| format!("\n  warning: {warning}"))
                .chain(
                    suggestions
                        .iter()
                        .map(|suggestion| format!("\n  suggestion: {suggestion}")),
                );
            Attempt::Refused(
                core::iter::once("the password is too weak".to_owned())
                    .chain(hints)
                    .collect(),
            )
        }
        Err(CreateOwnerError::OwnerExists) => Attempt::Stop(refuse(err, "an Owner already exists")),
        Err(CreateOwnerError::UsernameTaken) => Attempt::Stop(refuse(err, "the username is taken")),
        Err(error) => Attempt::Stop(fail(err, "the Owner can't be created", &error)),
    }
}

/// The input was refused: one line on stderr, and exit 1.
fn refuse(err: &mut dyn Write, why: &str) -> Exit {
    let _ = writeln!(err, "afkfleet-server: {why}");
    Exit::Failed
}

/// Something failed: one `error` with the whole chain, one line on stderr,
/// and exit 1.
fn fail(err: &mut dyn Write, what: &str, error: &(dyn std::error::Error + 'static)) -> Exit {
    let chain = chain(error);
    error!(error = %chain, "the Owner can't be created");
    let _ = writeln!(err, "afkfleet-server: {what}: {chain}");
    Exit::Failed
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use crate::config::RateLimitConfig;
    use crate::http::rate_limit::TrustedProxies;
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
    use fleet_core::authz::Role;
    use fleet_core::value::Username;
    use fleet_startup::config::LogConfig;
    use fleet_testkit::system::SeededRandom;
    use tempfile::TempDir;

    use super::*;
    use crate::config::{AccountsConfig, AuthConfig, DatabaseConfig, HttpConfig, VaultConfig};
    use crate::infra::crypto::passwords::Argon2Passwords;
    use crate::infra::sqlite::{self, DatabaseOptions};
    use crate::ports::audit::{AuditLimit, AuditPage};
    use crate::ports::passwords::Passwords as _;
    use crate::ports::store::Store as _;

    const STRONG: &str = "correct horse battery staple";

    /// A config whose database lives in `dir`. The vault key is never read.
    fn config(dir: &Path) -> ServerConfig {
        ServerConfig {
            dev_mode: false,
            http: HttpConfig {
                bind: HttpConfig::DEFAULT_BIND,
                request_timeout: Duration::from_secs(15),
                max_body_bytes: 65_536,
                trusted_proxies: TrustedProxies::default(),
            },
            database: DatabaseConfig {
                path: dir.join("afkfleet.db"),
            },
            log: LogConfig::default(),
            auth: AuthConfig::default(),
            rate_limits: RateLimitConfig::default(),
            vault: VaultConfig {
                key_files: BTreeMap::from([(1, dir.join("never-read"))]),
                active_key_id: 1,
            },
            accounts: AccountsConfig::default(),
        }
    }

    /// Passwords typed in order; asking for one more fails the test.
    struct Typed(VecDeque<&'static str>);

    impl Typed {
        fn new(passwords: &[&'static str]) -> Self {
            Self(passwords.iter().copied().collect())
        }
    }

    impl PasswordPrompt for Typed {
        fn read(&mut self, _prompt: &str) -> io::Result<SecretString> {
            let typed = self
                .0
                .pop_front()
                .expect("asked for more passwords than typed");
            Ok(SecretString::from(typed))
        }
    }

    /// A prompt that must never be asked.
    struct NeverAsked;

    impl PasswordPrompt for NeverAsked {
        fn read(&mut self, _prompt: &str) -> io::Result<SecretString> {
            panic!("asked for a password")
        }
    }

    /// A prompt without a terminal.
    struct NoTerminal;

    impl PasswordPrompt for NoTerminal {
        fn read(&mut self, _prompt: &str) -> io::Result<SecretString> {
            Err(io::Error::other("no terminal"))
        }
    }

    /// A stdin that must never be read.
    struct NeverRead;

    impl Read for NeverRead {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            panic!("stdin was read")
        }
    }

    /// The outcome and what went to stdout and stderr.
    struct Run {
        exit: Exit,
        out: String,
        err: String,
    }

    async fn run(dir: &Path, username: &str, input: PasswordInput) -> Run {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = create_owner(&config(dir), username, input, &mut out, &mut err).await;
        Run {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    async fn prompted(dir: &Path, typed: &[&'static str]) -> Run {
        run(
            dir,
            "alice",
            PasswordInput::Prompt(Box::new(Typed::new(typed))),
        )
        .await
    }

    async fn piped(dir: &Path, stdin: &[u8]) -> Run {
        run(
            dir,
            "alice",
            PasswordInput::Stdin {
                reader: Box::new(Cursor::new(stdin.to_vec())),
                is_terminal: false,
            },
        )
        .await
    }

    /// The stored Owner, if any, and whether `password` is theirs.
    async fn owner(dir: &Path, password: &str) -> Option<(Role, bool)> {
        let database = sqlite::open(&config(dir).database, DatabaseOptions::default())
            .await
            .unwrap();
        let user = database
            .users()
            .find_by_username(&Username::try_from("alice").unwrap())
            .await
            .unwrap()?;
        let passwords =
            Argon2Passwords::new(AuthConfig::default().argon2, Arc::new(SeededRandom::new(1)))
                .await
                .unwrap();
        let matches = passwords
            .verify(&SecretString::from(password), &user.password_hash)
            .await
            .unwrap();
        Some((user.role, matches))
    }

    // --- At a terminal -----------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_password_typed_twice_creates_the_owner() {
        let dir = TempDir::new().unwrap();

        let run = prompted(dir.path(), &[STRONG, STRONG]).await;

        assert_eq!(run.exit, Exit::Created, "{}", run.err);
        assert_eq!(run.out, "the Owner \"alice\" is created\n");
        assert_eq!(run.err, "");
        assert_eq!(owner(dir.path(), STRONG).await, Some((Role::Owner, true)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mismatch_asks_again() {
        let dir = TempDir::new().unwrap();

        let run = prompted(
            dir.path(),
            &[
                "first long password",
                "second long password",
                STRONG,
                STRONG,
            ],
        )
        .await;

        assert_eq!(run.exit, Exit::Created, "{}", run.err);
        assert_eq!(run.err, "the passwords don't match\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_weak_password_shows_the_hints_and_asks_again() {
        let dir = TempDir::new().unwrap();

        let run = prompted(
            dir.path(),
            &["passwordpassword", "passwordpassword", STRONG, STRONG],
        )
        .await;

        assert_eq!(run.exit, Exit::Created, "{}", run.err);
        assert_eq!(
            run.err,
            "the password is too weak\n  \
             warning: Repeats like \"abcabcabc\" are only slightly harder to guess than \"abc\".\n  \
             suggestion: Add another word or two. Uncommon words are better.\n  \
             suggestion: Avoid repeated words and characters.\n"
        );
        assert_eq!(owner(dir.path(), STRONG).await, Some((Role::Owner, true)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_short_password_names_the_rule_and_asks_again() {
        let dir = TempDir::new().unwrap();

        let run = prompted(dir.path(), &["too short", "too short", STRONG, STRONG]).await;

        assert_eq!(run.exit, Exit::Created, "{}", run.err);
        assert_eq!(run.err, "a password has at least 12 characters\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn three_refused_rounds_give_up_and_create_nothing() {
        let dir = TempDir::new().unwrap();

        let run = prompted(
            dir.path(),
            &[
                "first long password",
                "second long password",
                "too short",
                "too short",
                "passwordpassword",
                "passwordpassword",
            ],
        )
        .await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(run.out, "");
        assert!(
            run.err
                .ends_with("afkfleet-server: 3 passwords were refused\n"),
            "{}",
            run.err
        );
        assert_eq!(owner(dir.path(), STRONG).await, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_terminal_the_prompt_says_how_to_pass_the_password() {
        let dir = TempDir::new().unwrap();

        let run = run(
            dir.path(),
            "alice",
            PasswordInput::Prompt(Box::new(NoTerminal)),
        )
        .await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(run.err, format!("afkfleet-server: {NO_TERMINAL}\n"));
    }

    /// A prompt that answers only once another task on the same runtime has
    /// seen it start reading. Read inline on a current-thread runtime, that
    /// task never runs, so the wait is bounded at 5 s.
    struct NeedsTheRuntime {
        reading: tokio::sync::mpsc::Sender<()>,
        answer: std::sync::mpsc::Receiver<()>,
    }

    impl PasswordPrompt for NeedsTheRuntime {
        fn read(&mut self, _prompt: &str) -> io::Result<SecretString> {
            self.reading.try_send(()).map_err(io::Error::other)?;
            self.answer
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| io::Error::other("the prompt blocked the runtime for 5 s"))?;
            Ok(SecretString::from(STRONG))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_prompt_never_blocks_the_runtime() {
        let dir = TempDir::new().unwrap();
        let (reading, mut started) = tokio::sync::mpsc::channel(2);
        let (answer, answers) = std::sync::mpsc::sync_channel(2);
        let prompt = NeedsTheRuntime {
            reading,
            answer: answers,
        };
        // Ends when the prompt is dropped and its channel closes. A prompt
        // that already gave up has dropped its receiver; the assertion below
        // reports that.
        let answerer = async move {
            while started.recv().await.is_some() {
                let _ = answer.try_send(());
            }
        };

        let (run, ()) = tokio::join!(
            run(dir.path(), "alice", PasswordInput::Prompt(Box::new(prompt))),
            answerer
        );

        assert_eq!(
            run.exit,
            Exit::Created,
            "the prompt ran on the runtime's own thread: {}",
            run.err
        );
    }

    /// A stdin that delivers its password only once another task on the same
    /// runtime has seen it start reading, bounded at 5 s like the prompt.
    struct StdinNeedsTheRuntime {
        reading: tokio::sync::mpsc::Sender<()>,
        answer: std::sync::mpsc::Receiver<()>,
        rest: Cursor<Vec<u8>>,
        waited: bool,
    }

    impl Read for StdinNeedsTheRuntime {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.waited {
                self.waited = true;
                self.reading.try_send(()).map_err(io::Error::other)?;
                self.answer
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| io::Error::other("the stdin read blocked the runtime for 5 s"))?;
            }
            self.rest.read(buf)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_stdin_read_never_blocks_the_runtime() {
        let dir = TempDir::new().unwrap();
        let (reading, mut started) = tokio::sync::mpsc::channel(1);
        let (answer, answers) = std::sync::mpsc::sync_channel(1);
        let reader = StdinNeedsTheRuntime {
            reading,
            answer: answers,
            rest: Cursor::new(STRONG.as_bytes().to_vec()),
            waited: false,
        };
        // Ends when the reader is dropped and its channel closes, as above.
        let answerer = async move {
            while started.recv().await.is_some() {
                let _ = answer.try_send(());
            }
        };

        let (run, ()) = tokio::join!(
            run(
                dir.path(),
                "alice",
                PasswordInput::Stdin {
                    reader: Box::new(reader),
                    is_terminal: false,
                },
            ),
            answerer
        );

        assert_eq!(
            run.exit,
            Exit::Created,
            "stdin was read on the runtime's own thread: {}",
            run.err
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_existing_owner_stops_before_any_password_is_asked() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            prompted(dir.path(), &[STRONG, STRONG]).await.exit,
            Exit::Created
        );

        let again = run(
            dir.path(),
            "bob",
            PasswordInput::Prompt(Box::new(NeverAsked)),
        )
        .await;

        assert_eq!(again.exit, Exit::Failed);
        assert_eq!(again.err, "afkfleet-server: an Owner already exists\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_existing_owner_stops_before_stdin_is_read() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            prompted(dir.path(), &[STRONG, STRONG]).await.exit,
            Exit::Created
        );

        let again = run(
            dir.path(),
            "bob",
            PasswordInput::Stdin {
                reader: Box::new(NeverRead),
                is_terminal: false,
            },
        )
        .await;

        assert_eq!(again.exit, Exit::Failed);
        assert_eq!(again.err, "afkfleet-server: an Owner already exists\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_username_is_refused_before_anything_else() {
        let dir = TempDir::new().unwrap();

        let run = run(
            dir.path(),
            "a!",
            PasswordInput::Prompt(Box::new(NeverAsked)),
        )
        .await;

        assert_eq!(run.exit, Exit::Failed);
        assert!(
            run.err
                .starts_with("afkfleet-server: the username is invalid: "),
            "{}",
            run.err
        );
        assert!(
            !dir.path().join("afkfleet.db").exists(),
            "nothing was opened"
        );
    }

    // --- --password-stdin ----------------------------------------------------------

    #[rstest::rstest]
    #[case::bare(b"correct horse battery staple".as_slice())]
    #[case::trailing_lf(b"correct horse battery staple\n")]
    #[case::trailing_crlf(b"correct horse battery staple\r\n")]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_password_on_stdin_creates_the_owner_without_its_line_break(#[case] stdin: &[u8]) {
        let dir = TempDir::new().unwrap();

        let run = piped(dir.path(), stdin).await;

        assert_eq!(run.exit, Exit::Created, "{}", run.err);
        assert_eq!(run.out, "the Owner \"alice\" is created\n");
        assert_eq!(owner(dir.path(), STRONG).await, Some((Role::Owner, true)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_weak_password_on_stdin_is_refused_once_with_its_hints() {
        let dir = TempDir::new().unwrap();

        let run = piped(dir.path(), b"passwordpassword\n").await;

        assert_eq!(run.exit, Exit::Failed);
        assert!(
            run.err
                .starts_with("afkfleet-server: the password is too weak\n  warning: "),
            "{}",
            run.err
        );
        assert_eq!(owner(dir.path(), STRONG).await, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn more_than_1_kib_on_stdin_is_refused() {
        let dir = TempDir::new().unwrap();

        let run = piped(dir.path(), &[b'a'; STDIN_MAX_BYTES + 1]).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(
            run.err,
            "afkfleet-server: the password on stdin is longer than 1024 bytes\n"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_kib_on_stdin_reaches_the_length_rule() {
        let dir = TempDir::new().unwrap();

        let run = piped(dir.path(), &[b'a'; STDIN_MAX_BYTES]).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(
            run.err,
            "afkfleet-server: a password has at most 128 characters\n"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stdin_that_is_not_utf8_is_refused() {
        let dir = TempDir::new().unwrap();

        let run = piped(dir.path(), &[0xFF, 0xFE, b'a', b'b']).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(
            run.err,
            "afkfleet-server: the password on stdin isn't UTF-8\n"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stdin_at_a_terminal_is_refused_unread() {
        let dir = TempDir::new().unwrap();

        let run = run(
            dir.path(),
            "alice",
            PasswordInput::Stdin {
                reader: Box::new(NeverRead),
                is_terminal: true,
            },
        )
        .await;

        assert_eq!(run.exit, Exit::Failed);
        assert!(run.err.contains("--password-stdin"), "{}", run.err);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_owner_s_creation_is_audited_once() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            piped(dir.path(), STRONG.as_bytes()).await.exit,
            Exit::Created
        );
        let database = sqlite::open(&config(dir.path()).database, DatabaseOptions::default())
            .await
            .unwrap();

        let entries = database
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(10).unwrap(),
            })
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.action, AuditAction::USER_CREATE);
        assert_eq!(entry.outcome, AuditOutcome::Success);
        assert_eq!(entry.actor, None);
        assert_eq!(entry.ip, None);
        let owner = database
            .users()
            .find_by_username(&Username::try_from("alice").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            entry.target,
            Some(RecordedTarget::Known(AuditTarget::User(owner.id)))
        );
    }
}
