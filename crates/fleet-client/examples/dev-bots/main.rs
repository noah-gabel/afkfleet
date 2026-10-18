//! `just dev-bots username [server]` (Plan.md P10.7; ADR-0019, S71): a
//! console for `just dev-server`'s bot endpoints over the real client, so
//! the hands-on check needs neither curl nor a password on a command line.
//!
//! The password, the 2FA code and the step-up's password are typed hidden
//! through fleet-server's `TerminalPrompt` (rpassword, as `just dev-owner`
//! asks), never an argument, a variable or stdin; without a terminal it
//! refuses to start. The session lives in memory and ends with the console.
//! The logic is in `console`, which fleet-client's tests run against the
//! in-process server.

mod console;

use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::process::ExitCode;

use fleet_client::{Client, ClientConfig, MemoryStore, ServerUrl, Trust};
use fleet_server::cli::user::{PasswordPrompt as _, TerminalPrompt};
use secrecy::SecretString;

use self::console::{Ended, Terminal};

/// `just dev-server`'s API.
const DEV_SERVER: &str = "http://127.0.0.1:8080";

/// The process's terminal.
struct StdTerminal;

impl Terminal for StdTerminal {
    fn line(&mut self, prompt: &str) -> io::Result<Option<String>> {
        let mut out = io::stdout().lock();
        write!(out, "{prompt}")?;
        out.flush()?;
        let mut line = String::new();
        let read = io::stdin().lock().read_line(&mut line)?;
        Ok((read > 0).then(|| line.trim_end_matches(['\r', '\n']).to_owned()))
    }

    fn secret(&mut self, prompt: &str) -> io::Result<SecretString> {
        TerminalPrompt.read(prompt)
    }

    fn say(&mut self, text: &str) {
        // A failed write has nowhere else to go.
        let _ = writeln!(io::stdout().lock(), "{text}");
    }
}

/// One line on stderr.
fn complain(text: &str) {
    let _ = writeln!(io::stderr().lock(), "dev-bots: {text}");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (username, server) = match args.as_slice() {
        [username] => (username.as_str(), DEV_SERVER),
        [username, server] => (username.as_str(), server.as_str()),
        _ => {
            complain(&format!(
                "usage: dev-bots <username> [server URL, by default {DEV_SERVER}]"
            ));
            return ExitCode::from(2);
        }
    };
    if !io::stdin().is_terminal() {
        complain("needs a terminal: the password and the codes are typed hidden, never piped");
        return ExitCode::from(2);
    }
    let server = match ServerUrl::parse(server) {
        Ok(server) => server,
        Err(error) => {
            complain(&error.to_string());
            return ExitCode::from(2);
        }
    };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = match Client::new(
        ClientConfig {
            server,
            trust: Trust::System,
        },
        MemoryStore::new(),
    ) {
        Ok(client) => client,
        Err(error) => {
            complain(&error.to_string());
            return ExitCode::from(1);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            complain(&format!("the async runtime can't be built: {error}"));
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(console::run(&client, username, &mut StdTerminal)) {
        Ended::LoggedOut => ExitCode::SUCCESS,
        Ended::NotLoggedIn | Ended::SessionLost => ExitCode::from(1),
    }
}
