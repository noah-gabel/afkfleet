//! The dev console's logic (Plan.md P10.7, P11.4; ADR-0019, S71): log in
//! with the password and a 2FA code, typed hidden, then commands for the bot
//! and mode endpoints, until `quit` or the end of input, which log out.
//!
//! It prints IDs, names, states and the server's answers; never a token,
//! the password or a code, never a line it didn't understand, and never a
//! chat line typed into it back, nor a mode's steps: `new-mode` and
//! `edit-mode` take a mode's JSON and show only its line, and JSON that
//! isn't a mode is said so, never repeated. `delete-mode` asks for the
//! password again when the server wants it. `history` shows a bot's chat
//! as the server sends it to this user: cleaned, a line break as ` | `, a
//! command's arguments only for Manage holders (ADR-0020, S9, S11, U3).
//! `events` prints the live events, one line each, until Enter (P11.5;
//! ADR-0020, S37): never the socket's URL, its ticket or the token it
//! opens with.

use core::fmt::Write as _;
use core::pin::Pin;
use core::time::Duration;
use std::io;

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::auth::{LoginRequest, StepUpRequest};
use fleet_api_types::bots::{BotModeRequest, BotPatchRequest, BotResponse, BotStatus};
use fleet_api_types::chat::{
    ChatCommandRequest, ChatLine, ChatPageQuery, ChatSendOutcome, ChatSendRequest, ChatSendResponse,
};
use fleet_api_types::events::{EventMessage, EventMessageKind};
use fleet_api_types::modes::{
    CreateModeRequest, ModeOwnership, ModeResponse, ModeVisibility, UpdateModeRequest,
};
use fleet_api_types::{BotId, ErrorCode, ErrorResponse, ModeId, PageQuery, SafeInt, SecretText};
use fleet_client::events::{EventsHandshake, Received, SocketClose, read};
use fleet_client::{Client, ClientError, LoginStep, SessionStore};
use futures_util::StreamExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

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

    /// Waits for Enter, without holding the terminal: `events` prints the
    /// live events meanwhile.
    fn enter(&mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;
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
pub(crate) const HELP: &str = "commands: bots | bot <id> | offline <name> | server <id> <address> | mode <id> <mode> | start <id> | stop <id> | chat <id> <text> | command <id> <text> | history <id> [before] | modes | new-mode <json> | edit-mode <id> <json> | delete-mode <id> | events | quit";

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
    /// One page of a bot's chat: the newest, or the one before a line.
    History(BotId, Option<SafeInt>),
    /// A bot's mode.
    SetMode(BotId, ModeId),
    /// Every mode the user may see.
    Modes,
    /// A new mode: the rest of the line, as typed, its JSON.
    NewMode(String),
    /// A mode edited: the rest of the line, as typed, its JSON with the
    /// version read.
    EditMode(ModeId, String),
    /// A mode deleted.
    DeleteMode(ModeId),
    /// The live events, until Enter.
    Events,
    Quit,
}

