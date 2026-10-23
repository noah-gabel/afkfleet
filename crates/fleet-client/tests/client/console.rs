//! The dev console (`examples/dev-bots`; ADR-0019, S71) against the
//! in-process server, its terminal scripted: the login, its refusals, the
//! commands, the step-up, and that nothing secret or unknown is ever shown.

use core::pin::Pin;
use core::time::Duration;
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::{BotId, PageQuery, SecretText};
use fleet_client::{Client, ClientConfig, MemoryStore, ServerUrl, SessionStore, StoreError, Trust};
use fleet_testkit::system::ManualClock;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::watch;

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
    /// What was said, for a test that waits on it while the console runs.
    heard: watch::Sender<Vec<String>>,
    /// Each Enter `events` waits for comes once a line holding this was said.
    enters: VecDeque<String>,
    /// The server's clock, for [`A_MONTH`].
    clock: Option<ManualClock>,
}

impl Scripted {
    fn new(secrets: &[&str], lines: &[&str]) -> Self {
        Self {
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            secrets: secrets.iter().map(|secret| (*secret).to_owned()).collect(),
            said: Vec::new(),
            heard: watch::Sender::default(),
            enters: VecDeque::new(),
            clock: None,
        }
    }

    /// The script, with Enter pressed for each `events` once a line holding
    /// its text was said.
    fn with_enters(mut self, after: &[&str]) -> Self {
        self.enters = after.iter().map(|text| (*text).to_owned()).collect();
        self
    }

    /// What the console said and will say, as it goes.
    fn heard(&self) -> watch::Receiver<Vec<String>> {
        self.heard.subscribe()
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
        self.heard.send_modify(|said| said.push(text.to_owned()));
    }

    fn enter(&mut self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        let after = self.enters.pop_front();
        let mut heard = self.heard.subscribe();
        Box::pin(async move {
            let after = after.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Enter"))?;
            heard
                .wait_for(|said| said.iter().any(|line| line.contains(&after)))
                .await
                .map(drop)
                .map_err(|_| io::Error::other("the terminal is gone"))
        })
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

/// The farm preset's fixed ID.
const FARM: &str = "01a14202-2800-7000-8000-000000000002";

#[tokio::test]
async fn modes_lists_every_mode_one_line_each() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &["modes"]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(
        text.contains(&format!("mode {FARM} farm built-in")),
        "{text}"
    );
    assert!(
        text.contains("mode 01a14202-2800-7000-8000-000000000001 afk built-in"),
        "{text}"
    );
    server.stop().await;
}

/// `new-mode` creates the mode its JSON holds and shows its line, never its
/// steps' text.
#[tokio::test]
async fn new_mode_creates_the_mode_and_never_shows_its_steps() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let line = r#"new-mode {"name":"Greeter","visibility":"private","steps":[{"action":{"type":"send_chat","message":"hello secret-text"},"schedule":{"type":"every","interval_ms":60000,"jitter_ms":0},"probability":100}]}"#;
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[line]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(text.contains(" Greeter private mine editable"), "{text}");
    assert!(!text.contains("secret-text"), "{text}");
    let names: Vec<String> = owner
        .modes(&PageQuery {
            before: None,
            limit: None,
        })
        .await
        .unwrap()
        .items
        .iter()
        .map(|mode| mode.name.as_str().to_owned())
        .collect();
    assert_eq!(names, ["Greeter", "farm", "afk"]);
    server.stop().await;
}

/// `new-mode` takes on-respawn steps: the hands-on line of group C3's PR,
/// as the user types it (ADR-0020, S20).
#[tokio::test]
async fn new_mode_takes_on_respawn_steps() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let line = r#"new-mode {"name":"Back again","visibility":"private","steps":[{"action":{"type":"select_hotbar_slot","slot":2},"schedule":{"type":"at_start"},"probability":100},{"action":{"type":"select_hotbar_slot","slot":4},"schedule":{"type":"on_respawn"},"probability":100},{"action":{"type":"send_chat","message":"back again"},"schedule":{"type":"on_respawn"},"probability":100}]}"#;
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[line]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(
        text.contains(" Back again private mine editable version 1"),
        "{text}"
    );
    let created = owner
        .modes(&PageQuery {
            before: None,
            limit: None,
        })
        .await
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let schedules: Vec<&str> = created
        .steps
        .iter()
        .map(|step| step.schedule.kind.as_str())
        .collect();
    assert_eq!(schedules, ["at_start", "on_respawn", "on_respawn"]);
    server.stop().await;
}

