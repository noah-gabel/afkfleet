//! The dev console (`examples/dev-bots`; ADR-0019, S71) against the
//! in-process server, its terminal scripted: the login, its refusals, the
//! commands, the step-up, and that nothing secret or unknown is ever shown.

use core::time::Duration;
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::{BotId, PageQuery, SecretText};
use fleet_client::{Client, ClientConfig, MemoryStore, ServerUrl, SessionStore, StoreError, Trust};
use fleet_testkit::system::ManualClock;
use secrecy::{ExposeSecret as _, SecretString};

use crate::dev_console::{self, DEV_SERVER, Ended, HELP, Terminal};
use crate::support::{OWNER, PASSWORD, Server};

/// A line of the script that moves the server's clock on by 31 days, past
/// the session's 30, before the next line is read.
const A_MONTH: &str = "<a month passes>";

/// A terminal that answers from its script and keeps what was said.
#[derive(Debug, Default)]
struct Scripted {
    lines: VecDeque<String>,
    secrets: VecDeque<String>,
    said: Vec<String>,
    /// The server's clock, for [`A_MONTH`].
    clock: Option<ManualClock>,
}

impl Scripted {
    fn new(secrets: &[&str], lines: &[&str]) -> Self {
        Self {
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            secrets: secrets.iter().map(|secret| (*secret).to_owned()).collect(),
            said: Vec::new(),
            clock: None,
        }
    }

    /// The script, with [`A_MONTH`] moving `clock`.
    fn with_clock(mut self, clock: ManualClock) -> Self {
        self.clock = Some(clock);
        self
    }

    fn text(&self) -> String {
        self.said.join("\n")
    }
}

impl Terminal for Scripted {
    fn line(&mut self, _prompt: &str) -> io::Result<Option<String>> {
        let line = self.lines.pop_front();
        if line.as_deref() == Some(A_MONTH) {
            self.clock
                .as_ref()
                .expect("a clock to move")
                .advance(Duration::from_hours(31 * 24));
            return Ok(self.lines.pop_front());
        }
        Ok(line)
    }

    fn secret(&mut self, _prompt: &str) -> io::Result<SecretString> {
        self.secrets
            .pop_front()
            .map(SecretString::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no terminal"))
    }

    fn say(&mut self, text: &str) {
        self.said.push(text.to_owned());
    }
}

/// A store that keeps every refresh token it was given.
#[derive(Debug, Clone, Default)]
struct Recording {
    inner: MemoryStore,
    saved: Arc<Mutex<Vec<String>>>,
}

impl SessionStore for Recording {
    fn load(&self) -> impl Future<Output = Result<Option<SecretString>, StoreError>> + Send {
        self.inner.load()
    }

    fn save(
        &self,
        refresh_token: &SecretString,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        self.saved
            .lock()
            .unwrap()
            .push(refresh_token.expose_secret().to_owned());
        self.inner.save(refresh_token)
    }

    fn clear(&self) -> impl Future<Output = Result<(), StoreError>> + Send {
        self.inner.clear()
    }
}

/// The Owner set up with 2FA, and the code its app shows now.
async fn owner_and_code(server: &Server) -> (Client<MemoryStore>, String) {
    let (owner, _, app) = server.owner().await;
    let code = server.code(&app).expose().to_owned();
    (owner, code)
}

/// The Owner's new offline account `name`'s bot.
async fn bot_of(owner: &Client<MemoryStore>, name: &str) -> BotId {
    let account = owner
        .create_offline_account(&CreateOfflineAccountRequest {
            name: SecretText::new(name.to_owned()),
        })
        .await
        .unwrap()
        .id;
    owner
        .bots(&PageQuery {
            before: None,
            limit: None,
        })
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|bot| bot.account.id == account)
        .unwrap()
        .id
}

#[tokio::test]
async fn a_console_logs_in_with_the_password_and_a_code_and_out_at_the_end_of_input() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(
        terminal.said,
        [
            format!("logged in as {OWNER}"),
            HELP.to_owned(),
            "logged out".to_owned()
        ]
    );
    server.stop().await;
}