impl Command {
    /// `line` as a command; none for anything else.
    fn parse(line: &str) -> Option<Self> {
        let bot = |text: &str| text.parse::<BotId>().ok();
        // A line's text is the rest of the line, its spaces kept; a line
        // without text is no command, never an empty line sent.
        if let Some((verb, id, text)) =
            with_text(line).filter(|(_, _, text)| !text.trim().is_empty())
        {
            match verb {
                "chat" => return bot(id).map(|id| Self::Chat(id, text.to_owned())),
                "command" => return bot(id).map(|id| Self::Slash(id, text.to_owned())),
                // An edit's JSON is the rest of the line, as typed.
                "edit-mode" => {
                    return id
                        .parse::<ModeId>()
                        .ok()
                        .map(|id| Self::EditMode(id, text.to_owned()));
                }
                _ => {}
            }
        }
        // A new mode's JSON is the rest of the line, as typed.
        if let Some(("new-mode", json)) = line
            .trim_start()
            .split_once(char::is_whitespace)
            .filter(|(_, json)| !json.trim().is_empty())
        {
            return Some(Self::NewMode(json.to_owned()));
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["modes"] => Some(Self::Modes),
            ["events"] => Some(Self::Events),
            ["delete-mode", id] => id.parse::<ModeId>().ok().map(Self::DeleteMode),
            ["mode", id, mode] => bot(id)
                .zip(mode.parse::<ModeId>().ok())
                .map(|(id, mode)| Self::SetMode(id, mode)),
            ["bots"] => Some(Self::Bots),
            ["bot", id] => bot(id).map(Self::Bot),
            ["offline", name] => Some(Self::Offline((*name).to_owned())),
            ["server", id, address] => bot(id).map(|id| Self::Server(id, (*address).to_owned())),
            ["start", id] => bot(id).map(Self::Start),
            ["stop", id] => bot(id).map(Self::Stop),
            ["history", id] => bot(id).map(|id| Self::History(id, None)),
            ["history", id, before] => {
                let before = before.parse::<i64>().ok()?;
                let before = SafeInt::try_new(before).ok()?;
                bot(id).map(|id| Self::History(id, Some(before)))
            }
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
        Command::History(id, before) => history(client, id, before, terminal).await,
        Command::SetMode(id, mode) => client
            .set_bot_mode(id, &BotModeRequest { mode_id: mode })
            .await
            .map(|bot| terminal.say(&line_of(&bot))),
        Command::Modes => modes(client, terminal).await,
        Command::NewMode(json) => new_mode(client, &json, terminal).await,
        Command::EditMode(id, json) => edit_mode(client, id, &json, terminal).await,
        Command::DeleteMode(id) => delete_mode(client, id, terminal).await,
        Command::Events => events(client, terminal).await,
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

/// One page of the bot `id`'s chat, the one before `before` or the newest,
/// oldest first, and where the older lines start.
async fn history<S: SessionStore>(
    client: &Client<S>,
    id: BotId,
    before: Option<SafeInt>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let page = client
        .chat(
            id,
            &ChatPageQuery {
                before,
                limit: None,
            },
        )
        .await?;
    if page.items.is_empty() {
        terminal.say("no chat yet");
    }
    for line in page.items.iter().rev() {
        terminal.say(&chat_line_of(line));
    }
    if let Some(next) = page.next {
        terminal.say(&format!("older: history {id} {next}"));
    }
    Ok(())
}

/// One chat line: its ID, time and kind; who sent it (a received line's
/// Minecraft sender, or the user who had it sent, as the server shows
/// them); its text; and whether arguments were left out or the text cut.
fn chat_line_of(line: &ChatLine) -> String {
    let who = match (&line.sender, &line.sent_by) {
        (Some(sender), _) => format!(" {}", sender.name),
        (None, _) if line.mine => " by you".to_owned(),
        (None, Some(by)) => format!(" by {}", by.username),
        (None, None) => String::new(),
    };
    let mut shown = format!(
        "#{} {} {}{who}: {}",
        line.id,
        line.at,
        line.kind.as_str(),
        line.text
    );
    if line.arguments_hidden {
        shown.push_str(" (arguments hidden)");
    }
    if line.truncated {
        shown.push_str(" (cut)");
    }
    shown
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

/// Every page of the modes the user may see, one line each.
async fn modes<S: SessionStore>(
    client: &Client<S>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let mut before = None;
    loop {
        let page = client
            .modes(&PageQuery {
                before,
                limit: None,
            })
            .await?;
        for mode in &page.items {
            terminal.say(&mode_line_of(mode));
        }
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    Ok(())
}

/// Creates the mode `json` holds, through the step-up route after the
/// password again when its steps send a command off the allowlist, once.
/// JSON that isn't a mode is said so, never repeated: serde's message would
/// quote it, and its chat lines with it.
async fn new_mode<S: SessionStore>(
    client: &Client<S>,
    json: &str,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let Ok(request) = serde_json::from_str::<CreateModeRequest>(json) else {
        terminal.say(
            "not a mode's JSON: {\"name\", \"visibility\", \"steps\"}, as the README shows; nothing was sent",
        );
        return Ok(());
    };
    let mode = match client.create_mode(&request).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::ModeNeedsStepUp) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.create_mode_with_commands(&request).await?
        }
        other => other?,
    };
    terminal.say(&mode_line_of(&mode));
    Ok(())
}

/// Edits the mode `id` as `json` holds it; see [`new_mode`].
async fn edit_mode<S: SessionStore>(
    client: &Client<S>,
    id: ModeId,
    json: &str,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let Ok(request) = serde_json::from_str::<UpdateModeRequest>(json) else {
        terminal.say(
            "not a mode's JSON: {\"version\", \"name\", \"visibility\", \"steps\"}, as the README shows; nothing was sent",
        );
        return Ok(());
    };
    let mode = match client.update_mode(id, &request).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::ModeNeedsStepUp) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.update_mode_with_commands(id, &request).await?
        }
        other => other?,
    };
    terminal.say(&mode_line_of(&mode));
    Ok(())
}