/// A mode with a command off the allowlist asks for the password again,
/// once, and is created through the step-up route.
#[tokio::test]
async fn new_mode_with_a_command_off_the_allowlist_asks_for_the_password_once() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let line = r#"new-mode {"name":"Ops","visibility":"private","steps":[{"action":{"type":"send_chat","message":"/op AfkBot2"},"schedule":{"type":"at_start"},"probability":100}]}"#;
    let mut terminal = Scripted::new(&[PASSWORD, &code, PASSWORD], &[line]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(terminal.secrets.len(), 0, "the step-up asked once");
    let text = terminal.text();
    assert!(text.contains(" Ops private mine editable"), "{text}");
    assert!(!text.contains("mode_needs_step_up"), "{text}");
    assert!(
        !text.contains("AfkBot2") && !text.contains(PASSWORD),
        "{text}"
    );
    server.stop().await;
}

/// JSON that isn't a mode is said so and never repeated, and nothing is
/// sent; a mode the server refuses shows the field it names.
#[tokio::test]
async fn new_mode_that_isnt_a_mode_is_never_echoed_and_a_refused_one_shows_its_field() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let not_json = r#"new-mode {"name":"x","steps":[{"action":{"type":"say secret-text"}}]}"#;
    let refused = r#"new-mode {"name":"AFK","visibility":"private","steps":[]}"#;
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[not_json, refused]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(text.contains("not a mode's JSON"), "{text}");
    assert!(!text.contains("secret-text"), "{text}");
    assert!(text.contains("name: is a built-in mode's name"), "{text}");
    server.stop().await;
}

#[tokio::test]
async fn mode_switches_a_bots_mode() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let bot = bot_of(&owner, "AfkBot9").await;
    let (client, _) = server.client();
    let line = format!("mode {bot} {FARM}");
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[&line]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(owner.bot(bot).await.unwrap().mode_id.to_string(), FARM);
    assert!(
        terminal.text().contains(&format!("bot {bot}")),
        "{}",
        terminal.text()
    );
    server.stop().await;
}

/// The ID of the newest mode `client` sees.
async fn newest_mode(client: &fleet_client::Client<impl fleet_client::SessionStore>) -> String {
    client
        .modes(&PageQuery {
            before: None,
            limit: None,
        })
        .await
        .unwrap()
        .items
        .first()
        .unwrap()
        .id
        .to_string()
}

/// A greeting mode of the Owner's, created through `owner`.
async fn greeter(owner: &fleet_client::Client<impl fleet_client::SessionStore>) -> String {
    let request = serde_json::from_value(
        serde_json::json!({ "name": "Greeter", "visibility": "private", "steps": [] }),
    )
    .unwrap();
    owner.create_mode(&request).await.unwrap();
    newest_mode(owner).await
}

/// `edit-mode` edits the mode its JSON holds and shows its line at the next
/// version, never its steps' text; the same line again changes nothing,
/// and a stale one shows `mode_changed`'s text.
#[tokio::test]
async fn edit_mode_edits_the_mode_and_never_shows_its_steps() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let id = greeter(&owner).await;
    let (client, _) = server.client();
    let edit = |version: u32, message: &str| {
        format!(
            r#"edit-mode {id} {{"version":{version},"name":"Greeter","visibility":"private","steps":[{{"action":{{"type":"send_chat","message":"{message}"}},"schedule":{{"type":"every","interval_ms":60000,"jitter_ms":0}},"probability":100}}]}}"#
        )
    };
    let lines = [
        edit(1, "hello secret-text"),
        edit(1, "hello secret-text"),
        edit(1, "bye secret-text"),
    ];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &lines);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert_eq!(
        text.matches(&format!(
            "mode {id} Greeter private mine editable version 2"
        ))
        .count(),
        2,
        "{text}"
    );
    assert!(
        text.contains("This mode changed since you opened it. Load it again to see the change."),
        "{text}"
    );
    assert!(!text.contains("secret-text"), "{text}");
    server.stop().await;
}

/// An edit with a command off the allowlist asks for the password again,
/// once, and goes through the step-up twin.
#[tokio::test]
async fn edit_mode_with_a_command_off_the_allowlist_asks_for_the_password_once() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let id = greeter(&owner).await;
    let (client, _) = server.client();
    let line = format!(
        r#"edit-mode {id} {{"version":1,"name":"Greeter","visibility":"private","steps":[{{"action":{{"type":"send_chat","message":"/op AfkBot2"}},"schedule":{{"type":"at_start"}},"probability":100}}]}}"#
    );
    let mut terminal = Scripted::new(&[PASSWORD, &code, PASSWORD], &[&line]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(terminal.secrets.len(), 0, "the step-up asked once");
    let text = terminal.text();
    assert!(
        text.contains(" Greeter private mine editable version 2"),
        "{text}"
    );
    assert!(
        !text.contains("AfkBot2") && !text.contains(PASSWORD),
        "{text}"
    );
    server.stop().await;
}

