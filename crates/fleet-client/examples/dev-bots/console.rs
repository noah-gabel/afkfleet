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
//! `events` prints the live events, one line each, until Enter, over
//! fleet-client's own socket, which reconnects by itself and keeps its
//! ticket and token (P11.5, P11.8; ADR-0020, S37, S53). `audit` shows a page
//! of the audit log, `users` every user and `agents` every agent, for Admins
//! and the Owner; `release` releases a disabled agent's bots, asking for the
//! password again when the server wants it; `held` and `shared` show the
//! review of a user's grants at a role change, the Owner's; `leave` gives up
//! the user's own grant on a shared account, asking for the password again
//! when the server wants it (P11.9; ADR-0020, R67, S51). `servers` shows
//! every server's settings, a line each, for Admins and the Owner;
//! `new-server` and `edit-server` take their JSON, `delete-server` an ID,
//! each asking for the password again when the server wants it; a line
//! shows a server's key and how many conflict texts and chat formats it
//! has, never a text or a format, and JSON that isn't server settings is
//! said so, never repeated (P11.9; ADR-0020, S77). `preview-formats` takes
//! a preview's JSON and says which format reads which sender from its
//! line, counted from 1, never printing the line back (U58, R143).

use core::fmt::Write as _;
use core::pin::Pin;
use std::io;

use fleet_api_types::accounts::{CreateOfflineAccountRequest, HeldGrantResponse};
use fleet_api_types::agents::AgentResponse;
use fleet_api_types::audit::{AuditEntryResponse, AuditPage, AuditPageQuery};
use fleet_api_types::auth::{LoginRequest, StepUpRequest};
use fleet_api_types::bots::{BotModeRequest, BotPatchRequest, BotResponse, BotStatus};
use fleet_api_types::chat::{
    ChatCommandRequest, ChatLine, ChatPageQuery, ChatSendOutcome, ChatSendRequest, ChatSendResponse,
};
use fleet_api_types::events::{EventMessage, EventMessageKind};
use fleet_api_types::ids::AgentId;
use fleet_api_types::modes::{
    CreateModeRequest, ModeOwnership, ModeResponse, ModeVisibility, UpdateModeRequest,
};
use fleet_api_types::server_settings::{
    CreateServerSettingsRequest, PreviewChatFormatsRequest, ServerSettingsPageQuery,
    ServerSettingsResponse, UpdateServerSettingsRequest,
};
use fleet_api_types::{
    AccountId, BotId, ErrorCode, ModeId, PageQuery, SafeInt, SecretText, ServerSettingsId, UserId,
};
use fleet_client::events::{Received, SocketClose};
use fleet_client::{
    Client, ClientError, Disconnect, LiveEnd, LiveUpdate, LoginStep, RefetchReason, SessionStore,
};
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
pub(crate) const HELP: &str = "commands: bots | bot <id> | offline <name> | server <id> <address> | mode <id> <mode> | start <id> | stop <id> | chat <id> <text> | command <id> <text> | history <id> [before] | modes | new-mode <json> | edit-mode <id> <json> | delete-mode <id> | events | users | audit [before] | agents | release <agent-id> | held <user-id> [before] | shared <user-id> [before] | leave <account-id> | servers | new-server <json> | edit-server <id> <json> | delete-server <id> | preview-formats <json> | quit";

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
    /// Every user, for Admins and the Owner.
    Users,
    /// One page of the audit log: the newest, or the one before an entry.
    Audit(Option<SafeInt>),
    /// Every agent, for Admins and the Owner.
    Agents,
    /// A disabled agent's bots released.
    Release(AgentId),
    /// One page of the grants a user holds: the newest, or the one before
    /// an account.
    Held(UserId, Option<AccountId>),
    /// One page of a user's accounts that a grant shares.
    Shared(UserId, Option<AccountId>),
    /// The user's own grant on an account given up.
    Leave(AccountId),
    /// Every server's settings, for Admins and the Owner.
    Servers,
    /// A server's new settings: the rest of the line, as typed, their JSON.
    NewServer(String),
    /// A server's settings edited: the rest of the line, as typed, their
    /// JSON with the version read.
    EditServer(ServerSettingsId, String),
    /// A server's settings deleted.
    DeleteServer(ServerSettingsId),
    /// Which sender chat formats read from a line: the rest of the line, as
    /// typed, a preview's JSON.
    PreviewFormats(String),
    Quit,
}

