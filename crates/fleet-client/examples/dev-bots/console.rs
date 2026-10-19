//! The dev console's logic (Plan.md P10.7; ADR-0019, S71): log in with the
//! password and a 2FA code, typed hidden, then commands for the bot
//! endpoints, until `quit` or the end of input, which log out.
//!
//! It prints IDs, names, states and the server's answers; never a token,
//! the password or a code, never a line it didn't understand, and never a
//! chat line back.

use core::fmt::Write as _;
use std::io;

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::auth::{LoginRequest, StepUpRequest};
use fleet_api_types::bots::{BotPatchRequest, BotResponse, BotStatus};
use fleet_api_types::chat::{
    ChatCommandRequest, ChatSendOutcome, ChatSendRequest, ChatSendResponse,
};
use fleet_api_types::{BotId, ErrorCode, PageQuery, SecretText};
use fleet_client::{Client, ClientError, LoginStep, SessionStore};
use secrecy::{ExposeSecret as _, SecretString};

/// Where the console reads and writes.
pub(crate) trait Terminal {
    /// One line typed after `prompt`, without its line break; none at the end
    /// of input.
    ///
    /// # Errors
    /// The terminal's.
    fn line(&mut self, prompt: &str) -> io::Result<Option<String>>;

    /// A secret typed after `prompt`, never shown.
    ///
    /// # Errors
    /// The terminal's, also when there's no terminal to ask.
    fn secret(&mut self, prompt: &str) -> io::Result<SecretString>;

    /// Shows `text`.
    fn say(&mut self, text: &str);
}

/// `just dev-server`'s API.
pub(crate) const DEV_SERVER: &str = "http://127.0.0.1:8080";

/// The console's arguments, after the program's name: the username, then
/// the server's URL, [`DEV_SERVER`] when it's left out; none for anything
/// else.
pub(crate) fn arguments(args: &[String]) -> Option<(&str, &str)> {
    match args {
        [username] => Some((username.as_str(), DEV_SERVER)),
        [username, server] => Some((username.as_str(), server.as_str())),
        _ => None,
    }
}

/// The commands, as the console lists them.
pub(crate) const HELP: &str = "commands: bots | bot <id> | offline <name> | server <id> <address> | start <id> | stop <id> | chat <id> <text> | command <id> <text> | quit";

/// How many 2FA codes a login may try.
const CODES: usize = 3;

/// How a console session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// Logged in, then out.
    LoggedOut,
    /// It never logged in.
    NotLoggedIn,
    /// The session ended under it.
    SessionLost,
}

/// One command.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Bots,
    Bot(BotId),
    Offline(String),
    Server(BotId, String),
    Start(BotId),
    Stop(BotId),
    /// A chat line: the rest of the line, as typed.
    Chat(BotId, String),
    /// A slash command: the rest of the line, as typed.
    Slash(BotId, String),
    Quit,
}

impl Command {
    /// `line` as a command; none for anything else.
    fn parse(line: &str) -> Option<Self> {
        let bot = |text: &str| text.parse::<BotId>().ok();
        // A line's text is the rest of the line, its spaces kept.
        if let Some((verb, id, text)) = with_text(line) {
            match verb {
                "chat" => return bot(id).map(|id| Self::Chat(id, text.to_owned())),
                "command" => return bot(id).map(|id| Self::Slash(id, text.to_owned())),
                _ => {}
            }
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["bots"] => Some(Self::Bots),
            ["bot", id] => bot(id).map(Self::Bot),
            ["offline", name] => Some(Self::Offline((*name).to_owned())),
            ["server", id, address] => bot(id).map(|id| Self::Server(id, (*address).to_owned())),
            ["start", id] => bot(id).map(Self::Start),
            ["stop", id] => bot(id).map(Self::Stop),
            ["quit"] => Some(Self::Quit),
            _ => None,
        }
    }
}

/// `line`'s first word, its second, and the rest after the space that ends
/// the second, as typed; none without all three.
fn with_text(line: &str) -> Option<(&str, &str, &str)> {
    let (verb, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let (id, text) = rest.trim_start().split_once(char::is_whitespace)?;
    Some((verb, id, text))
}

/// Runs a console session for `username` over `client`; see the [module
/// docs](self).
pub(crate) async fn run<S: SessionStore>(
    client: &Client<S>,
    username: &str,
    terminal: &mut impl Terminal,
) -> Ended {
    if !log_in(client, username, terminal).await {
        return Ended::NotLoggedIn;
    }
    terminal.say(HELP);
    loop {
        let line = match terminal.line("> ") {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                terminal.say(&format!("the terminal can't be read: {error}"));
                break;
            }
        };
        let Some(command) = Command::parse(&line) else {
            terminal.say(&format!("not a command; {HELP}"));
            continue;
        };
        if command == Command::Quit {
            break;
        }
        if let Err(ClientError::NoSession) = command_of(client, command, terminal).await {
            terminal.say("the session ended; run the console again to log in");
            return Ended::SessionLost;
        }
    }
    match client.logout().await {
        Ok(()) => terminal.say("logged out"),
        Err(error) => terminal.say(&describe(&error)),
    }
    Ended::LoggedOut
}