/// JSON that isn't an edit is said so and never repeated, and nothing is
/// sent.
#[tokio::test]
async fn edit_mode_that_isnt_an_edit_is_never_echoed() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let id = greeter(&owner).await;
    let (client, _) = server.client();
    let line = format!(
        r#"edit-mode {id} {{"name":"Greeter","visibility":"private","steps":[{{"say":"secret-text"}}]}}"#
    );
    let mut terminal = Scripted::new(&[PASSWORD, &code], &[&line]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    assert!(text.contains("not a mode's JSON"), "{text}");
    assert!(!text.contains("secret-text"), "{text}");
    assert_eq!(newest_mode(&owner).await, id);
    server.stop().await;
}

/// `delete-mode` asks for the password again and deletes the mode; one a
/// bot uses shows `mode_in_use`'s text and stays.
#[tokio::test]
async fn delete_mode_asks_for_the_password_and_deletes_the_mode() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let id = greeter(&owner).await;
    let used = {
        let request = serde_json::from_value(
            serde_json::json!({ "name": "Used", "visibility": "private", "steps": [] }),
        )
        .unwrap();
        owner.create_mode(&request).await.unwrap().id
    };
    let bot = bot_of(&owner, "AfkBot9").await;
    owner
        .set_bot_mode(
            bot,
            &fleet_api_types::bots::BotModeRequest { mode_id: used },
        )
        .await
        .unwrap();
    let (client, _) = server.client();
    let delete = format!("delete-mode {id}");
    let in_use = format!("delete-mode {used}");
    let mut terminal = Scripted::new(&[PASSWORD, &code, PASSWORD], &[&delete, &in_use]);

    let ended = dev_console::run(&client, OWNER, &mut terminal).await;

    assert_eq!(ended, Ended::LoggedOut);
    assert_eq!(terminal.secrets.len(), 0, "the step-up asked once");
    let text = terminal.text();
    assert!(text.contains(&format!("mode {id} deleted")), "{text}");
    assert!(
        text.contains(
            "A bot uses this mode, so it can't be deleted. Switch every bot to another mode first."
        ),
        "{text}"
    );
    assert!(!text.contains(PASSWORD), "{text}");
    assert_eq!(newest_mode(&owner).await, used.to_string());
    server.stop().await;
}

// --- `events` (P11.5; ADR-0020, S37) ---------------------------------------------

/// The longest an `events` test waits for the console or the server.
const EVENTS_PATIENCE: Duration = Duration::from_secs(30);

#[tokio::test]
async fn events_shows_ready_and_stops_at_enter_without_a_ticket_or_token() {
    let server = Server::start().await;
    let (_owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &["events"]).with_enters(&["ready"]);

    let ended = tokio::time::timeout(
        EVENTS_PATIENCE,
        dev_console::run(&client, OWNER, &mut terminal),
    )
    .await
    .expect("the console ends in time");

    assert_eq!(ended, Ended::LoggedOut);
    let text = terminal.text();
    for line in ["events: press Enter to stop", "ready", "events stopped"] {
        assert!(
            terminal.said.iter().any(|said| said == line),
            "{line}: {text}"
        );
    }
    for secret in ["afk_wt_", "ticket=", "afk_at_", "Bearer"] {
        assert!(!text.contains(secret), "{secret}: {text}");
    }
    server.stop().await;
}

#[tokio::test]
async fn ending_the_consoles_session_closes_its_events_with_4001_and_it_waits_for_enter() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &["events"])
        .with_enters(&["the server closed the events socket"]);
    let mut heard = terminal.heard();

    let both = async {
        tokio::join!(dev_console::run(&client, OWNER, &mut terminal), async {
            heard
                .wait_for(|said| said.iter().any(|line| line == "ready"))
                .await
                .unwrap();
            let sessions = owner
                .sessions(&PageQuery {
                    before: None,
                    limit: None,
                })
                .await
                .unwrap();
            let consoles = sessions
                .items
                .iter()
                .find(|session| !session.current)
                .unwrap();
            owner.revoke_session(consoles.id).await.unwrap();
        })
    };
    let (ended, ()) = tokio::time::timeout(EVENTS_PATIENCE, both)
        .await
        .expect("the console ends in time");

    assert_eq!(ended, Ended::LoggedOut);
    assert!(
        terminal
            .said
            .iter()
            .any(|line| line
                == "the server closed the events socket: 4001 Authentication is required."),
        "{}",
        terminal.text()
    );
    assert!(terminal.said.iter().any(|line| line == "press Enter"));
    server.stop().await;
}