/// Deletes the mode `id`, with a step-up when the server asks, once.
async fn delete_mode<S: SessionStore>(
    client: &Client<S>,
    id: ModeId,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    match client.delete_mode(id).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.delete_mode(id).await?;
        }
        other => other?,
    }
    terminal.say(&format!("mode {id} deleted"));
    Ok(())
}

/// One mode as a line: `mode <id> <name> <kind>`, then ` mine` and
/// ` editable` when they hold, and ` version <n>`, which `edit-mode` sends
/// back; never its steps.
fn mode_line_of(mode: &ModeResponse) -> String {
    let kind = match (mode.ownership.known(), mode.visibility.known()) {
        (Some(ModeOwnership::BuiltIn), _) => "built-in",
        (_, Some(ModeVisibility::Private)) => "private",
        (_, Some(ModeVisibility::Shared)) => "shared",
        (_, None) => mode.visibility.as_str(),
    };
    let mut shown = format!("mode {} {} {kind}", mode.id, mode.name);
    if mode.ownership.known() == Some(ModeOwnership::Mine) {
        shown.push_str(" mine");
    }
    if mode.editable {
        shown.push_str(" editable");
    }
    let _ = write!(shown, " version {}", mode.version);
    shown
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

/// How long the live events socket's handshake may take.
const EVENTS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the console's own close may take, after Enter.
const EVENTS_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// `events`: the live events (P11.5; ADR-0020, S37), one line each, until
/// Enter; when the server closes the socket first, its code and the
/// approved reason, then Enter. Only over dev-server's plain http: the
/// console's WebSocket client has no TLS until the app's own (P11.8). It
/// never prints the URL, the ticket, the token, or the client library's
/// error text, which could quote them.
pub(crate) async fn events<S: SessionStore>(
    client: &Client<S>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    if client.server().as_str().starts_with("https:") {
        terminal.say("events need dev-server's plain http until the app's own client (P11.8)");
        return Ok(());
    }
    let handshake = client.events_handshake().await?;
    let opened = open_events(&handshake).await;
    drop(handshake);
    let socket = match opened {
        Ok(socket) => socket,
        Err(said) => {
            terminal.say(&said);
            return Ok(());
        }
    };
    terminal.say("events: press Enter to stop");
    let enter = terminal.enter();
    watch_events(socket, enter, terminal).await;
    Ok(())
}

/// The live events socket, as the console reads it.
type EventsSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the socket with `handshake`; on a failure, what the console says:
/// the server's status and code at most.
async fn open_events(handshake: &EventsHandshake) -> Result<EventsSocket, String> {
    const COULDNT: &str = "the events socket couldn't open";
    let mut request = handshake
        .url()
        .expose_secret()
        .into_client_request()
        .map_err(|_| COULDNT.to_owned())?;
    let mut bearer = HeaderValue::from_str(handshake.authorization().expose_secret())
        .map_err(|_| COULDNT.to_owned())?;
    bearer.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, bearer);
    match tokio::time::timeout(EVENTS_CONNECT_TIMEOUT, connect_async(request)).await {
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(WsError::Http(response))) => {
            let code = response
                .body()
                .as_deref()
                .and_then(|body| serde_json::from_slice::<ErrorResponse>(body).ok())
                .map(|error| error.error.code.as_str().to_owned())
                .unwrap_or_default();
            Err(format!(
                "the events socket was refused: {} {code}",
                response.status().as_u16()
            ))
        }
        Ok(Err(_)) | Err(_) => Err(COULDNT.to_owned()),
    }
}