/// The password, then the 2FA code, up to [`CODES`] of them; whether it
/// logged in.
async fn log_in<S: SessionStore>(
    client: &Client<S>,
    username: &str,
    terminal: &mut impl Terminal,
) -> bool {
    let password = match terminal.secret("password: ") {
        Ok(password) => password,
        Err(error) => {
            terminal.say(&format!("the password can't be read: {error}"));
            return false;
        }
    };
    let request = LoginRequest {
        username: SecretText::new(username.to_owned()),
        password: secret_text(&password),
    };
    drop(password);
    match client.login(&request).await {
        Ok(LoginStep::MfaRequired { .. }) => {}
        Ok(LoginStep::SetupRequired { .. }) => {
            terminal.say("this user has no 2FA yet: log in once in the app (`just dev-app`) to set it up, then come back");
            return false;
        }
        Err(error) => {
            terminal.say(&describe(&error));
            return false;
        }
    }
    for attempt in 1..=CODES {
        let code = match terminal.secret("2FA code: ") {
            Ok(code) => code,
            Err(error) => {
                terminal.say(&format!("the code can't be read: {error}"));
                return false;
            }
        };
        match client.login_mfa(secret_text(&code)).await {
            Ok(()) => {
                terminal.say(&format!("logged in as {username}"));
                return true;
            }
            Err(ClientError::Api(failure))
                if attempt < CODES && failure.is(ErrorCode::InvalidCredentials) =>
            {
                terminal.say("that code wasn't accepted; try the next one");
            }
            Err(error) => {
                terminal.say(&describe(&error));
                return false;
            }
        }
    }
    false
}

/// `secret` as the API's secret text.
fn secret_text<const MAX: usize>(secret: &SecretString) -> SecretText<MAX> {
    SecretText::new(secret.expose_secret().to_owned())
}

/// Runs `command`; a missing session comes back, every other error is shown.
async fn command_of<S: SessionStore>(
    client: &Client<S>,
    command: Command,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let done = match command {
        Command::Bots => bots(client, terminal).await,
        Command::Bot(id) => client.bot(id).await.map(|bot| {
            terminal.say(&serde_json::to_string_pretty(&bot).unwrap_or_default());
        }),
        Command::Offline(name) => offline(client, &name, terminal).await,
        Command::Server(id, address) => server(client, id, address, terminal).await,
        Command::Start(id) => client
            .start_bot(id)
            .await
            .map(|bot| terminal.say(&line_of(&bot))),
        Command::Stop(id) => client
            .stop_bot(id)
            .await
            .map(|bot| terminal.say(&line_of(&bot))),
        Command::Chat(id, text) => client
            .send_chat(id, &ChatSendRequest { message: text })
            .await
            .map(|answer| terminal.say(&answer_of(&answer))),
        Command::Slash(id, text) => send_command(client, id, text, terminal).await,
        Command::Quit => Ok(()),
    };
    match done {
        Err(ClientError::NoSession) => Err(ClientError::NoSession),
        Err(error) => {
            terminal.say(&describe(&error));
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}

/// Every page of the caller's bots, one line each.
async fn bots<S: SessionStore>(
    client: &Client<S>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let mut before = None;
    let mut any = false;
    loop {
        let page = client
            .bots(&PageQuery {
                before,
                limit: None,
            })
            .await?;
        for bot in &page.items {
            terminal.say(&line_of(bot));
            any = true;
        }
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    if !any {
        terminal.say("no bots yet: `offline <name>` adds one");
    }
    Ok(())
}

/// A new offline account `name`, and its bot.
async fn offline<S: SessionStore>(
    client: &Client<S>,
    name: &str,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let account = client
        .create_offline_account(&CreateOfflineAccountRequest {
            name: SecretText::new(name.to_owned()),
        })
        .await?;
    let mut before = None;
    loop {
        let page = client
            .bots(&PageQuery {
                before,
                limit: None,
            })
            .await?;
        if let Some(bot) = page.items.iter().find(|bot| bot.account.id == account.id) {
            terminal.say(&format!(
                "account {} ({}); {}",
                account.id,
                account.name,
                line_of(bot)
            ));
            return Ok(());
        }
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    terminal.say(&format!("account {} ({})", account.id, account.name));
    Ok(())
}

/// Sends bot `id` the command `text`, with a step-up when the server asks,
/// once.
async fn send_command<S: SessionStore>(
    client: &Client<S>,
    id: BotId,
    text: String,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let request = ChatCommandRequest { message: text };
    let answer = match client.send_command(id, &request).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.send_command(id, &request).await?
        }
        other => other?,
    };
    terminal.say(&answer_of(&answer));
    Ok(())
}

/// Asks for the password again and steps up: false when it can't be read.
async fn stepped_up<S: SessionStore>(
    client: &Client<S>,
    terminal: &mut impl Terminal,
) -> Result<bool, ClientError> {
    let password = match terminal.secret("password again: ") {
        Ok(password) => password,
        Err(error) => {
            terminal.say(&format!("the password can't be read: {error}"));
            return Ok(false);
        }
    };
    let stepped = client
        .step_up(&StepUpRequest {
            password: secret_text(&password),
        })
        .await;
    drop(password);
    stepped?;
    Ok(true)
}

/// Sets bot `id`'s server, with a step-up when the server asks, once.
async fn server<S: SessionStore>(
    client: &Client<S>,
    id: BotId,
    address: String,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let request = BotPatchRequest {
        server_address: address,
    };
    let bot = match client.patch_bot(id, &request).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.patch_bot(id, &request).await?
        }
        other => other?,
    };
    terminal.say(&line_of(&bot));
    Ok(())
}