impl Command {
    /// `line` as a command; none for anything else.
    fn parse(line: &str) -> Option<Self> {
        let bot = |text: &str| text.parse::<BotId>().ok();
        let user = |text: &str| text.parse::<UserId>().ok();
        // A line's text is the rest of the line, its spaces kept; a line
        // without text is no command, never an empty line sent.
        if let Some((verb, id, text)) =
            with_text(line).filter(|(_, _, text)| !text.trim().is_empty())
        {
            match verb {
                "chat" => return bot(id).map(|id| Self::Chat(id, text.to_owned())),
                "command" => return bot(id).map(|id| Self::Slash(id, text.to_owned())),
                "edit-server" => {
                    return id
                        .parse::<ServerSettingsId>()
                        .ok()
                        .map(|id| Self::EditServer(id, text.to_owned()));
                }
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
        // A new mode's or server's JSON, or a preview's, is the rest of the
        // line, as typed.
        match line
            .trim_start()
            .split_once(char::is_whitespace)
            .filter(|(_, json)| !json.trim().is_empty())
        {
            Some(("new-mode", json)) => return Some(Self::NewMode(json.to_owned())),
            Some(("new-server", json)) => return Some(Self::NewServer(json.to_owned())),
            Some(("preview-formats", json)) => {
                return Some(Self::PreviewFormats(json.to_owned()));
            }
            _ => {}
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["modes"] => Some(Self::Modes),
            ["servers"] => Some(Self::Servers),
            ["delete-server", id] => id.parse::<ServerSettingsId>().ok().map(Self::DeleteServer),
            ["events"] => Some(Self::Events),
            ["users"] => Some(Self::Users),
            ["agents"] => Some(Self::Agents),
            ["held", id] => user(id).map(|id| Self::Held(id, None)),
            ["held", id, before] => {
                let before = before.parse::<AccountId>().ok()?;
                user(id).map(|id| Self::Held(id, Some(before)))
            }
            ["shared", id] => user(id).map(|id| Self::Shared(id, None)),
            ["shared", id, before] => {
                let before = before.parse::<AccountId>().ok()?;
                user(id).map(|id| Self::Shared(id, Some(before)))
            }
            ["release", id] => id.parse::<AgentId>().ok().map(Self::Release),
            ["leave", id] => id.parse::<AccountId>().ok().map(Self::Leave),
            ["audit"] => Some(Self::Audit(None)),
            ["audit", before] => {
                let before = before.parse::<i64>().ok()?;
                SafeInt::try_new(before)
                    .ok()
                    .map(|before| Self::Audit(Some(before)))
            }
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
        Command::Events => {
            events(client, terminal).await;
            Ok(())
        }
        Command::Users => users(client, terminal).await,
        Command::Audit(before) => audit(client, before, terminal).await,
        Command::Agents => agents(client, terminal).await,
        Command::Release(id) => release(client, id, terminal).await,
        Command::Held(id, before) => held(client, id, before, terminal).await,
        Command::Shared(id, before) => shared(client, id, before, terminal).await,
        Command::Leave(id) => leave(client, id, terminal).await,
        Command::Servers => servers(client, terminal).await,
        Command::NewServer(json) => new_server(client, &json, terminal).await,
        Command::EditServer(id, json) => edit_server(client, id, &json, terminal).await,
        Command::DeleteServer(id) => delete_server(client, id, terminal).await,
        Command::PreviewFormats(json) => preview_formats(client, &json, terminal).await,
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

/// Every page of the users, one line each: the ID, the name, the role, and
/// whether they're disabled.
async fn users<S: SessionStore>(
    client: &Client<S>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let mut before = None;
    loop {
        let page = client
            .users(&PageQuery {
                before,
                limit: None,
            })
            .await?;
        for user in &page.items {
            let disabled = if user.disabled { " (disabled)" } else { "" };
            terminal.say(&format!(
                "{} {} {}{disabled}",
                user.id,
                user.username,
                user.role.as_str()
            ));
        }
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    Ok(())
}

/// Every page of the agents, one line each.
async fn agents<S: SessionStore>(
    client: &Client<S>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let mut before = None;
    let mut any = false;
    loop {
        let page = client
            .agents(&PageQuery {
                before,
                limit: None,
            })
            .await?;
        for agent in &page.items {
            terminal.say(&agent_line_of(agent));
            any = true;
        }
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    if !any {
        terminal.say("no agents yet");
    }
    Ok(())
}

/// One agent: its ID, name and status, and how many bots it holds.
fn agent_line_of(agent: &AgentResponse) -> String {
    format!(
        "{} {} {} bots={}",
        agent.id,
        agent.name,
        agent.status.as_str(),
        agent.bots
    )
}

/// Releases the disabled agent `id`'s bots, with a step-up when the server
/// asks, once.
async fn release<S: SessionStore>(
    client: &Client<S>,
    id: AgentId,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let agent = match client.release_agent(id).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.release_agent(id).await?
        }
        other => other?,
    };
    terminal.say(&agent_line_of(&agent));
    Ok(())
}

/// Gives up the user's own grant on account `id`, with a step-up when the
/// server asks, once.
async fn leave<S: SessionStore>(
    client: &Client<S>,
    id: AccountId,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    match client.leave_account(id).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.leave_account(id).await?;
        }
        other => other?,
    }
    terminal.say(&format!("left account {id}"));
    Ok(())
}

/// One page of the grants user `id` holds, the one before `before` or the
/// newest, newest account first, and where the next page starts.
async fn held<S: SessionStore>(
    client: &Client<S>,
    id: UserId,
    before: Option<AccountId>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let page = client
        .user_grants(
            id,
            &PageQuery {
                before,
                limit: None,
            },
        )
        .await?;
    if page.items.is_empty() {
        terminal.say("holds no grants");
    }
    for grant in &page.items {
        terminal.say(&held_line_of(grant));
    }
    if let Some(next) = page.next {
        terminal.say(&format!("older: held {id} {next}"));
    }
    Ok(())
}

/// One held grant: the account, its owner, the level, who set it, and
/// whether it's dormant.
fn held_line_of(grant: &HeldGrantResponse) -> String {
    let dormant = if grant.dormant { " (dormant)" } else { "" };
    format!(
        "{} {} of {} {} by {}{dormant}",
        grant.account.id,
        grant.account.name,
        grant.owner.username,
        grant.level.as_str(),
        grant.granted_by.username
    )
}

/// One page of user `id`'s accounts that a grant shares, the one before
/// `before` or the newest, and where the next page starts.
async fn shared<S: SessionStore>(
    client: &Client<S>,
    id: UserId,
    before: Option<AccountId>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let page = client
        .shared_accounts(
            id,
            &PageQuery {
                before,
                limit: None,
            },
        )
        .await?;
    if page.items.is_empty() {
        terminal.say("shares no accounts");
    }
    for account in &page.items {
        terminal.say(&format!(
            "{} {} {} {}",
            account.id,
            account.name,
            account.kind.as_str(),
            account.status.as_str()
        ));
    }
    if let Some(next) = page.next {
        terminal.say(&format!("older: shared {id} {next}"));
    }
    Ok(())
}

/// One page of the audit log, the one before `before` or the newest,
/// oldest first, and where the older entries start.
async fn audit<S: SessionStore>(
    client: &Client<S>,
    before: Option<SafeInt>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let page = client
        .audit(&AuditPageQuery {
            before,
            limit: None,
            target_kind: None,
            target_id: None,
        })
        .await?;
    for line in audit_lines_of(&page) {
        terminal.say(&line);
    }
    if let Some(next) = page.next {
        terminal.say(&format!("older: audit {next}"));
    }
    Ok(())
}

/// A line for each entry of `page`, oldest first: each that reads, and each
/// that doesn't by its ID alone, in its place (ADR-0020, S52); one line
/// saying so for an empty page.
fn audit_lines_of(page: &AuditPage) -> Vec<String> {
    let mut lines: Vec<(i64, String)> = page
        .items
        .iter()
        .map(|entry| (entry.id.get(), audit_line_of(entry)))
        .chain(
            page.unreadable
                .iter()
                .map(|id| (id.get(), format!("#{id} unreadable"))),
        )
        .collect();
    if lines.is_empty() {
        return vec!["no audit entries".to_owned()];
    }
    lines.sort_unstable_by_key(|(id, _)| *id);
    lines.into_iter().map(|(_, line)| line).collect()
}

/// One audit entry: its ID, time, action and outcome; who, from where and
/// on what, when it says; and its details, as the server sends them.
fn audit_line_of(entry: &AuditEntryResponse) -> String {
    let mut shown = format!(
        "#{} {} {} {}",
        entry.id,
        entry.at,
        entry.action.as_str(),
        entry.outcome.as_str()
    );
    if let Some(actor) = entry.actor {
        let _ = write!(shown, " by {actor}");
    }
    if let Some(ip) = &entry.ip {
        let _ = write!(shown, " from {ip}");
    }
    if let Some(target) = &entry.target {
        let _ = write!(shown, " on {} {}", target.kind.as_str(), target.id);
    }
    let details: Vec<String> = entry
        .metadata
        .iter()
        .map(|detail| {
            let value = match (&detail.text, detail.int, detail.flag) {
                (Some(text), _, _) => text.to_string(),
                (None, Some(int), _) => int.to_string(),
                (None, None, Some(flag)) => flag.to_string(),
                (None, None, None) => "?".to_owned(),
            };
            format!("{}={value}", detail.key)
        })
        .collect();
    if !details.is_empty() {
        let _ = write!(shown, ": {}", details.join(", "));
    }
    shown
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

/// One chat line: its ID, time and kind; the name a chat format read, right
/// after the kind and never where a sender goes (ADR-0020, R143); who sent
/// it (a received line's Minecraft sender, or the user who had it sent, as
/// the server shows them); its text; and whether arguments were left out or
/// the text cut.
fn chat_line_of(line: &ChatLine) -> String {
    let parsed = line
        .parsed_sender
        .as_ref()
        .map(|name| format!(" (format: {name})"))
        .unwrap_or_default();
    let who = match (&line.sender, &line.sent_by) {
        (Some(sender), _) => format!(" {}", sender.name),
        (None, _) if line.mine => " by you".to_owned(),
        (None, Some(by)) => format!(" by {}", by.username),
        (None, None) => String::new(),
    };
    let mut shown = format!(
        "#{} {} {}{parsed}{who}: {}",
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

/// Every page of the servers' settings, a line each: pages of the
/// default 10, so at most 10 pages.
async fn servers<S: SessionStore>(
    client: &Client<S>,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let mut before = None;
    loop {
        let page = client
            .server_settings(&ServerSettingsPageQuery {
                before,
                limit: None,
            })
            .await?;
        for settings in &page.items {
            terminal.say(&server_line_of(settings));
        }
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    Ok(())
}

/// Creates the server settings `json` holds, with a step-up when the
/// server asks, once. JSON that isn't server settings is said so, never
/// repeated: serde's message would quote it.
async fn new_server<S: SessionStore>(
    client: &Client<S>,
    json: &str,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let Ok(request) = serde_json::from_str::<CreateServerSettingsRequest>(json) else {
        terminal.say(
            "not server settings' JSON: {\"server\", \"conflict_texts\", \"chat_formats\"}, as the README shows; nothing was sent",
        );
        return Ok(());
    };
    let settings = match client.create_server_settings(&request).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.create_server_settings(&request).await?
        }
        other => other?,
    };
    terminal.say(&server_line_of(&settings));
    Ok(())
}

/// Edits the server settings `id` as `json` holds them; see [`new_server`].
async fn edit_server<S: SessionStore>(
    client: &Client<S>,
    id: ServerSettingsId,
    json: &str,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let Ok(request) = serde_json::from_str::<UpdateServerSettingsRequest>(json) else {
        terminal.say(
            "not server settings' JSON: {\"version\", \"server\", \"conflict_texts\", \"chat_formats\"}, as the README shows; nothing was sent",
        );
        return Ok(());
    };
    let settings = match client.update_server_settings(id, &request).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.update_server_settings(id, &request).await?
        }
        other => other?,
    };
    terminal.say(&server_line_of(&settings));
    Ok(())
}

/// Deletes the server settings `id`, with a step-up when the server asks,
/// once.
async fn delete_server<S: SessionStore>(
    client: &Client<S>,
    id: ServerSettingsId,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    match client.delete_server_settings(id).await {
        Err(ClientError::Api(failure)) if failure.is(ErrorCode::StepUpRequired) => {
            if !stepped_up(client, terminal).await? {
                return Ok(());
            }
            client.delete_server_settings(id).await?;
        }
        other => other?,
    }
    terminal.say(&format!("server settings {id} deleted"));
    Ok(())
}

/// Which format reads which sender from the line `json` holds, counted
/// from 1 as the app shows it; never the line, which can hold anything a
/// server sends. JSON that isn't a preview's is said so, never repeated.
async fn preview_formats<S: SessionStore>(
    client: &Client<S>,
    json: &str,
    terminal: &mut impl Terminal,
) -> Result<(), ClientError> {
    let Ok(request) = serde_json::from_str::<PreviewChatFormatsRequest>(json) else {
        terminal.say(
            "not a preview's JSON: {\"chat_formats\", \"line\"}, as the README shows; nothing was sent",
        );
        return Ok(());
    };
    let found = client.preview_chat_formats(&request).await?;
    match (found.format, found.sender) {
        (Some(format), Some(sender)) => {
            terminal.say(&format!("format {} reads {sender}", u64::from(format) + 1));
        }
        _ => terminal.say("no format fits"),
    }
    Ok(())
}

/// One server's settings as a line: `server <id> <key> texts <n> formats
/// <m> version <v>`, the version `edit-server` sends back; never a text or
/// a format.
fn server_line_of(settings: &ServerSettingsResponse) -> String {
    format!(
        "server {} {} texts {} formats {} version {}",
        settings.id,
        settings.server,
        settings.conflict_texts.len(),
        settings.chat_formats.len(),
        settings.version
    )
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

/// `events`: the live events (P11.5, P11.8; ADR-0020, S37, S53), one line
/// each, until Enter, over fleet-client's own socket, which reconnects as
/// the app's does: what to fetch again, each event, and each disconnect with
/// its wait. It ends by itself only with the session or at a refused
/// certificate, then waits for Enter. The ticket and the token never leave
/// the client, so it can't print them.
pub(crate) async fn events<S: SessionStore>(client: &Client<S>, terminal: &mut impl Terminal) {
    let mut live = client.live_events();
    terminal.say("events: press Enter to stop");
    let mut enter = terminal.enter();
    loop {
        tokio::select! {
            // Both are cancel-safe: `recv` is, and Enter's future is kept
            // across turns.
            _ = &mut enter => {
                // A session's end decided before Enter stands: its close
                // may still have been running in the `recv` this dropped.
                let end = live.close().await;
                terminal.say(end_line(end));
                return;
            }
            update = live.recv() => match update {
                Ok(update) => terminal.say(&update_line(update)),
                Err(end) => {
                    terminal.say(end_line(end));
                    break;
                }
            },
        }
    }
    terminal.say("press Enter");
    let _ = enter.await;
}

/// One update of the live events socket, as the console shows it.
pub(crate) fn update_line(update: LiveUpdate) -> String {
    match update {
        LiveUpdate::Refetch(reason) => {
            let reason = match reason {
                RefetchReason::Ready => "ready",
                RefetchReason::Resync => "resync",
                RefetchReason::Unreadable => "unreadable",
            };
            format!("fetch everything ({reason})")
        }
        LiveUpdate::Event(event) => event_line(&Received::Event(event)),
        LiveUpdate::Disconnected { why, retry_in } => {
            let why = match why {
                Disconnect::Closed { code } => closed_line(code),
                Disconnect::Lost => "the events socket was lost".to_owned(),
                Disconnect::Silent => "the events socket went silent".to_owned(),
                Disconnect::NotOpened(ClientError::Api(failure)) => format!(
                    "the events socket was refused: {} {}",
                    failure.status,
                    failure.code.as_str()
                ),
                Disconnect::NotOpened(error) => {
                    format!("the events socket couldn't open: {error}")
                }
            };
            let seconds = retry_in.as_millis().div_ceil(1000);
            format!("{why} (reconnecting in {seconds} s)")
        }
    }
}

/// How the live events socket ended, as the console says it.
pub(crate) const fn end_line(end: LiveEnd) -> &'static str {
    match end {
        LiveEnd::Stopped => "events stopped",
        LiveEnd::NoSession => "the session ended",
        LiveEnd::Certificate => "the server's certificate was refused",
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
    fn held_and_shared_take_a_user_and_maybe_the_account_to_page_before() {
        let user = "018bcfe5-6800-7303-8303-030303030303";
        let account = "018bcfe5-6800-7404-8404-040404040404";
        let (user_id, account_id): (UserId, AccountId) =
            (user.parse().unwrap(), account.parse().unwrap());

        assert_eq!(
            Command::parse(&format!("held {user}")),
            Some(Command::Held(user_id, None))
        );
        assert_eq!(
            Command::parse(&format!(" shared {user} {account} ")),
            Some(Command::Shared(user_id, Some(account_id)))
        );
        assert_eq!(Command::parse("held"), None);
        assert_eq!(Command::parse("shared not-an-id"), None);
        assert_eq!(Command::parse(&format!("held {user} x")), None);
    }

    #[test]
    fn a_held_line_names_the_account_its_owner_and_whether_its_dormant() {
        let grant: HeldGrantResponse = serde_json::from_value(serde_json::json!({
            "account": { "id": "018bcfe5-6800-7404-8404-040404040404", "name": "AfkBot4" },
            "owner": { "id": "018bcfe5-6800-7202-8202-020202020202", "username": "alice" },
            "level": "control",
            "granted_by": { "id": "018bcfe5-6800-7202-8202-020202020202", "username": "alice" },
            "granted_at": "2027-01-15T08:00:00.000Z",
            "dormant": true
        }))
        .unwrap();

        assert_eq!(
            held_line_of(&grant),
            "018bcfe5-6800-7404-8404-040404040404 AfkBot4 of alice control by alice (dormant)"
        );
    }

    #[test]
    fn leave_takes_one_account() {
        let id = "018bcfe5-6800-7404-8404-040404040404";

        assert_eq!(
            Command::parse(&format!(" leave {id} ")),
            Some(Command::Leave(id.parse().unwrap()))
        );
        assert_eq!(Command::parse("leave"), None);
        assert_eq!(Command::parse("leave not-an-id"), None);
        assert_eq!(Command::parse(&format!("leave {id} now")), None);
    }

    #[test]
    fn release_takes_an_agent_and_agents_nothing() {
        let id = "018bcfe5-6800-7101-8101-010101010101";
        let agent: AgentId = id.parse().unwrap();

        assert_eq!(Command::parse("agents"), Some(Command::Agents));
        assert_eq!(
            Command::parse(&format!(" release {id} ")),
            Some(Command::Release(agent))
        );
        assert_eq!(Command::parse("release"), None);
        assert_eq!(Command::parse("release not-an-id"), None);
        assert_eq!(Command::parse(&format!("release {id} now")), None);
        assert_eq!(Command::parse("agents all"), None);
    }

    #[test]
    fn audit_takes_maybe_the_entry_to_page_before_and_users_nothing() {
        assert_eq!(Command::parse("audit"), Some(Command::Audit(None)));
        assert_eq!(
            Command::parse(" audit 41 "),
            Some(Command::Audit(Some(SafeInt::try_new(41).unwrap())))
        );
        assert_eq!(Command::parse("users"), Some(Command::Users));
        assert_eq!(Command::parse("audit x"), None);
        assert_eq!(Command::parse("audit 1 2"), None);
        assert_eq!(Command::parse("users all"), None);
    }

    #[test]
    fn an_audit_line_shows_who_from_where_on_what_and_its_details() {
        let entry: AuditEntryResponse = serde_json::from_value(serde_json::json!({
            "id": 41,
            "at": "2027-01-15T08:00:01.000Z",
            "actor": "018bcfe5-6800-7101-8101-010101010101",
            "ip": "192.0.2.10",
            "action": "bot.set_server",
            "target": { "kind": "bot", "id": "018bcfe5-6800-7202-8202-020202020202" },
            "outcome": "success",
            "metadata": [
                { "key": "server_host", "text": "play.example.com" },
                { "key": "server_port", "int": 25565 }
            ]
        }))
        .unwrap();
        let bare: AuditEntryResponse = serde_json::from_value(serde_json::json!({
            "id": 7,
            "at": "2027-01-15T08:00:02.000Z",
            "action": "retention.purge",
            "outcome": "success",
            "metadata": []
        }))
        .unwrap();

        assert_eq!(
            audit_line_of(&entry),
            "#41 2027-01-15T08:00:01.000Z bot.set_server success \
             by 018bcfe5-6800-7101-8101-010101010101 from 192.0.2.10 \
             on bot 018bcfe5-6800-7202-8202-020202020202: \
             server_host=play.example.com, server_port=25565"
        );
        assert_eq!(
            audit_line_of(&bare),
            "#7 2027-01-15T08:00:02.000Z retention.purge success"
        );
    }

    #[test]
    fn an_audit_page_shows_an_unreadable_entry_in_its_place_oldest_first() {
        let page: AuditPage = serde_json::from_value(serde_json::json!({
            "items": [
                {
                    "id": 9,
                    "at": "2027-01-15T08:00:03.000Z",
                    "action": "user.set_disabled",
                    "outcome": "success",
                    "metadata": [{ "key": "disabled", "flag": true }]
                },
                {
                    "id": 7,
                    "at": "2027-01-15T08:00:01.000Z",
                    "action": "retention.purge",
                    "outcome": "success",
                    "metadata": []
                }
            ],
            "unreadable": [8],
            "next": 7
        }))
        .unwrap();

        assert_eq!(
            audit_lines_of(&page),
            [
                "#7 2027-01-15T08:00:01.000Z retention.purge success",
                "#8 unreadable",
                "#9 2027-01-15T08:00:03.000Z user.set_disabled success: disabled=true",
            ]
        );
    }

    #[test]
    fn an_empty_audit_page_says_so() {
        let page: AuditPage =
            serde_json::from_value(serde_json::json!({ "items": [], "unreadable": [] })).unwrap();

        assert_eq!(audit_lines_of(&page), ["no audit entries"]);
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
        let mut parsed = a_line("system", "[Clan] Notch : hello");
        parsed.parsed_sender = Some(fleet_api_types::BoundedText::new("Notch"));

        let shown: Vec<String> = [
            steve, cut, mode, by, mine, unnamed, newer, emote, announced, system, parsed,
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
                format!("{at} system (format: Notch): [Clan] Notch : hello"),
            ]
        );
    }

    #[test]
    fn the_server_settings_verbs_take_their_ids_and_new_server_the_rest_of_the_line() {
        let id = "018bcfe5-6800-7bab-abab-abababababab";
        let settings: ServerSettingsId = id.parse().unwrap();
        let json = r#"{"server": "mc.example.com", "conflict_texts": ["One"], "chat_formats": []}"#;

        assert_eq!(Command::parse("servers"), Some(Command::Servers));
        assert_eq!(
            Command::parse(&format!("new-server {json}")),
            Some(Command::NewServer(json.to_owned()))
        );
        assert_eq!(Command::parse("new-server"), None);
        assert_eq!(
            Command::parse(r#"preview-formats {"chat_formats": [], "line": "a  b"}"#),
            Some(Command::PreviewFormats(
                r#"{"chat_formats": [], "line": "a  b"}"#.to_owned()
            ))
        );
        assert_eq!(Command::parse("preview-formats "), None);
        assert_eq!(Command::parse("new-server   "), None);
        assert_eq!(
            Command::parse(&format!("edit-server {id} {json}")),
            Some(Command::EditServer(settings, json.to_owned()))
        );
        assert_eq!(Command::parse(&format!("edit-server {id}")), None);
        assert_eq!(
            Command::parse(&format!("edit-server not-an-id {json}")),
            None
        );
        assert_eq!(
            Command::parse(&format!("delete-server {id}")),
            Some(Command::DeleteServer(settings))
        );
        assert_eq!(Command::parse("delete-server not-an-id"), None);
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
            "unlisted_command": false,
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