/// Prints each event of `socket` until `enter`, or until the server closes
/// it, then waits for `enter`.
async fn watch_events(
    mut socket: EventsSocket,
    mut enter: Pin<Box<dyn Future<Output = io::Result<()>> + Send>>,
    terminal: &mut impl Terminal,
) {
    loop {
        tokio::select! {
            // Both are cancel-safe: `next` loses no frame, and Enter's
            // future is kept across turns.
            _ = &mut enter => {
                let _ = tokio::time::timeout(EVENTS_CLOSE_TIMEOUT, socket.close(None)).await;
                terminal.say("events stopped");
                return;
            }
            frame = socket.next() => match frame {
                Some(Ok(Message::Close(frame))) => {
                    terminal.say(&closed_line(frame.map(|frame| u16::from(frame.code))));
                    break;
                }
                Some(Ok(frame)) => {
                    if let Some(line) = frame_line(&frame) {
                        terminal.say(&line);
                    }
                }
                Some(Err(_)) | None => {
                    terminal.say("the events socket ended");
                    break;
                }
            },
        }
    }
    terminal.say("press Enter");
    let _ = enter.await;
}

/// What the console prints for a frame other than a close: an event's line,
/// a binary frame read as one that doesn't read (a resync, the protocol's
/// second rule), and nothing for a ping or a pong, which tungstenite
/// answers by itself.
pub(crate) fn frame_line(frame: &Message) -> Option<String> {
    match frame {
        Message::Text(text) => Some(event_line(&read(text))),
        Message::Binary(_) => Some(event_line(&Received::Unreadable)),
        _ => None,
    }
}

/// One event, as the console shows it: a bot in `bots`' line format, a chat
/// line in `history`'s, a mode in `modes`' (never its steps), each named
/// by its bot or mode.
pub(crate) fn event_line(received: &Received) -> String {
    match received {
        Received::Event(event) => event.kind.known().map_or_else(
            || "an event this console doesn't know".to_owned(),
            |kind| known_event_line(kind, event),
        ),
        Received::Ignored => "an event this console doesn't know".to_owned(),
        Received::Unreadable => "an event that doesn't read: taken for a resync".to_owned(),
    }
}

/// `event`, of the known `kind`, as one line. [`Received::Event`] holds only
/// events with every field their kind carries, so the fields read here are
/// there; one missing anyway shows as `?`.
fn known_event_line(kind: EventMessageKind, event: &EventMessage) -> String {
    let bot = event
        .bot_id
        .map_or_else(|| "?".to_owned(), |id| id.to_string());
    let request = event
        .request
        .map_or_else(|| "?".to_owned(), |id| id.to_string());
    let at = event
        .at
        .as_ref()
        .map_or_else(|| "?".to_owned(), ToString::to_string);
    match kind {
        EventMessageKind::Ready => "ready".to_owned(),
        EventMessageKind::Resync => "resync: fetch again".to_owned(),
        EventMessageKind::Bot => event
            .bot
            .as_ref()
            .map_or_else(|| format!("bot {bot}"), line_of),
        EventMessageKind::BotRemoved => format!("bot {bot} removed"),
        EventMessageKind::BotDied => format!("bot {bot} died at {at}"),
        EventMessageKind::BotAlert => {
            let state = event.state.as_ref().map_or("?", |state| state.as_str());
            let reason = event.reason.as_ref().map_or("?", |reason| reason.as_str());
            format!("bot {bot} {state} reason={reason} at {at}")
        }
        EventMessageKind::Chat => {
            let mut shown = format!("bot {bot} ");
            if let Some(line) = &event.line {
                shown.push_str(&chat_line_of(line));
            }
            if let Some(request) = event.request {
                let _ = write!(shown, " (request {request})");
            }
            shown
        }
        EventMessageKind::ChatSent => format!("bot {bot} request {request} sent"),
        EventMessageKind::ChatFailed => match &event.failure {
            Some(failure) => format!("bot {bot} request {request} failed: {}", failure.as_str()),
            None => format!("bot {bot} request {request} failed"),
        },
        EventMessageKind::Mode => event.mode.as_ref().map_or_else(
            || {
                let id = event
                    .mode_id
                    .map_or_else(|| "?".to_owned(), |id| id.to_string());
                format!("mode {id}")
            },
            mode_line_of,
        ),
        EventMessageKind::ModeRemoved => {
            let id = event
                .mode_id
                .map_or_else(|| "?".to_owned(), |id| id.to_string());
            format!("mode {id} removed")
        }
    }
}