/// A sent line's answer as one line: whether its agent said the bot sent
/// it, and the request's and the line's IDs; never the text.
fn answer_of(answer: &ChatSendResponse) -> String {
    let outcome = match answer.outcome.known() {
        Some(ChatSendOutcome::Sent) => "sent",
        Some(ChatSendOutcome::Pending) => "pending, no answer yet",
        None => answer.outcome.as_str(),
    };
    format!(
        "{outcome} (request {}, line {})",
        answer.request_id,
        answer.message_id.get()
    )
}

/// A bot as one line: its ID, its account's name and its status. The ID is
/// labelled: a bot's and its account's are minted from the same time and
/// look almost alike.
fn line_of(bot: &BotResponse) -> String {
    format!(
        "bot {} {} {}",
        bot.id,
        bot.account.name,
        status_of(&bot.status)
    )
}

/// A status as `kind[/state][ reason=…][ why=…]`.
fn status_of(status: &BotStatus) -> String {
    let mut shown = status.kind.as_str().to_owned();
    if let Some(state) = &status.state {
        shown.push('/');
        shown.push_str(state.as_str());
    }
    if let Some(reason) = &status.reason {
        shown.push_str(" reason=");
        shown.push_str(reason.as_str());
    }
    if let Some(why) = &status.why {
        shown.push_str(" why=");
        shown.push_str(why.as_str());
    }
    shown
}

/// An error as the console shows it: the server's answer with its fields,
/// or the client's own text.
fn describe(error: &ClientError) -> String {
    match error {
        ClientError::Api(failure) => {
            let mut shown = failure.to_string();
            for field in &failure.fields {
                // Writing into a String never fails.
                let _ = write!(shown, "\n  {}: {}", field.path, field.message);
            }
            shown
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_a_command_only_in_its_exact_shape() {
        let id = "018bcfe5-6800-7bab-abab-abababababab";
        let bot: BotId = id.parse().unwrap();

        assert_eq!(Command::parse("bots"), Some(Command::Bots));
        assert_eq!(
            Command::parse(&format!("  start {id} ")),
            Some(Command::Start(bot))
        );
        assert_eq!(
            Command::parse(&format!("server {id} 127.0.0.1:25565")),
            Some(Command::Server(bot, "127.0.0.1:25565".to_owned()))
        );
        assert_eq!(Command::parse("start not-an-id"), None);
        assert_eq!(Command::parse("bots please"), None);
        assert_eq!(Command::parse(""), None);
    }

    #[test]
    fn a_chat_or_command_line_takes_the_rest_of_the_line_as_typed() {
        let id = "018bcfe5-6800-7bab-abab-abababababab";
        let bot: BotId = id.parse().unwrap();

        assert_eq!(
            Command::parse(&format!("chat {id} hello   there ")),
            Some(Command::Chat(bot, "hello   there ".to_owned()))
        );
        assert_eq!(
            Command::parse(&format!("  command  {id}  /op AfkBot2")),
            Some(Command::Slash(bot, " /op AfkBot2".to_owned()))
        );
        assert_eq!(Command::parse(&format!("chat {id}")), None, "no text");
        assert_eq!(Command::parse("chat not-an-id hello"), None);
        assert_eq!(Command::parse("chatter x hello"), None);
    }

    #[test]
    fn an_answer_reads_as_sent_or_pending_with_its_ids() {
        use fleet_api_types::SafeInt;
        use fleet_api_types::chat::ChatSendOutcome;
        let request = "018bcfe5-6800-7bab-abab-abababababac";
        let answer = |outcome: ChatSendOutcome| ChatSendResponse {
            request_id: request.parse().unwrap(),
            message_id: SafeInt::try_new(41).unwrap(),
            outcome: outcome.into(),
        };

        assert_eq!(
            answer_of(&answer(ChatSendOutcome::Sent)),
            format!("sent (request {request}, line 41)")
        );
        assert_eq!(
            answer_of(&answer(ChatSendOutcome::Pending)),
            format!("pending, no answer yet (request {request}, line 41)")
        );
    }
}