#[tokio::test]
async fn an_https_server_gets_no_events_and_no_ticket_is_asked_for() {
    let client = Client::new(
        ClientConfig {
            server: ServerUrl::parse("https://127.0.0.1:1").unwrap(),
            trust: Trust::System,
        },
        MemoryStore::new(),
    )
    .unwrap();
    let mut terminal = Scripted::new(&[], &[]);

    let done = dev_console::events(&client, &mut terminal).await;

    assert!(done.is_ok());
    assert_eq!(
        terminal.said,
        ["events need dev-server's plain http until the app's own client (P11.8)"]
    );
}

#[test]
fn a_binary_frame_reads_as_a_resync_and_a_ping_as_nothing() {
    use tokio_tungstenite::tungstenite::{Bytes, Message};

    let binary = dev_console::frame_line(&Message::Binary(Bytes::from_static(b"\x00")));
    let text = dev_console::frame_line(&Message::Text(r#"{"type":"ready"}"#.into()));
    let ping = dev_console::frame_line(&Message::Ping(Bytes::new()));

    // The protocol's second rule: a frame that doesn't read is a resync.
    assert_eq!(
        binary.as_deref(),
        Some("an event that doesn't read: taken for a resync")
    );
    assert_eq!(text.as_deref(), Some("ready"));
    assert_eq!(ping, None);
}

#[tokio::test]
async fn a_refused_events_socket_reads_as_its_status_and_code() {
    let server = Server::start().await;
    let (owner, code) = owner_and_code(&server).await;
    // The owner's three places, taken by another session of theirs.
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(held_socket(&owner).await);
    }
    let (client, _) = server.client();
    let mut terminal = Scripted::new(&[PASSWORD, &code], &["events"]);

    tokio::time::timeout(
        EVENTS_PATIENCE,
        dev_console::run(&client, OWNER, &mut terminal),
    )
    .await
    .expect("the console ends in time");

    assert!(
        terminal
            .said
            .iter()
            .any(|line| line == "the events socket was refused: 429 too_many_connections"),
        "{}",
        terminal.text()
    );
    drop(held);
    server.stop().await;
}

/// A live events socket of `client`'s session, held open.
async fn held_socket<S: SessionStore>(
    client: &Client<S>,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    let handshake = client.events_handshake().await.unwrap();
    let mut request = handshake
        .url()
        .expose_secret()
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(handshake.authorization().expose_secret()).unwrap(),
    );
    let (socket, _) =
        tokio::time::timeout(EVENTS_PATIENCE, tokio_tungstenite::connect_async(request))
            .await
            .expect("the socket opens in time")
            .unwrap();
    socket
}

#[test]
fn each_event_and_close_reads_as_a_line() {
    use fleet_api_types::events::{EventMessage, Received};

    assert_eq!(
        dev_console::event_line(&Received::Event(Box::new(EventMessage::ready()))),
        "ready"
    );
    assert_eq!(
        dev_console::event_line(&Received::Event(Box::new(EventMessage::resync()))),
        "resync: fetch again"
    );
    assert_eq!(
        dev_console::event_line(&Received::Ignored),
        "an event this console doesn't know"
    );
    assert_eq!(
        dev_console::event_line(&Received::Unreadable),
        "an event that doesn't read: taken for a resync"
    );
    assert_eq!(
        dev_console::closed_line(Some(1001)),
        "the server closed the events socket: 1001 The server is shutting down."
    );
    assert_eq!(
        dev_console::closed_line(Some(1000)),
        "the server closed the events socket: 1000"
    );
    assert_eq!(
        dev_console::closed_line(None),
        "the server closed the events socket"
    );
}

const EVENT_BOT: &str = "0190a5b0-0000-7000-8000-000000000001";
const EVENT_MODE: &str = "0190a5b0-0000-7000-8000-000000000002";
const EVENT_REQUEST: &str = "0190a5b0-0000-7000-8000-000000000003";
const EVENT_AT: &str = "2027-01-15T08:00:00.000Z";