#[tokio::test]
async fn a_user_without_2fa_is_pointed_to_the_app() {
    let server = Server::start().await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD], &[]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::NotLoggedIn);
    assert!(
        terminal.text().contains("`just dev-app`"),
        "{}",
        terminal.text()
    );
    server.stop().await;
}

#[tokio::test]
async fn a_wrong_password_is_refused_by_the_servers_answer_alone() {
    let server = Server::start().await;
    let (_owner, _code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&["a wrong password"], &[]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::NotLoggedIn);
    let text = terminal.text();
    assert!(text.contains("invalid_credentials"), "{text}");
    assert!(!text.contains("a wrong password"), "{text}");
    server.stop().await;
}

#[tokio::test]
async fn three_wrong_codes_end_the_login() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let wrong: Vec<String> = ["000000", "111111", "222222"]
        .into_iter()
        .filter(|wrong| *wrong != code)
        .map(str::to_owned)
        .collect();
    let (client, _) = server.client();
    let mut secrets = vec![PASSWORD];
    secrets.extend(wrong.iter().map(String::as_str));
    let mut terminal = Scripted::new(&secrets, &[]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::NotLoggedIn);
    let retries = terminal
        .said
        .iter()
        .filter(|line| line.contains("try the next one"))
        .count();
    assert_eq!(retries, wrong.len() - 1, "{}", terminal.text());
    server.stop().await;
}

#[tokio::test]
async fn offline_adds_an_account_whose_bot_bots_lists_unplaced_without_a_server() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &["offline AfkBot9", "bots"]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let listed: Vec<&String> = terminal
        .said
        .iter()
        .filter(|line| line.contains("AfkBot9"))
        .collect();
    assert_eq!(listed.len(), 2, "{}", terminal.text());
    // The account and the bot each named with their IDs, which look alike.
    assert!(
        listed[0].starts_with("account ") && listed[0].contains("); bot "),
        "{}",
        listed[0]
    );
    assert!(listed[1].starts_with("bot "), "{}", listed[1]);
    assert!(listed[1].contains("unplaced"), "{}", listed[1]);
    assert!(listed[1].contains("why=no_server"), "{}", listed[1]);
    server.stop().await;
}

#[tokio::test]
async fn server_asks_for_the_password_again_once_and_sets_the_address() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let command = format!("server {bot} 127.0.0.1:25565");
    let mut terminal = Scripted::new(&[PASSWORD, &code, PASSWORD], &[&command]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(terminal.secrets.len(), 0, "the step-up asked once");
    let stored = owner.bot(bot).await.unwrap().server_address;
    assert_eq!(
        stored.as_ref().map(fleet_api_types::BoundedText::as_str),
        Some("127.0.0.1:25565")
    );
    server.stop().await;
}

#[tokio::test]
async fn a_refused_step_up_changes_nothing() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let command = format!("server {bot} 127.0.0.1:25565");
    let mut terminal = Scripted::new(&[PASSWORD, &code, "a wrong password"], &[&command]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert!(terminal.text().contains("forbidden"), "{}", terminal.text());
    assert_eq!(owner.bot(bot).await.unwrap().server_address, None);
    server.stop().await;
}

#[tokio::test]
async fn an_address_the_server_refuses_shows_its_field() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let command = format!("server {bot} [::1");
    let mut terminal = Scripted::new(&[PASSWORD, &code, PASSWORD], &[&command]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(text.contains("validation_failed"), "{text}");
    assert!(text.contains("server_address"), "{text}");
    server.stop().await;
}