/// The server's close as the console shows it: the code, and the reason
/// this version knows for it, never the text the server sent.
pub(crate) fn closed_line(code: Option<u16>) -> String {
    match code {
        Some(code) => match SocketClose::of_code(code) {
            Some(close) => format!(
                "the server closed the events socket: {code} {}",
                close.reason()
            ),
            None => format!("the server closed the events socket: {code}"),
        },
        None => "the server closed the events socket".to_owned(),
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

    /// A line without text is invalid input, never an empty line sent.
    #[test]
    fn a_chat_or_command_line_without_text_is_not_a_command() {
        let id = "018bcfe5-6800-7bab-abab-abababababab";

        for verb in ["chat", "command"] {
            for line in [
                format!("{verb} {id}"),
                format!("{verb} {id} "),
                format!("{verb} {id}    "),
                format!("{verb} {id} \t "),
            ] {
                assert_eq!(Command::parse(&line), None, "{line:?}");
            }
        }
    }

    #[test]
    fn history_takes_a_bot_and_maybe_the_line_to_page_before() {
        let id = "018bcfe5-6800-7bab-abab-abababababab";
        let bot: BotId = id.parse().unwrap();

        assert_eq!(
            Command::parse(&format!("history {id}")),
            Some(Command::History(bot, None))
        );
        assert_eq!(
            Command::parse(&format!(" history {id} 41 ")),
            Some(Command::History(bot, Some(SafeInt::try_new(41).unwrap())))
        );
        assert_eq!(Command::parse(&format!("history {id} x")), None);
        assert_eq!(Command::parse(&format!("history {id} 1 2")), None);
        assert_eq!(Command::parse("history"), None);
        assert_eq!(Command::parse("history not-an-id"), None);
    }

    fn a_line(kind: &str, text: &str) -> ChatLine {
        serde_json::from_value(serde_json::json!({
            "id": 41,
            "at": "2027-01-15T08:00:01.000Z",
            "direction": "in",
            "kind": kind,
            "text": text,
            "truncated": false,
            "arguments_hidden": false,
            "mine": false,
        }))
        .unwrap()
    }

    #[test]
    fn a_chat_line_shows_its_id_time_kind_sender_text_and_marks() {
        let mut steve = a_line("chat", "hello bot");
        steve.sender = serde_json::from_value(serde_json::json!({ "name": "Steve" })).unwrap();
        let mut cut = a_line("whisper", "a whisper cut");
        cut.sender = serde_json::from_value(serde_json::json!({ "name": "Alex" })).unwrap();
        cut.truncated = true;
        let mut mode = a_line("mode", "/login");
        mode.arguments_hidden = true;
        let mut by = a_line("user", "hello there");
        by.sent_by = serde_json::from_value(serde_json::json!({
            "id": "018bcfe5-6800-7bab-abab-abababababab",
            "username": "manager",
        }))
        .unwrap();
        let mut mine = a_line("user", "/spawn");
        mine.mine = true;
        mine.sent_by = by.sent_by.clone();
        let unnamed = a_line("user", "hello again");
        let newer = a_line("shout", "a kind this console doesn't know");
        let mut emote = a_line("emote", "waves");
        emote.sender = serde_json::from_value(serde_json::json!({ "name": "Alex" })).unwrap();
        let mut announced = a_line("announcement", "restart soon");
        announced.sender = serde_json::from_value(serde_json::json!({ "name": "Server" })).unwrap();
        let system = a_line("system", "Steve joined the game");

        let shown: Vec<String> = [
            steve, cut, mode, by, mine, unnamed, newer, emote, announced, system,
        ]
        .iter()
        .map(chat_line_of)
        .collect();

        let at = "#41 2027-01-15T08:00:01.000Z";
        assert_eq!(
            shown,
            [
                format!("{at} chat Steve: hello bot"),
                format!("{at} whisper Alex: a whisper cut (cut)"),
                format!("{at} mode: /login (arguments hidden)"),
                format!("{at} user by manager: hello there"),
                format!("{at} user by you: /spawn"),
                format!("{at} user: hello again"),
                format!("{at} shout: a kind this console doesn't know"),
                format!("{at} emote Alex: waves"),
                format!("{at} announcement Server: restart soon"),
                format!("{at} system: Steve joined the game"),
            ]
        );
    }

    #[test]
    fn the_mode_verbs_take_their_ids_and_new_mode_the_rest_of_the_line() {
        let id = "018bcfe5-6800-7bab-abab-abababababab";
        let bot: BotId = id.parse().unwrap();
        let farm = "01a14202-2800-7000-8000-000000000002";
        let mode: ModeId = farm.parse().unwrap();
        let json = r#"{"name": "My mode", "steps": []}"#;

        assert_eq!(Command::parse("modes"), Some(Command::Modes));
        assert_eq!(
            Command::parse(&format!("mode {id} {farm}")),
            Some(Command::SetMode(bot, mode))
        );
        assert_eq!(
            Command::parse(&format!("new-mode {json}")),
            Some(Command::NewMode(json.to_owned()))
        );
        assert_eq!(Command::parse("new-mode"), None);
        assert_eq!(Command::parse("new-mode   "), None);
        assert_eq!(
            Command::parse(&format!("edit-mode {farm} {json}")),
            Some(Command::EditMode(mode, json.to_owned()))
        );
        assert_eq!(
            Command::parse(&format!("delete-mode {farm}")),
            Some(Command::DeleteMode(mode))
        );
        assert_eq!(Command::parse(&format!("edit-mode {farm}")), None);
        assert_eq!(Command::parse(&format!("edit-mode {farm}   ")), None);
        assert_eq!(
            Command::parse(&format!("edit-mode not-a-mode {json}")),
            None
        );
        assert_eq!(Command::parse("delete-mode not-a-mode"), None);
        assert_eq!(Command::parse(&format!("delete-mode {farm} now")), None);
        assert_eq!(Command::parse(&format!("mode {id}")), None);
        assert_eq!(Command::parse(&format!("mode {id} not-a-mode")), None);
        assert_eq!(Command::parse("modes now"), None);
    }

    fn a_mode(ownership: &str, visibility: &str, editable: bool) -> ModeResponse {
        a_mode_at(ownership, visibility, editable, 1)
    }

    fn a_mode_at(ownership: &str, visibility: &str, editable: bool, version: u32) -> ModeResponse {
        serde_json::from_value(serde_json::json!({
            "id": "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd",
            "name": "Fishing trip",
            "visibility": visibility,
            "ownership": ownership,
            "editable": editable,
            "deletable": editable,
            "visibility_editable": false,
            "version": version,
            "created_at": "2027-01-15T08:00:00.000Z",
            "updated_at": "2027-01-15T08:00:00.000Z",
            "steps": [{ "action": { "type": "send_chat", "message": "/login hunter2", "arguments_hidden": false }, "schedule": { "type": "at_start" }, "probability": 100 }],
        }))
        .unwrap()
    }

    #[test]
    fn a_mode_line_shows_its_id_name_kind_and_marks_never_its_steps() {
        let shown: Vec<String> = [
            a_mode("built_in", "shared", false),
            a_mode("mine", "private", true),
            a_mode("mine", "shared", false),
            a_mode("someone_else", "shared", true),
            a_mode("someone_else", "public", false),
            a_mode_at("mine", "private", true, 4_294_967_295),
        ]
        .iter()
        .map(mode_line_of)
        .collect();

        let id = "mode 018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd Fishing trip";
        assert_eq!(
            shown,
            [
                format!("{id} built-in version 1"),
                format!("{id} private mine editable version 1"),
                format!("{id} shared mine version 1"),
                format!("{id} shared editable version 1"),
                format!("{id} public version 1"),
                format!("{id} private mine editable version 4294967295"),
            ]
        );
        assert!(shown.iter().all(|line| !line.contains("hunter2")));
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
