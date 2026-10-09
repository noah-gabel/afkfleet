//! The command line of `afkfleet-agent` (Plan.md P5.3, ADR-0014).
//!
//! main.rs parses it with `Cli::try_parse` and handles clap's errors itself,
//! so each command can choose the exit code of its usage errors. Each
//! command lists its exit codes in its own help.

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
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;
    use clap::error::ErrorKind;

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
    fn the_top_level_help_points_to_each_commands_exit_codes() {
        let help = Cli::command().render_help().to_string();

        assert!(help.contains("afkfleet-agent run --help"), "{help}");
    }
}
