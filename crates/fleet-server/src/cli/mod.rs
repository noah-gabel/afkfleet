//! The command line of `afkfleet-server` (Plan.md P6.11, ADR-0015), and
//! what each command runs.
//!
//! main.rs parses it with `Cli::try_parse` and handles clap's errors itself,
//! through [`exit_code`], so each command can choose the exit code of its
//! usage errors: `healthcheck`'s exit 1, since Docker reserves 2 for
//! healthchecks. Each command lists its exit codes in its own help.
//!
//! - [`serve`]: runs the HTTP API until the process ends.
//! - [`migrate`]: applies every pending migration and exits.
//! - [`healthcheck`]: asks a running server's `GET /health/ready`, for
//!   Docker's HEALTHCHECK.
//!
//! Later phases add `user create-owner` (P7.5), `pki` (P10), `vault
//! rotate-key` (P9) and `backup` (P12).

use core::time::Duration;
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod healthcheck;
pub mod migrate;
pub mod serve;

/// How long closing the database may take once nothing uses it any more,
/// the pools' acquire timeout: `migrate` waits this long, and `serve` at
/// least this long within its shutdown deadline.
pub const DATABASE_CLOSE: Duration = Duration::from_secs(5);

/// The exit codes of `afkfleet-server serve`, shown in its help.
pub const SERVE_EXIT_CODES: &str = "\
Exit codes:
  1  startup error: the config, logging, the address or the database
  2  usage error
  4  the server ended unasked";

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
    /// Runs the HTTP API. It migrates the database first.
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

    #[test]
    fn the_top_level_help_points_to_each_commands_exit_codes() {
        let help = Cli::command().render_help().to_string();

        assert!(help.contains("afkfleet-server serve --help"), "{help}");
    }
}
