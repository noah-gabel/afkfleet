//! The command line of `afkfleet-server` (Plan.md P6.11, ADR-0015), and
//! what each command runs.
//!
//! main.rs parses it with `Cli::try_parse` and handles clap's errors itself,
//! through [`exit_code`], so each command can choose the exit code of its
//! usage errors: `healthcheck`'s exit 1, since Docker reserves 2 for
//! healthchecks. Each command lists its exit codes in its own help.
//!
//! - [`serve`]: runs the HTTP API until a signal stops it.
//! - [`housekeeping`]: `serve`'s periodic background task: the rate
//!   limiters' memory, the lockout's purge and the refusals' summary (P7.10).
//! - [`migrate`]: applies every pending migration and exits.
//! - [`healthcheck`]: asks a running server's `GET /health/ready`, for
//!   Docker's HEALTHCHECK.
//! - [`user`]: `user create-owner`, the bootstrap of the Owner (P7.5).
//! - [`vault`]: `vault status` and `vault rotate-key`, what the vault's keys
//!   seal and re-encrypting it all under the active key (P9.2).
//!
//! Later phases add `pki` (P10) and `backup` (P12).

use core::time::Duration;
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod healthcheck;
pub mod housekeeping;
pub mod migrate;
pub mod serve;
pub mod user;
pub mod vault;

/// How long closing the database may take once nothing uses it any more,
/// the pools' acquire timeout: `migrate` waits this long, and `serve` at
/// least this long within its shutdown deadline.
pub const DATABASE_CLOSE: Duration = Duration::from_secs(5);

/// The exit codes of `afkfleet-server serve`, shown in its help.
pub const SERVE_EXIT_CODES: &str = "\
Exit codes:
  0  stopped by a signal (SIGTERM or SIGINT; Ctrl+C or Ctrl+Break on Windows)
  1  startup error: the config, logging, the address or the database
  2  usage error
  4  the server ended unasked, or didn't finish its shutdown by the deadline";

/// The exit codes of `afkfleet-server migrate`, shown in its help.
pub const MIGRATE_EXIT_CODES: &str = "\
Exit codes:
  0  the database is migrated
  1  the config, logging or the database failed
  2  usage error";

/// The exit codes of `afkfleet-server healthcheck`, shown in its help.
/// Docker reserves 2 for healthchecks, so it's never used.
pub const HEALTHCHECK_EXIT_CODES: &str = "\
Exit codes:
  0  healthy: GET /health/ready answered 200 with an empty body
  1  unhealthy, or a usage or config error (Docker reserves 2)";