#[tokio::test]
async fn start_stop_and_bot_answer_with_the_bot() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let lines = [
        format!("start {bot}"),
        format!("stop {bot}"),
        format!("bot {bot}"),
    ];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &lines);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let started = &terminal.said[2];
    let stopped = &terminal.said[3];
    let json = &terminal.said[4];
    assert!(
        started.starts_with(&format!("bot {bot} ")) && started.contains("why=no_server"),
        "{started}"
    );
    assert!(
        stopped.starts_with(&format!("bot {bot} ")) && stopped.contains("unplaced"),
        "{stopped}"
    );
    assert_eq!(
        owner.bot(bot).await.unwrap().desired_state.known(),
        Some(fleet_api_types::bots::DesiredState::Stopped)
    );
    assert!(json.contains(&format!("\"id\": \"{bot}\"")), "{json}");
    server.stop().await;
}

#[tokio::test]
async fn input_that_isnt_a_command_is_never_echoed() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(
        &[PASSWORD, &code],
        &["sudo secret-input-7f", "start not-a-bot"],
    );

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert_eq!(text.matches("not a command").count(), 2, "{text}");
    assert!(
        !text.contains("secret-input-7f") && !text.contains("not-a-bot"),
        "{text}"
    );
    server.stop().await;
}

/// A line for a bot that runs nowhere: the server's answer is shown, and
/// the line itself never is.
#[tokio::test]
async fn chat_sends_the_rest_of_the_line_and_shows_the_answer_never_the_line() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let command = format!("chat {bot} hello there secret_label");
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[&command]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(text.contains("bot_not_online"), "{text}");
    assert!(!text.contains("secret_label"), "{text}");
    server.stop().await;
}

/// `history` shows a page oldest first and where the older lines start;
/// a bot without chat says so.
#[tokio::test]
async fn history_shows_a_page_oldest_first_and_where_older_lines_start() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let quiet = bot_of(&owner, "AfkBot8").await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let texts: Vec<String> = (1..=52).map(|n| format!("line {n}")).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    server.said(bot, &texts).await;
    let (client, _) = server.client();
    let newest = format!("history {bot}");
    let empty = format!("history {quiet}");
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[&newest, &empty]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let said = &terminal.said;
    let lines: Vec<&String> = said.iter().filter(|line| line.starts_with('#')).collect();
    assert_eq!(lines.len(), 50, "{said:?}");
    assert!(lines[0].ends_with("system: line 3"), "{said:?}");
    assert!(lines[49].ends_with("system: line 52"), "{said:?}");
    let older = said
        .iter()
        .find(|line| line.starts_with("older: history "))
        .unwrap();
    assert!(
        older.starts_with(&format!("older: history {bot} ")),
        "{older}"
    );
    assert!(said.iter().any(|line| line == "no chat yet"), "{said:?}");
    server.stop().await;
}

/// `chat <id>` with only spaces after it is invalid input: nothing is sent,
/// so the server never answers that the line is empty.
#[tokio::test]
async fn chat_without_text_is_invalid_input_and_never_sent() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let chat = format!("chat {bot}   ");
    let command = format!("command {bot} ");
    // A third secret, which a step-up would take.
    let mut terminal = Scripted::new(&[PASSWORD, &code, PASSWORD], &[&chat, &command]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert_eq!(text.matches("not a command").count(), 2, "{text}");
    assert!(!text.contains("must not be empty"), "{text}");
    assert!(!text.contains("step_up_required"), "{text}");
    assert_eq!(terminal.secrets.len(), 1, "no step-up was asked for");
    server.stop().await;
}

#[tokio::test]
async fn chat_with_a_command_off_the_allowlist_shows_the_pointer_to_command() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let command = format!("chat {bot} /op AfkBot2");
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[&command]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(text.contains("command_needs_step_up"), "{text}");
    assert!(!text.contains("AfkBot2"), "{text}");
    server.stop().await;
}

#[tokio::test]
async fn command_asks_for_the_password_again_once_and_sends() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let command = format!("command {bot} /op AfkBot2");
    let mut terminal = Scripted::new(&[PASSWORD, &code, PASSWORD], &[&command]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(terminal.secrets.len(), 0, "the step-up asked once");
    let text = terminal.text();
    assert!(text.contains("bot_not_online"), "past the step-up: {text}");
    assert!(!text.contains("step_up_required"), "{text}");
    assert!(
        !text.contains(PASSWORD) && !text.contains("AfkBot2"),
        "{text}"
    );
    server.stop().await;
}

