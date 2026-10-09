//! The command line of `afkfleet-agent` (Plan.md P5.3, ADR-0014).
//!
//! main.rs parses it with `Cli::try_parse` and handles clap's errors itself,
//! through [`exit_code`], so each command can choose the exit code of its
//! usage errors: `healthcheck`'s exit 1, since Docker reserves 2 for
//! healthchecks. Each command lists its exit codes in its own help.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// The exit codes of `afkfleet-agent run`, shown in its help.
pub const RUN_EXIT_CODES: &str = "\
Exit codes:
  0  stopped by a signal (SIGTERM or SIGINT; Ctrl+C or Ctrl+Break on Windows)
  1  startup error: the config, logging, or the fleet's setup
  2  usage error
  3  the abandoned-thread limit was reached; the agent shut down first
  4  the fleet's supervisor ended unasked, or didn't end when asked";

/// The exit codes of `afkfleet-agent healthcheck`, shown in its help. Docker
/// reserves 2 for healthchecks, so it's never used.
pub const HEALTHCHECK_EXIT_CODES: &str = "\
Exit codes:
  0  healthy: the agent touched its heartbeat file less than 30 s ago
  1  unhealthy, or a usage or config error (Docker reserves 2)";

/// `afkfleet-agent`: runs Minecraft AFK bots.
#[derive(Debug, Parser)]
#[command(
    name = "afkfleet-agent",
    bin_name = "afkfleet-agent",
    version,
    about = "Runs afkfleet's Minecraft AFK bots.",
    after_help = "Each command lists its exit codes in its own help, e.g. `afkfleet-agent run --help`."
)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The agent's commands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Runs the bots of a standalone config until a signal stops them.
    #[command(after_help = RUN_EXIT_CODES)]
    Run {
        /// The config file (TOML). `AFKFLEET_AGENT__…` environment variables
        /// override its keys.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
    /// Checks that a running agent's heartbeat is fresh, for Docker's
    /// HEALTHCHECK. Prints one line and exits 0 (healthy) or 1.
    #[command(after_help = HEALTHCHECK_EXIT_CODES)]
    Healthcheck {
        /// The running agent's config file: the check reads its
        /// `heartbeat_file`. `AFKFLEET_AGENT__…` environment variables
        /// override its keys, as for `run`.
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

    #[test]
    fn run_takes_the_config_path() {
        let cli = Cli::try_parse_from(["afkfleet-agent", "run", "--config", "agent.toml"]).unwrap();

        assert_eq!(
            cli.command,
            Command::Run {
                config: PathBuf::from("agent.toml")
            }
        );
    }

    #[test]
    fn run_requires_a_config() {
        let error = Cli::try_parse_from(["afkfleet-agent", "run"]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn the_help_of_run_lists_its_exit_codes() {
        let mut command = Cli::command();
        let run = command.find_subcommand_mut("run").unwrap();

        let help = run.render_help().to_string();

        assert!(help.contains(RUN_EXIT_CODES), "{help}");
    }

    #[test]
    fn healthcheck_takes_the_config_path() {
        let cli = Cli::try_parse_from(["afkfleet-agent", "healthcheck", "--config", "agent.toml"])
            .unwrap();

        assert_eq!(
            cli.command,
            Command::Healthcheck {
                config: PathBuf::from("agent.toml")
            }
        );
    }

    #[test]
    fn healthcheck_requires_a_config() {
        let error = Cli::try_parse_from(["afkfleet-agent", "healthcheck"]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn the_help_of_healthcheck_lists_its_exit_codes() {
        let mut command = Cli::command();
        let healthcheck = command.find_subcommand_mut("healthcheck").unwrap();

        let help = healthcheck.render_help().to_string();

        assert!(help.contains(HEALTHCHECK_EXIT_CODES), "{help}");
    }

    #[rstest]
    #[case::healthcheck_without_a_config(&["healthcheck"], 1)]
    #[case::healthcheck_with_an_unknown_flag(&["healthcheck", "--config", "a.toml", "--bogus"], 1)]
    #[case::healthcheck_help(&["healthcheck", "--help"], 0)]
    #[case::run_without_a_config(&["run"], 2)]
    #[case::an_unknown_command(&["sleep"], 2)]
    #[case::no_command(&[], 2)]
    #[case::version(&["--version"], 0)]
    #[case::help(&["--help"], 0)]
    fn usage_errors_keep_clap_s_code_except_healthcheck_s_which_exit_1(
        #[case] args: &[&str],
        #[case] code: u8,
    ) {
        let args: Vec<OsString> = core::iter::once("afkfleet-agent")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let error = Cli::try_parse_from(&args).unwrap_err();

        assert_eq!(exit_code(&error, args), code, "{error}");
    }

    #[test]
    fn the_top_level_help_points_to_each_commands_exit_codes() {
        let help = Cli::command().render_help().to_string();

        assert!(help.contains("afkfleet-agent run --help"), "{help}");
    }
}