/// `afkfleet-server`: the afkfleet server.
#[derive(Debug, Parser)]
#[command(
    name = "afkfleet-server",
    bin_name = "afkfleet-server",
    version,
    about = "Runs afkfleet's server: its HTTP API and database.",
    after_help = "Each command lists its exit codes in its own help, e.g. `afkfleet-server serve --help`."
)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The server's commands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Runs the HTTP API until a signal stops it. It migrates the database
    /// first.
    #[command(after_help = SERVE_EXIT_CODES)]
    Serve {
        /// The config file (TOML). `AFKFLEET_SERVER__…` environment variables
        /// override its keys.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
    /// Applies every pending migration to the database, creating it if it
    /// doesn't exist yet, and exits.
    #[command(after_help = MIGRATE_EXIT_CODES)]
    Migrate {
        /// The config file (TOML): the command reads `[database] path` and
        /// `[log]`. `AFKFLEET_SERVER__…` environment variables override its
        /// keys, as for `serve`.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
    /// Checks that a running server is ready, for Docker's HEALTHCHECK.
    /// Prints one line and exits 0 (healthy) or 1.
    #[command(after_help = HEALTHCHECK_EXIT_CODES)]
    Healthcheck {
        /// The running server's config file: the check asks `[http] bind`,
        /// or loopback on its port when it's an unspecified address.
        /// `AFKFLEET_SERVER__…` environment variables override its keys, as
        /// for `serve`.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
    /// Manages users from the command line.
    User {
        /// What to do.
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Shows what the vault's keys seal, and re-encrypts the secrets under
    /// the active key. Neither creates or migrates the database.
    Vault {
        /// What to do.
        #[command(subcommand)]
        command: VaultCommand,
    },
}

/// `afkfleet-server user …`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum UserCommand {
    /// Creates the Owner, the one user with every right. It refuses when an
    /// Owner exists. The password is typed twice at the terminal, or read
    /// once from stdin with --password-stdin; there's no password flag.
    #[command(after_help = user::CREATE_OWNER_EXIT_CODES)]
    CreateOwner {
        /// The config file (TOML): the command reads `[database] path`,
        /// `[auth]` and `[log]`, never the vault's keys.
        /// `AFKFLEET_SERVER__…` environment variables override its keys, as
        /// for `serve`.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        /// The Owner's username: 3 to 32 letters, digits, `_`, `.` and `-`.
        #[arg(long, value_name = "NAME")]
        username: String,
        /// Reads the password from stdin (one line, at most 1 KiB) instead of
        /// asking at the terminal, for scripts.
        #[arg(long)]
        password_stdin: bool,
    },
}

/// `afkfleet-server vault …`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum VaultCommand {
    /// Prints how many secrets each key ID seals, read from the envelopes'
    /// headers. It reads no key file.
    #[command(after_help = vault::STATUS_EXIT_CODES)]
    Status {
        /// The config file (TOML): the command reads `[database] path`,
        /// `[vault]` (the key IDs, never the files) and `[log]`.
        /// `AFKFLEET_SERVER__…` environment variables override its keys, as
        /// for `serve`.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
    /// Re-encrypts every secret sealed under another key than `[vault]
    /// active_key_id`, in one transaction, then prints the status. Stop the
    /// server and copy the database first (README).
    #[command(after_help = vault::ROTATE_KEY_EXIT_CODES)]
    RotateKey {
        /// The config file (TOML): the command reads `[database] path`,
        /// `[vault]` with every key file, and `[log]`. `AFKFLEET_SERVER__…`
        /// environment variables override its keys, as for `serve`.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
}

/// The exit code for a command line clap refused, or for `--help` and
/// `--version`, which clap reports as errors too.
///
/// It's clap's own code, except that a usage error of `healthcheck` exits
/// with 1: Docker reserves 2 for healthchecks. `args` are the process's
/// arguments, the program's name first, so the command is the second.
#[must_use]
pub fn exit_code(error: &clap::Error, args: impl IntoIterator<Item = OsString>) -> u8 {
    // clap's codes are 0 (`--help`, `--version`) and 2 (usage errors).
    let code = u8::try_from(error.exit_code()).unwrap_or(2);
    let healthcheck = args
        .into_iter()
        .nth(1)
        .is_some_and(|command| command == "healthcheck");
    if healthcheck && code != 0 { 1 } else { code }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;
    use clap::error::ErrorKind;
    use rstest::rstest;

    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[rstest]
    #[case::serve("serve", Command::Serve { config: PathBuf::from("server.toml") })]
    #[case::migrate("migrate", Command::Migrate { config: PathBuf::from("server.toml") })]
    #[case::healthcheck("healthcheck", Command::Healthcheck { config: PathBuf::from("server.toml") })]
    fn each_command_takes_the_config_path(#[case] name: &str, #[case] expected: Command) {
        let cli =
            Cli::try_parse_from(["afkfleet-server", name, "--config", "server.toml"]).unwrap();

        assert_eq!(cli.command, expected);
    }

    #[rstest]
    #[case::serve("serve")]
    #[case::migrate("migrate")]
    #[case::healthcheck("healthcheck")]
    fn each_command_requires_a_config(#[case] name: &str) {
        let error = Cli::try_parse_from(["afkfleet-server", name]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[rstest]
    #[case::serve("serve", SERVE_EXIT_CODES)]
    #[case::migrate("migrate", MIGRATE_EXIT_CODES)]
    #[case::healthcheck("healthcheck", HEALTHCHECK_EXIT_CODES)]
    fn each_commands_help_lists_its_exit_codes(#[case] name: &str, #[case] codes: &str) {
        let mut command = Cli::command();
        let subcommand = command.find_subcommand_mut(name).unwrap();

        let help = subcommand.render_help().to_string();

        assert!(help.contains(codes), "{help}");
    }

    #[rstest]
    #[case::healthcheck_without_a_config(&["healthcheck"], 1)]
    #[case::healthcheck_with_an_unknown_flag(&["healthcheck", "--config", "a.toml", "--bogus"], 1)]
    #[case::healthcheck_help(&["healthcheck", "--help"], 0)]
    #[case::serve_without_a_config(&["serve"], 2)]
    #[case::migrate_without_a_config(&["migrate"], 2)]
    #[case::an_unknown_command(&["sleep"], 2)]
    #[case::no_command(&[], 2)]
    #[case::version(&["--version"], 0)]
    #[case::help(&["--help"], 0)]
    fn usage_errors_keep_clap_s_code_except_healthcheck_s_which_exit_1(
        #[case] args: &[&str],
        #[case] code: u8,
    ) {
        let args: Vec<OsString> = core::iter::once("afkfleet-server")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let error = Cli::try_parse_from(&args).unwrap_err();

        assert_eq!(exit_code(&error, args), code, "{error}");
    }

    #[rstest]
    #[case::prompt(&[], false)]
    #[case::stdin(&["--password-stdin"], true)]
    fn create_owner_takes_the_config_the_username_and_where_the_password_comes_from(
        #[case] extra: &[&str],
        #[case] password_stdin: bool,
    ) {
        let args = [
            "afkfleet-server",
            "user",
            "create-owner",
            "--config",
            "server.toml",
            "--username",
            "alice",
        ];

        let cli = Cli::try_parse_from(args.iter().chain(extra)).unwrap();

        assert_eq!(
            cli.command,
            Command::User {
                command: UserCommand::CreateOwner {
                    config: PathBuf::from("server.toml"),
                    username: "alice".to_owned(),
                    password_stdin,
                }
            }
        );
    }

    #[rstest]
    #[case::no_config(&["user", "create-owner", "--username", "alice"])]
    #[case::no_username(&["user", "create-owner", "--config", "server.toml"])]
    #[case::a_password_flag(&["user", "create-owner", "--config", "a.toml", "--username", "alice", "--password", "x"])]
    #[case::no_user_command(&["user"])]
    fn create_owner_usage_errors_exit_2(#[case] args: &[&str]) {
        let args: Vec<OsString> = core::iter::once("afkfleet-server")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let error = Cli::try_parse_from(&args).unwrap_err();

        assert_eq!(exit_code(&error, args), 2, "{error}");
    }

    #[test]
    fn create_owner_s_help_lists_its_exit_codes() {
        let mut command = Cli::command();
        let create_owner = command
            .find_subcommand_mut("user")
            .unwrap()
            .find_subcommand_mut("create-owner")
            .unwrap();

        let help = create_owner.render_help().to_string();

        assert!(help.contains(user::CREATE_OWNER_EXIT_CODES), "{help}");
    }

    #[rstest]
    #[case::status("status", VaultCommand::Status { config: PathBuf::from("server.toml") })]
    #[case::rotate_key("rotate-key", VaultCommand::RotateKey { config: PathBuf::from("server.toml") })]
    fn each_vault_command_takes_the_config_path(
        #[case] name: &str,
        #[case] expected: VaultCommand,
    ) {
        let cli =
            Cli::try_parse_from(["afkfleet-server", "vault", name, "--config", "server.toml"])
                .unwrap();

        assert_eq!(cli.command, Command::Vault { command: expected });
    }

    #[rstest]
    #[case::status_without_a_config(&["vault", "status"])]
    #[case::rotate_key_without_a_config(&["vault", "rotate-key"])]
    #[case::no_vault_command(&["vault"])]
    #[case::an_unknown_vault_command(&["vault", "rotate", "--config", "server.toml"])]
    fn vault_usage_errors_exit_2(#[case] args: &[&str]) {
        let args: Vec<OsString> = core::iter::once("afkfleet-server")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let error = Cli::try_parse_from(&args).unwrap_err();

        assert_eq!(exit_code(&error, args), 2, "{error}");
    }

    #[rstest]
    #[case::status("status", vault::STATUS_EXIT_CODES)]
    #[case::rotate_key("rotate-key", vault::ROTATE_KEY_EXIT_CODES)]
    fn each_vault_commands_help_lists_its_exit_codes(#[case] name: &str, #[case] codes: &str) {
        let mut command = Cli::command();
        let subcommand = command
            .find_subcommand_mut("vault")
            .unwrap()
            .find_subcommand_mut(name)
            .unwrap();

        let help = subcommand.render_help().to_string();

        assert!(help.contains(codes), "{help}");
    }

    #[test]
    fn the_top_level_help_points_to_each_commands_exit_codes() {
        let help = Cli::command().render_help().to_string();

        assert!(help.contains("afkfleet-server serve --help"), "{help}");
    }
}