#[tokio::test]
async fn quit_logs_out_and_nothing_secret_was_shown_on_the_way() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let store = Recording::default();
    let client = server.client_over(store.clone());
    let command = format!("server {bot} 127.0.0.1:25565");
    let mut terminal = Scripted::new(
        &[PASSWORD, &code, PASSWORD],
        &[&command, "bots", "quit", "bots"],
    );

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(terminal.lines.len(), 1, "nothing read after quit");
    assert_eq!(
        crate::support::stored(&store.inner).await,
        None,
        "the session is gone"
    );
    let text = terminal.text();
    assert!(!text.contains(PASSWORD) && !text.contains(&code), "{text}");
    // Access and refresh tokens alike: neither kind's prefix, nor any refresh
    // token the session saved.
    assert!(
        !text.contains("afk_at_") && !text.contains("afk_rt_"),
        "{text}"
    );
    let saved = store.saved.lock().unwrap().clone();
    assert_ne!(saved, Vec::<String>::new(), "the session saved its token");
    for token in saved {
        assert!(!text.contains(&token), "{text}");
    }
    server.stop().await;
}

#[tokio::test]
async fn a_server_that_cant_be_reached_is_said_so() {
    let client = Client::new(
        ClientConfig {
            server: ServerUrl::parse("http://127.0.0.1:1").unwrap(),
            trust: Trust::System,
        },
        MemoryStore::new(),
    )
    .unwrap();
    let mut terminal = Scripted::new(&[PASSWORD], &[]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::NotLoggedIn);
    assert!(
        terminal.text().contains("can't be reached"),
        "{}",
        terminal.text()
    );
}

#[tokio::test]
async fn no_terminal_to_ask_ends_the_login() {
    let server = Server::start().await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[], &[]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::NotLoggedIn);
    assert!(
        terminal.text().contains("can't be read"),
        "{}",
        terminal.text()
    );
    server.stop().await;
}

#[test]
fn the_arguments_are_a_username_then_maybe_the_servers_url() {
    let args =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };

    assert_eq!(
        dev_console::arguments(&args(&["alice"])),
        Some(("alice", DEV_SERVER))
    );
    assert_eq!(
        dev_console::arguments(&args(&["alice", "https://fleet.example.com"])),
        Some(("alice", "https://fleet.example.com"))
    );
    assert_eq!(dev_console::arguments(&args(&[])), None);
    assert_eq!(
        dev_console::arguments(&args(&["alice", "https://fleet.example.com", "more"])),
        None
    );
}

#[tokio::test]
async fn bots_lists_every_page_of_the_callers_bots() {
    let server = Server::start().await;
    let (owner, _, app) = server.owner().await;
    // One more than a page.
    for n in 0..51 {
        if n % 20 == 19 {
            // The per-IP bucket refills on the server's clock.
            server.advance(Duration::from_secs(2));
        }
        owner
            .create_offline_account(&CreateOfflineAccountRequest {
                name: SecretText::new(format!("AfkBot{n}")),
            })
            .await
            .unwrap();
    }
    server.next_step();
    let code = server.code(&app).expose().to_owned();
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &["bots"]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    for n in 0..51 {
        let name = format!(" AfkBot{n} ");
        assert!(
            terminal
                .said
                .iter()
                .any(|line| line.starts_with("bot ") && line.contains(&name)),
            "{name}: {}",
            terminal.text()
        );
    }
    server.stop().await;
}

#[tokio::test]
async fn a_session_lost_under_the_console_ends_it_with_a_word_and_reads_no_more() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[A_MONTH, "bots", "bots"])
        .with_clock(server.clock.clone());

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::SessionLost);
    assert_eq!(
        terminal.said.last().map(String::as_str),
        Some("the session ended; run the console again to log in"),
        "{}",
        terminal.text()
    );
    assert_eq!(terminal.lines.len(), 1, "nothing read after");
    server.stop().await;
}