/// The console's line for the frame `value`, read as the console reads it.
fn event_line_of(value: &serde_json::Value) -> String {
    dev_console::event_line(&fleet_client::events::read(&value.to_string()))
}

/// A mode as the API answers it, with a chat step holding `message`.
fn a_mode_frame(message: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "mode",
        "mode_id": EVENT_MODE,
        "mode": {
            "id": EVENT_MODE,
            "name": "home",
            "visibility": "shared",
            "ownership": "mine",
            "editable": true,
            "deletable": true,
            "visibility_editable": true,
            "version": 3,
            "created_at": EVENT_AT,
            "updated_at": EVENT_AT,
            "steps": [{
                "action": { "type": "chat", "message": message, "arguments_hidden": false },
                "schedule": { "type": "at_start" },
                "probability": 100
            }]
        }
    })
}

#[test]
fn each_event_kind_reads_as_its_console_line() {
    use serde_json::json;

    let bot = json!({
        "type": "bot",
        "bot_id": EVENT_BOT,
        "bot": {
            "id": EVENT_BOT,
            "account": {
                "id": "0190a5b0-0000-7000-8000-000000000004",
                "name": "AfkBot1",
                "kind": "offline",
                "status": "active",
                "level": "view"
            },
            "mode_id": EVENT_MODE,
            "desired_state": "running",
            "status": { "kind": "reported", "state": "online", "since": EVENT_AT }
        }
    });
    let line = json!({
        "id": 41, "at": EVENT_AT, "direction": "out", "kind": "user", "text": "/login",
        "truncated": false, "arguments_hidden": true, "mine": true
    });
    let cases = [
        (bot, format!("bot {EVENT_BOT} AfkBot1 reported/online")),
        (
            json!({ "type": "bot_removed", "bot_id": EVENT_BOT }),
            format!("bot {EVENT_BOT} removed"),
        ),
        (
            json!({ "type": "bot_died", "bot_id": EVENT_BOT, "at": EVENT_AT }),
            format!("bot {EVENT_BOT} died at {EVENT_AT}"),
        ),
        (
            json!({
                "type": "bot_alert", "bot_id": EVENT_BOT, "at": EVENT_AT,
                "state": "paused", "reason": "duplicate_login"
            }),
            format!("bot {EVENT_BOT} paused reason=duplicate_login at {EVENT_AT}"),
        ),
        (
            json!({
                "type": "chat", "bot_id": EVENT_BOT, "line": line, "request": EVENT_REQUEST
            }),
            format!(
                "bot {EVENT_BOT} #41 {EVENT_AT} user by you: /login (arguments hidden) (request {EVENT_REQUEST})"
            ),
        ),
        (
            json!({ "type": "chat", "bot_id": EVENT_BOT, "line": {
                "id": 40, "at": EVENT_AT, "direction": "in", "kind": "system", "text": "Welcome",
                "truncated": false, "arguments_hidden": false, "mine": false
            } }),
            format!("bot {EVENT_BOT} #40 {EVENT_AT} system: Welcome"),
        ),
        (
            json!({ "type": "chat_sent", "bot_id": EVENT_BOT, "request": EVENT_REQUEST }),
            format!("bot {EVENT_BOT} request {EVENT_REQUEST} sent"),
        ),
        (
            json!({
                "type": "chat_failed", "bot_id": EVENT_BOT, "request": EVENT_REQUEST,
                "failure": "chat_unavailable"
            }),
            format!("bot {EVENT_BOT} request {EVENT_REQUEST} failed: chat_unavailable"),
        ),
        (
            json!({ "type": "chat_failed", "bot_id": EVENT_BOT, "request": EVENT_REQUEST }),
            format!("bot {EVENT_BOT} request {EVENT_REQUEST} failed"),
        ),
        (
            a_mode_frame("hello"),
            format!("mode {EVENT_MODE} home shared mine editable version 3"),
        ),
        (
            json!({ "type": "mode_removed", "mode_id": EVENT_MODE }),
            format!("mode {EVENT_MODE} removed"),
        ),
    ];

    for (frame, expected) in cases {
        assert_eq!(event_line_of(&frame), expected, "{frame}");
    }
}

#[test]
fn a_mode_event_never_prints_its_steps() {
    let shown = event_line_of(&a_mode_frame("/warp canary-secret"));

    assert!(shown.starts_with("mode "), "{shown}");
    assert!(!shown.contains("canary"), "{shown}");
    assert!(!shown.contains("warp"), "{shown}");
}
